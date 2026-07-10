use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Map, Value};

use crate::models::{
    BundleDns, BundleGroup, BundleGroupMember, BundleProxy, ConfigBundle, DnsInput, DnsMapping,
    ImportSummary, ProxyGroup, ProxyGroupDomain, ProxyGroupInput, ProxyGroupMember, ProxyInput,
    ProxyRecord, TrafficLog, CONFIG_BUNDLE_KIND,
};

const TRAFFIC_LOG_VISIBLE_AFTER_ID_KEY: &str = "traffic_log_visible_after_id";

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

pub struct ProxyGroupSelection {
    pub group_name: String,
    pub proxy_ids: HashSet<i64>,
}

pub struct RequestLogEntry<'a> {
    pub proxy_id: Option<i64>,
    pub target_host: &'a str,
    pub target_port: i64,
    pub success: bool,
    pub response_time: Option<i64>,
    pub error_message: Option<&'a str>,
    pub result_type: &'a str,
}

impl Database {
    pub fn open() -> Result<Self> {
        let data_dir = env::var("DATA_DIR")
            .map(PathBuf::from)
            .map(Ok)
            .unwrap_or_else(|_| default_data_dir())?;
        fs::create_dir_all(&data_dir)
            .with_context(|| format!("创建数据目录失败: {}", data_dir.display()))?;
        import_initial_database(&data_dir)?;

        let db_path = data_dir.join("proxy.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("打开数据库失败: {}", db_path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> PathBuf {
        self.db_path.clone()
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| anyhow!("数据库连接锁已损坏"))
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.connection()?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS proxies (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              name TEXT NOT NULL,
              type TEXT NOT NULL,
              host TEXT NOT NULL,
              port INTEGER NOT NULL,
              username TEXT,
              password TEXT,
              status TEXT DEFAULT 'unknown',
              last_test DATETIME,
              response_time INTEGER,
              success_count INTEGER DEFAULT 0,
              fail_count INTEGER DEFAULT 0,
              priority INTEGER DEFAULT 999,
              enabled INTEGER DEFAULT 1,
              skip_cert_verify INTEGER DEFAULT 0,
              created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS settings (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS request_logs (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              proxy_id INTEGER,
              target_host TEXT,
              target_port INTEGER,
              success BOOLEAN,
              response_time INTEGER,
              error_message TEXT,
              result_type TEXT,
              created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS dns_mappings (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              domain TEXT NOT NULL UNIQUE,
              ip TEXT NOT NULL,
              description TEXT,
              enabled INTEGER DEFAULT 1,
              created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
              updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS proxy_groups (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              name TEXT NOT NULL,
              is_default INTEGER DEFAULT 0,
              enabled INTEGER DEFAULT 1,
              created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
              updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS proxy_group_domains (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              group_id INTEGER NOT NULL,
              domain TEXT NOT NULL,
              FOREIGN KEY (group_id) REFERENCES proxy_groups(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS proxy_group_members (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              group_id INTEGER NOT NULL,
              proxy_id INTEGER NOT NULL,
              FOREIGN KEY (group_id) REFERENCES proxy_groups(id) ON DELETE CASCADE,
              FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE,
              UNIQUE(group_id, proxy_id)
            );

            CREATE INDEX IF NOT EXISTS idx_logs_created_at ON request_logs (created_at);
            CREATE INDEX IF NOT EXISTS idx_logs_proxy_id ON request_logs (proxy_id);
            CREATE INDEX IF NOT EXISTS idx_logs_target ON request_logs (target_host, created_at);
            CREATE INDEX IF NOT EXISTS idx_dns_domain ON dns_mappings (domain);
            CREATE INDEX IF NOT EXISTS idx_dns_ip ON dns_mappings (ip);
            CREATE INDEX IF NOT EXISTS idx_group_domains_group ON proxy_group_domains (group_id);
            CREATE INDEX IF NOT EXISTS idx_group_domains_domain ON proxy_group_domains (domain);
            CREATE INDEX IF NOT EXISTS idx_group_members_group ON proxy_group_members (group_id);
            CREATE INDEX IF NOT EXISTS idx_group_members_proxy ON proxy_group_members (proxy_id);
            "#,
        )?;

        add_column_if_missing(&conn, "proxies", "test_url", "TEXT DEFAULT NULL")?;
        add_column_if_missing(&conn, "proxies", "test_timeout", "INTEGER DEFAULT NULL")?;
        add_column_if_missing(&conn, "proxies", "skip_cert_verify", "INTEGER DEFAULT 0")?;
        add_column_if_missing(&conn, "request_logs", "result_type", "TEXT")?;
        add_column_if_missing(&conn, "dns_mappings", "dynamic", "INTEGER DEFAULT 0")?;
        add_column_if_missing(
            &conn,
            "dns_mappings",
            "last_resolved",
            "DATETIME DEFAULT NULL",
        )?;
        // 历史版本把 HTTPS 目标误建模成独立的上游代理传输类型；运行时实际一直使用
        // HTTP CONNECT，因此统一为 HTTP，避免测活与真实流量采用不同协议。
        conn.execute("UPDATE proxies SET type = 'http' WHERE type = 'https'", [])?;
        // SOCKS4 没有密码字段；清理历史版本曾保存但运行时无法发送的无效值。
        conn.execute(
            "UPDATE proxies SET password = NULL WHERE type = 'socks4' AND password IS NOT NULL",
            [],
        )?;

        let test_url: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'test_url'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if test_url.is_none() {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('test_url', 'https://cms.zjzwfw.gov.cn/favicon.ico')",
                [],
            )?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('timeout', '10')",
                [],
            )?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('algorithm', 'adaptive')",
                [],
            )?;
        }
        conn.execute(
            r#"
            DELETE FROM settings
            WHERE key IN (
              'load_mode', 'stats_retention_days', 'pool_max_size', 'pool_idle_timeout',
              'pool_wait_timeout', 'circuit_half_open_attempts', 'health_check_interval',
              'health_degrade_threshold', 'health_recover_threshold', 'algorithm_weights'
            )
            "#,
            [],
        )?;
        conn.execute(
            "UPDATE settings SET value = 'round_robin' WHERE key = 'algorithm' AND value = 'weighted_round_robin'",
            [],
        )?;
        let legacy_log_cursor: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?",
                params![TRAFFIC_LOG_VISIBLE_AFTER_ID_KEY],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(raw_cursor) = legacy_log_cursor {
            let cursor = raw_cursor
                .parse::<i64>()
                .with_context(|| format!("历史流量日志清除游标无效: {raw_cursor:?}"))?;
            conn.execute(
                r#"
                DELETE FROM request_logs
                WHERE id <= ?
                  AND COALESCE(result_type, '') NOT IN ('health_success', 'health_failure')
                "#,
                params![cursor],
            )?;
            conn.execute(
                "DELETE FROM settings WHERE key = ?",
                params![TRAFFIC_LOG_VISIBLE_AFTER_ID_KEY],
            )?;
        }

        Ok(())
    }

    pub fn list_proxies(&self) -> Result<Vec<ProxyRecord>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT p.*
            FROM proxies p
            ORDER BY priority ASC, id ASC
            "#,
        )?;
        let rows = stmt.query_map([], proxy_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn list_enabled_proxies(&self) -> Result<Vec<ProxyRecord>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT p.*
            FROM proxies p
            WHERE enabled = 1
            ORDER BY priority ASC, id ASC
            "#,
        )?;
        let rows = stmt.query_map([], proxy_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_proxy(&self, id: i64) -> Result<Option<ProxyRecord>> {
        let conn = self.connection()?;
        conn.query_row(
            r#"
            SELECT p.*
            FROM proxies p
            WHERE p.id = ?
            "#,
            params![id],
            proxy_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_proxy(&self, input: ProxyInput) -> Result<ProxyRecord> {
        let input = normalize_proxy_input(input)?;
        let conn = self.connection()?;
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM proxies WHERE host = ? AND port = ? AND type = ?",
                params![input.host, input.port, input.proxy_type],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Err(anyhow!(
                "该代理已存在（{}://{}:{} 已被使用）",
                input.proxy_type,
                input.host,
                input.port
            ));
        }

        conn.execute(
            r#"
            INSERT INTO proxies
              (name, type, host, port, username, password, enabled, test_url, test_timeout, skip_cert_verify)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                input.name,
                input.proxy_type,
                input.host,
                input.port,
                input.username,
                input.password,
                input.enabled.unwrap_or(1),
                empty_to_none(input.test_url),
                input.test_timeout,
                flag_from_value(input.skip_cert_verify.as_ref())
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.get_proxy(id)?
            .ok_or_else(|| anyhow!("代理创建后无法读取"))
    }

    pub fn update_proxy(&self, id: i64, input: ProxyInput) -> Result<(ProxyRecord, bool)> {
        let input = normalize_proxy_input(input)?;
        let existing = self.get_proxy(id)?.ok_or_else(|| anyhow!("代理不存在"))?;
        let reset_status = proxy_connection_config_changed(&existing, &input);
        let conn = self.connection()?;
        let duplicate: Option<i64> = conn
            .query_row(
                "SELECT id FROM proxies WHERE host = ? AND port = ? AND type = ? AND id <> ?",
                params![input.host, input.port, input.proxy_type, id],
                |row| row.get(0),
            )
            .optional()?;
        if duplicate.is_some() {
            return Err(anyhow!(
                "该代理已存在（{}://{}:{} 已被使用）",
                input.proxy_type,
                input.host,
                input.port
            ));
        }
        conn.execute(
            r#"
            UPDATE proxies
            SET name = ?, type = ?, host = ?, port = ?, username = ?, password = ?, enabled = ?,
                test_url = ?, test_timeout = ?, skip_cert_verify = ?,
                status = CASE WHEN ? = 1 THEN 'unknown' ELSE status END,
                last_test = CASE WHEN ? = 1 THEN NULL ELSE last_test END,
                response_time = CASE WHEN ? = 1 THEN NULL ELSE response_time END,
                success_count = CASE WHEN ? = 1 THEN 0 ELSE success_count END,
                fail_count = CASE WHEN ? = 1 THEN 0 ELSE fail_count END
            WHERE id = ?
            "#,
            params![
                input.name,
                input.proxy_type,
                input.host,
                input.port,
                input.username,
                input.password,
                input.enabled.unwrap_or(1),
                empty_to_none(input.test_url),
                input.test_timeout,
                flag_from_value(input.skip_cert_verify.as_ref()),
                i64::from(reset_status),
                i64::from(reset_status),
                i64::from(reset_status),
                i64::from(reset_status),
                i64::from(reset_status),
                id
            ],
        )?;
        drop(conn);
        let proxy = self.get_proxy(id)?.ok_or_else(|| anyhow!("代理不存在"))?;
        Ok((proxy, reset_status))
    }

    pub fn delete_proxy(&self, id: i64) -> Result<()> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM proxies WHERE id = ?", params![id])?;
        Ok(())
    }

    pub fn update_proxy_priority(&self, id: i64, priority: i64) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE proxies SET priority = ? WHERE id = ?",
            params![priority, id],
        )?;
        Ok(())
    }

    pub fn update_proxy_priorities(&self, priorities: &[(i64, i64)]) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        for (id, priority) in priorities {
            tx.execute(
                "UPDATE proxies SET priority = ? WHERE id = ?",
                params![priority, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn update_proxy_status(
        &self,
        id: i64,
        status: &str,
        response_time: Option<i64>,
        success_delta: i64,
        fail_delta: i64,
    ) -> Result<bool> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE proxies
            SET status = ?,
                last_test = CURRENT_TIMESTAMP,
                response_time = ?,
                success_count = success_count + ?,
                fail_count = fail_count + ?
            WHERE id = ?
            "#,
            params![status, response_time, success_delta, fail_delta, id],
        )?;
        Ok(updated == 1)
    }

    pub fn record_proxy_probe_result(
        &self,
        proxy: &ProxyRecord,
        status: Option<&str>,
        response_time: Option<i64>,
        success: bool,
    ) -> Result<bool> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE proxies
            SET status = CASE WHEN ? IS NULL THEN status ELSE ? END,
                last_test = CURRENT_TIMESTAMP,
                response_time = CASE WHEN ? IS NULL THEN response_time ELSE ? END,
                success_count = success_count + ?,
                fail_count = fail_count + ?
            WHERE id = ? AND type = ? AND host = ? AND port = ?
              AND username IS ? AND password IS ? AND enabled = ?
              AND test_url IS ? AND test_timeout IS ? AND skip_cert_verify = ?
            "#,
            params![
                status,
                status,
                status,
                response_time,
                i64::from(success),
                i64::from(!success),
                proxy.id,
                proxy.proxy_type,
                proxy.host,
                proxy.port,
                proxy.username,
                proxy.password,
                proxy.enabled,
                proxy.test_url,
                proxy.test_timeout,
                proxy.skip_cert_verify
            ],
        )?;
        Ok(updated == 1)
    }

    pub fn settings_map(&self) -> Result<HashMap<String, String>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<HashMap<_, _>>>()
            .map_err(Into::into)
    }

    pub fn save_settings(&self, settings: &Map<String, Value>) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        for (key, value) in settings {
            let value = setting_value_to_string(value);
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_advanced_config(&self) -> Result<Value> {
        let mut config = default_advanced_config();
        let keys = config.keys().cloned().collect::<HashSet<_>>();
        let settings = self.settings_map()?;
        for (key, raw) in settings {
            if !keys.contains(&key) {
                continue;
            }
            let value = match config.get(&key) {
                Some(Value::Bool(_)) => parse_bool_setting_value(&key, &raw)?,
                Some(Value::String(_)) => Value::String(raw),
                _ => parse_setting_value(&raw),
            };
            config.insert(key, value);
        }
        Ok(Value::Object(config))
    }

    pub fn reset_advanced_config(&self) -> Result<()> {
        let keys = default_advanced_config()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        for key in keys {
            tx.execute("DELETE FROM settings WHERE key = ?", params![key])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn export_bundle(
        &self,
        proxy_ids: &[i64],
        dns_ids: &[i64],
        group_ids: &[i64],
    ) -> Result<ConfigBundle> {
        let proxy_ids = proxy_ids.iter().copied().collect::<HashSet<_>>();
        let dns_ids = dns_ids.iter().copied().collect::<HashSet<_>>();
        let group_ids = group_ids.iter().copied().collect::<HashSet<_>>();

        let proxies = self
            .list_proxies()?
            .into_iter()
            .filter(|proxy| proxy_ids.contains(&proxy.id))
            .map(|proxy| BundleProxy {
                name: proxy.name,
                proxy_type: proxy.proxy_type,
                host: proxy.host,
                port: proxy.port,
                username: proxy.username,
                password: proxy.password,
                enabled: proxy.enabled,
                priority: proxy.priority,
                test_url: proxy.test_url,
                test_timeout: proxy.test_timeout,
                skip_cert_verify: proxy.skip_cert_verify,
            })
            .collect();

        let dns_mappings = self
            .list_dns_mappings()?
            .into_iter()
            .filter(|mapping| dns_ids.contains(&mapping.id))
            .map(|mapping| BundleDns {
                domain: mapping.domain,
                ip: mapping.ip,
                description: mapping.description,
                enabled: mapping.enabled,
                dynamic: mapping.dynamic,
            })
            .collect();

        let proxy_groups = self
            .list_proxy_groups()?
            .into_iter()
            .filter(|group| group_ids.contains(&group.id))
            .map(|group| BundleGroup {
                name: group.name,
                is_default: group.is_default,
                enabled: group.enabled,
                domains: group
                    .domains
                    .into_iter()
                    .map(|domain| domain.domain)
                    .collect(),
                members: group
                    .members
                    .into_iter()
                    // 成员跟随代理勾选：未勾选的代理连地址引用都不写入导出文件，
                    // 因此分组允许导出为空成员（仅保留域名规则）
                    .filter(|member| proxy_ids.contains(&member.proxy_id))
                    .map(|member| BundleGroupMember {
                        name: member.name,
                        proxy_type: member.proxy_type,
                        host: member.host,
                        port: member.port,
                    })
                    .collect(),
            })
            .collect();

        Ok(ConfigBundle {
            kind: CONFIG_BUNDLE_KIND.to_string(),
            version: crate::version::VERSION.to_string(),
            exported_at: chrono::Utc::now().to_rfc3339(),
            proxies,
            dns_mappings,
            proxy_groups,
        })
    }

    pub fn import_bundle(&self, bundle: &ConfigBundle) -> Result<ImportSummary> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let mut summary = ImportSummary::default();

        for bundled_proxy in &bundle.proxies {
            let input = ProxyInput {
                name: bundled_proxy.name.clone(),
                proxy_type: bundled_proxy.proxy_type.clone(),
                host: bundled_proxy.host.clone(),
                port: bundled_proxy.port,
                username: bundled_proxy.username.clone(),
                password: bundled_proxy.password.clone(),
                enabled: Some(bundled_proxy.enabled),
                test_url: bundled_proxy.test_url.clone(),
                test_timeout: bundled_proxy.test_timeout,
                skip_cert_verify: Some(Value::from(bundled_proxy.skip_cert_verify)),
            };
            let Ok(proxy) = normalize_proxy_input(input) else {
                summary.proxies.skipped += 1;
                continue;
            };
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT id FROM proxies WHERE host = ? AND port = ? AND type = ?",
                    params![proxy.host, proxy.port, proxy.proxy_type],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_some() {
                summary.proxies.skipped += 1;
                continue;
            }
            tx.execute(
                r#"
                INSERT INTO proxies
                  (name, type, host, port, username, password, enabled, priority, test_url, test_timeout, skip_cert_verify)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                params![
                    proxy.name,
                    proxy.proxy_type,
                    proxy.host,
                    proxy.port,
                    proxy.username,
                    proxy.password,
                    proxy.enabled.unwrap_or(1),
                    bundled_proxy.priority,
                    proxy.test_url,
                    proxy.test_timeout,
                    flag_from_value(proxy.skip_cert_verify.as_ref())
                ],
            )?;
            summary.proxies.added += 1;
        }

        for mapping in &bundle.dns_mappings {
            let Ok(domain) = normalize_dns_domain(&mapping.domain) else {
                summary.dns_mappings.skipped += 1;
                continue;
            };
            if validate_ipv4(mapping.ip.trim()).is_err() {
                summary.dns_mappings.skipped += 1;
                continue;
            }
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT id FROM dns_mappings WHERE domain = ?",
                    params![domain],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_some() {
                summary.dns_mappings.skipped += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO dns_mappings (domain, ip, description, enabled, dynamic) VALUES (?, ?, ?, ?, ?)",
                params![
                    domain,
                    mapping.ip.trim(),
                    mapping.description,
                    i64::from(mapping.enabled != 0),
                    i64::from(mapping.dynamic != 0)
                ],
            )?;
            summary.dns_mappings.added += 1;
        }

        for group in &bundle.proxy_groups {
            let name = group.name.trim();
            let invalid_domain = group.domains.iter().any(|domain| {
                let domain = domain.trim().to_lowercase();
                !domain.is_empty() && validate_group_domain(&domain).is_err()
            });
            if name.is_empty() || invalid_domain {
                summary.proxy_groups.skipped += 1;
                continue;
            }
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT id FROM proxy_groups WHERE name = ?",
                    params![name],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_some() {
                summary.proxy_groups.skipped += 1;
                continue;
            }
            // 仅当本地尚无默认分组时才保留导入分组的默认标记，避免悄悄改变现有默认分组
            let has_default: i64 = tx.query_row(
                "SELECT COUNT(*) FROM proxy_groups WHERE is_default = 1",
                [],
                |row| row.get(0),
            )?;
            let is_default = i64::from(group.is_default == 1 && has_default == 0);
            tx.execute(
                "INSERT INTO proxy_groups (name, is_default, enabled) VALUES (?, ?, ?)",
                params![name, is_default, i64::from(group.enabled != 0)],
            )?;
            let group_id = tx.last_insert_rowid();
            save_group_domains(&tx, group_id, group.domains.clone())?;
            for member in &group.members {
                let proxy_type = normalize_proxy_type(&member.proxy_type).ok();
                let host = member.host.trim().to_lowercase();
                let proxy_id: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM proxies WHERE host = ? AND port = ? AND type = ?",
                        params![host, member.port, proxy_type],
                        |row| row.get(0),
                    )
                    .optional()?;
                match proxy_id {
                    Some(proxy_id) => {
                        tx.execute(
                            "INSERT OR IGNORE INTO proxy_group_members (group_id, proxy_id) VALUES (?, ?)",
                            params![group_id, proxy_id],
                        )?;
                    }
                    None => summary.unresolved_members += 1,
                }
            }
            summary.proxy_groups.added += 1;
        }

        tx.commit()?;
        Ok(summary)
    }

    pub fn list_dns_mappings(&self) -> Result<Vec<DnsMapping>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT * FROM dns_mappings ORDER BY domain ASC")?;
        let rows = stmt.query_map([], dns_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn active_dns_mappings(&self) -> Result<HashMap<String, String>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT domain, ip FROM dns_mappings WHERE enabled = 1")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<HashMap<_, _>>>()
            .map_err(Into::into)
    }

    pub fn create_dns_mapping(&self, input: DnsInput) -> Result<DnsMapping> {
        validate_ipv4(input.ip.trim())?;
        let domain = normalize_dns_domain(&input.domain)?;
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO dns_mappings (domain, ip, description, enabled, dynamic) VALUES (?, ?, ?, ?, ?)",
            params![
                domain,
                input.ip.trim(),
                input.description,
                i64::from(input.enabled.unwrap_or(1) != 0),
                i64::from(input.dynamic.unwrap_or(0) != 0)
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.get_dns_mapping(id)?
            .ok_or_else(|| anyhow!("DNS 映射创建后无法读取"))
    }

    pub fn get_dns_mapping(&self, id: i64) -> Result<Option<DnsMapping>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT * FROM dns_mappings WHERE id = ?",
            params![id],
            dns_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn update_dns_mapping(&self, id: i64, input: DnsInput) -> Result<DnsMapping> {
        validate_ipv4(input.ip.trim())?;
        let domain = normalize_dns_domain(&input.domain)?;
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE dns_mappings
            SET domain = ?, ip = ?, description = ?, enabled = ?, dynamic = ?, updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            params![
                domain,
                input.ip.trim(),
                input.description,
                i64::from(input.enabled.unwrap_or(1) != 0),
                i64::from(input.dynamic.unwrap_or(0) != 0),
                id
            ],
        )?;
        drop(conn);
        self.get_dns_mapping(id)?
            .ok_or_else(|| anyhow!("DNS 映射不存在"))
    }

    /// 仅当映射仍与解析开始时一致才更新，避免慢 DNS 结果覆盖用户刚保存的新配置。
    pub fn update_dns_ip_if_unchanged(
        &self,
        id: i64,
        expected_domain: &str,
        expected_ip: &str,
        expected_enabled: i64,
        expected_dynamic: i64,
        ip: &str,
    ) -> Result<bool> {
        validate_ipv4(ip)?;
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE dns_mappings
            SET ip = ?, last_resolved = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
            WHERE id = ? AND domain = ? AND ip = ? AND enabled = ? AND dynamic = ?
            "#,
            params![
                ip.trim(),
                id,
                expected_domain,
                expected_ip,
                expected_enabled,
                expected_dynamic
            ],
        )?;
        Ok(updated == 1)
    }

    /// 启用中的动态映射，供后台定期解析刷新。
    pub fn list_dynamic_dns_mappings(&self) -> Result<Vec<DnsMapping>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT * FROM dns_mappings WHERE dynamic = 1 AND enabled = 1 ORDER BY domain ASC",
        )?;
        let rows = stmt.query_map([], dns_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn delete_dns_mapping(&self, id: i64) -> Result<()> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM dns_mappings WHERE id = ?", params![id])?;
        Ok(())
    }

    pub fn toggle_dns_mapping(&self, id: i64) -> Result<i64> {
        let conn = self.connection()?;
        let current: i64 = conn
            .query_row(
                "SELECT enabled FROM dns_mappings WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("DNS 映射不存在"))?;
        let next = if current == 1 { 0 } else { 1 };
        conn.execute(
            "UPDATE dns_mappings SET enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![next, id],
        )?;
        Ok(next)
    }

    pub fn list_proxy_groups(&self) -> Result<Vec<ProxyGroup>> {
        let conn = self.connection()?;
        let mut stmt =
            conn.prepare("SELECT * FROM proxy_groups ORDER BY is_default DESC, id ASC")?;
        let groups = stmt
            .query_map([], |row| {
                Ok(ProxyGroup {
                    id: row.get("id")?,
                    name: row.get("name")?,
                    is_default: row.get("is_default")?,
                    enabled: row.get("enabled")?,
                    created_at: row.get("created_at")?,
                    updated_at: row.get("updated_at")?,
                    domains: Vec::new(),
                    members: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        let mut result = Vec::with_capacity(groups.len());
        for mut group in groups {
            group.domains = query_group_domains(&conn, group.id)?;
            group.members = query_group_members(&conn, group.id)?;
            result.push(group);
        }
        Ok(result)
    }

    pub fn create_proxy_group(&self, input: ProxyGroupInput) -> Result<ProxyGroup> {
        let name = input
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow!("分组名称不能为空"))?
            .to_string();
        let is_default = i64::from(input.is_default.unwrap_or(0) != 0);
        let enabled = i64::from(input.enabled.unwrap_or(1) != 0);
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        if is_default == 1 {
            tx.execute("UPDATE proxy_groups SET is_default = 0", [])?;
        }
        tx.execute(
            "INSERT INTO proxy_groups (name, is_default, enabled) VALUES (?, ?, ?)",
            params![name, is_default, enabled],
        )?;
        let id = tx.last_insert_rowid();
        save_group_domains(&tx, id, input.domains.unwrap_or_default())?;
        save_group_members(&tx, id, input.proxy_ids.unwrap_or_default())?;
        tx.commit()?;
        drop(conn);
        self.get_proxy_group(id)?
            .ok_or_else(|| anyhow!("代理分组创建后无法读取"))
    }

    pub fn update_proxy_group(&self, id: i64, input: ProxyGroupInput) -> Result<ProxyGroup> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let existing: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT name, is_default, enabled FROM proxy_groups WHERE id = ?",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (current_name, current_default, current_enabled) =
            existing.ok_or_else(|| anyhow!("分组不存在"))?;
        let next_name = input
            .name
            .as_deref()
            .map(str::trim)
            .unwrap_or(&current_name);
        if next_name.is_empty() {
            return Err(anyhow!("分组名称不能为空"));
        }
        let next_default = input
            .is_default
            .map(|value| i64::from(value != 0))
            .unwrap_or(current_default);
        if next_default == 1 {
            tx.execute("UPDATE proxy_groups SET is_default = 0", [])?;
        }

        tx.execute(
            r#"
            UPDATE proxy_groups
            SET name = ?, is_default = ?, enabled = ?, updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            params![
                next_name,
                next_default,
                input
                    .enabled
                    .map(|value| i64::from(value != 0))
                    .unwrap_or(current_enabled),
                id
            ],
        )?;

        if let Some(domains) = input.domains {
            tx.execute(
                "DELETE FROM proxy_group_domains WHERE group_id = ?",
                params![id],
            )?;
            save_group_domains(&tx, id, domains)?;
        }
        if let Some(proxy_ids) = input.proxy_ids {
            tx.execute(
                "DELETE FROM proxy_group_members WHERE group_id = ?",
                params![id],
            )?;
            save_group_members(&tx, id, proxy_ids)?;
        }
        tx.commit()?;
        drop(conn);
        self.get_proxy_group(id)?
            .ok_or_else(|| anyhow!("代理分组不存在"))
    }

    pub fn get_proxy_group(&self, id: i64) -> Result<Option<ProxyGroup>> {
        let conn = self.connection()?;
        let group = conn
            .query_row(
                "SELECT * FROM proxy_groups WHERE id = ?",
                params![id],
                |row| {
                    Ok(ProxyGroup {
                        id: row.get("id")?,
                        name: row.get("name")?,
                        is_default: row.get("is_default")?,
                        enabled: row.get("enabled")?,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                        domains: Vec::new(),
                        members: Vec::new(),
                    })
                },
            )
            .optional()?;
        if let Some(mut group) = group {
            group.domains = query_group_domains(&conn, group.id)?;
            group.members = query_group_members(&conn, group.id)?;
            Ok(Some(group))
        } else {
            Ok(None)
        }
    }

    pub fn delete_proxy_group(&self, id: i64) -> Result<()> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM proxy_groups WHERE id = ?", params![id])?;
        Ok(())
    }

    pub fn group_proxy_selection(&self, target_host: &str) -> Result<Option<ProxyGroupSelection>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT g.id, g.name, g.is_default, d.domain
            FROM proxy_groups g
            LEFT JOIN proxy_group_domains d ON d.group_id = g.id
            WHERE g.enabled = 1
            ORDER BY g.id ASC, d.id ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let host = target_host.to_lowercase();
        let mut best_match: Option<(usize, i64, String)> = None;
        let mut default_group: Option<(i64, String)> = None;
        for row in rows {
            let (group_id, group_name, is_default, domain) = row?;
            if is_default == 1 {
                default_group = Some((group_id, group_name.clone()));
            }
            let Some(domain) = domain else {
                continue;
            };
            if domain_matches(&host, &domain) {
                let specificity = domain.replace('*', "").len();
                if best_match
                    .as_ref()
                    .map(|(current, ..)| specificity > *current)
                    .unwrap_or(true)
                {
                    best_match = Some((specificity, group_id, group_name));
                }
            }
        }
        drop(stmt);

        let selected = best_match.map(|(_, id, name)| (id, name)).or(default_group);
        let Some((group_id, group_name)) = selected else {
            return Ok(None);
        };
        let mut members = conn.prepare(
            "SELECT proxy_id FROM proxy_group_members WHERE group_id = ? ORDER BY id ASC",
        )?;
        let proxy_ids = members
            .query_map(params![group_id], |row| row.get(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(Some(ProxyGroupSelection {
            group_name,
            proxy_ids,
        }))
    }

    pub fn log_request(&self, entry: &RequestLogEntry<'_>) -> Result<i64> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO request_logs
              (proxy_id, target_host, target_port, success, response_time, error_message, result_type)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                entry.proxy_id,
                entry.target_host,
                entry.target_port,
                if entry.success { 1 } else { 0 },
                entry.response_time,
                entry.error_message,
                entry.result_type
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn prune_request_logs(&self, retention_days: i64) -> Result<usize> {
        if retention_days < 1 {
            return Err(anyhow!("日志保留天数必须大于 0"));
        }
        let conn = self.connection()?;
        let deleted = conn.execute(
            r#"
            DELETE FROM request_logs
            WHERE created_at < datetime('now', '-' || ? || ' days')
            "#,
            params![retention_days],
        )?;
        Ok(deleted)
    }

    pub fn traffic_logs(
        &self,
        page: i64,
        page_size: i64,
        proxy_search: Option<&str>,
    ) -> Result<(Vec<TrafficLog>, i64)> {
        let page = page.max(1);
        let page_size = page_size.max(1);
        let offset = (page - 1) * page_size;
        let conn = self.connection()?;
        let proxy_search = proxy_search
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let (total, items) = if let Some(proxy_search) = proxy_search {
            let pattern = like_pattern(proxy_search);
            let total = conn.query_row(
                r#"
                SELECT COUNT(*)
                FROM request_logs rl
                LEFT JOIN proxies p ON p.id = rl.proxy_id
                WHERE COALESCE(rl.result_type, '') NOT IN ('health_success', 'health_failure')
                  AND COALESCE(p.name, '') LIKE ? ESCAPE '\'
                "#,
                params![pattern],
                |row| row.get(0),
            )?;
            let mut stmt = conn.prepare(
                r#"
                SELECT rl.id, rl.proxy_id, p.name AS proxy_name, p.type AS proxy_type,
                  p.host AS proxy_host, p.port AS proxy_port, rl.target_host, rl.target_port,
                  rl.success, rl.response_time, rl.error_message, rl.result_type, rl.created_at
                FROM request_logs rl
                LEFT JOIN proxies p ON p.id = rl.proxy_id
                WHERE COALESCE(rl.result_type, '') NOT IN ('health_success', 'health_failure')
                  AND COALESCE(p.name, '') LIKE ? ESCAPE '\'
                ORDER BY rl.id DESC
                LIMIT ? OFFSET ?
                "#,
            )?;
            let rows = stmt.query_map(params![pattern, page_size, offset], traffic_log_from_row)?;
            (total, rows.collect::<rusqlite::Result<Vec<_>>>()?)
        } else {
            let total = conn.query_row(
                r#"
                SELECT COUNT(*)
                FROM request_logs
                WHERE COALESCE(result_type, '') NOT IN ('health_success', 'health_failure')
                "#,
                [],
                |row| row.get(0),
            )?;
            let mut stmt = conn.prepare(
                r#"
                SELECT rl.id, rl.proxy_id, p.name AS proxy_name, p.type AS proxy_type,
                  p.host AS proxy_host, p.port AS proxy_port, rl.target_host, rl.target_port,
                  rl.success, rl.response_time, rl.error_message, rl.result_type, rl.created_at
                FROM request_logs rl
                LEFT JOIN proxies p ON p.id = rl.proxy_id
                WHERE COALESCE(rl.result_type, '') NOT IN ('health_success', 'health_failure')
                ORDER BY rl.id DESC
                LIMIT ? OFFSET ?
                "#,
            )?;
            let rows = stmt.query_map(params![page_size, offset], traffic_log_from_row)?;
            (total, rows.collect::<rusqlite::Result<Vec<_>>>()?)
        };

        Ok((items, total))
    }

    pub fn clear_traffic_logs(&self) -> Result<i64> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let deleted = tx.execute(
            r#"
            DELETE FROM request_logs
            WHERE COALESCE(result_type, '') NOT IN ('health_success', 'health_failure')
            "#,
            [],
        )?;
        tx.execute(
            "DELETE FROM settings WHERE key = ?",
            params![TRAFFIC_LOG_VISIBLE_AFTER_ID_KEY],
        )?;
        tx.commit()?;
        Ok(deleted as i64)
    }

    pub fn scalar_json(&self, sql: &str) -> Result<Value> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(sql)?;
        let column_count = stmt.column_count();
        let column_names = stmt
            .column_names()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let rows = stmt.query_map([], |row| {
            let mut map = Map::new();
            for (i, column_name) in column_names.iter().enumerate().take(column_count) {
                let value = sql_value_to_json(row, i)?;
                map.insert(column_name.clone(), value);
            }
            Ok(Value::Object(map))
        })?;
        let values = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Value::Array(values))
    }

    pub fn overview(&self, uptime: i64) -> Result<Value> {
        let conn = self.connection()?;
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxies WHERE status = 'active' AND enabled = 1",
            [],
            |row| row.get(0),
        )?;
        let (total, success, failed): (i64, i64, i64) = conn.query_row(
            r#"
            SELECT
              COUNT(*),
              COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0),
              COALESCE(SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END), 0)
            FROM request_logs
            WHERE created_at >= datetime('now', '-24 hours')
              AND COALESCE(result_type, '') NOT IN ('health_success', 'health_failure')
            "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let avg: Option<f64> = conn.query_row(
            r#"
            SELECT AVG(response_time)
            FROM request_logs
            WHERE success = 1 AND created_at >= datetime('now', '-24 hours')
              AND COALESCE(result_type, '') NOT IN ('health_success', 'health_failure')
            "#,
            [],
            |row| row.get(0),
        )?;
        Ok(json!({
            "activeProxies": active,
            "totalRequests": total,
            "successRequests": success,
            "failedRequests": failed,
            "avgResponseTime": avg.unwrap_or(0.0).round() as i64,
            "uptime": uptime
        }))
    }
}

fn default_data_dir() -> Result<PathBuf> {
    if cfg!(debug_assertions) {
        return Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .join("data"));
    }

    if cfg!(target_os = "windows") {
        return Ok(current_exe_dir()?.join("data"));
    }

    platform_data_dir()
}

fn platform_data_dir() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        return env::var("HOME")
            .map(|value| {
                PathBuf::from(value)
                    .join("Library")
                    .join("Application Support")
                    .join("proxy-load")
            })
            .with_context(|| "无法确定 macOS 用户数据目录，缺少 HOME 环境变量");
    }

    if cfg!(target_os = "linux") {
        if let Ok(value) = env::var("XDG_DATA_HOME") {
            return Ok(PathBuf::from(value).join("proxy-load"));
        }
        return env::var("HOME")
            .map(|value| {
                PathBuf::from(value)
                    .join(".local")
                    .join("share")
                    .join("proxy-load")
            })
            .with_context(|| "无法确定 Linux 用户数据目录，缺少 XDG_DATA_HOME 和 HOME 环境变量");
    }

    current_exe_dir().map(|path| path.join("data"))
}

fn current_exe_dir() -> Result<PathBuf> {
    env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("无法确定当前应用目录"))
}

fn import_initial_database(data_dir: &Path) -> Result<()> {
    let target_db = data_dir.join("proxy.db");
    if target_db.exists() {
        return Ok(());
    }

    for source_dir in initial_database_source_dirs()? {
        let source_db = source_dir.join("proxy.db");
        if !source_db.exists() || source_db == target_db {
            continue;
        }

        fs::copy(&source_db, &target_db).with_context(|| {
            format!(
                "导入初始数据库失败: {} -> {}",
                source_db.display(),
                target_db.display()
            )
        })?;
        copy_sqlite_sidecar(&source_db, &target_db, "wal")?;
        copy_sqlite_sidecar(&source_db, &target_db, "shm")?;
        return Ok(());
    }

    Ok(())
}

fn initial_database_source_dirs() -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let exe_dir = current_exe_dir()?;
    dirs.push(exe_dir.join("data"));

    if cfg!(target_os = "macos") {
        if let Some(app_dir) = macos_app_dir(&exe_dir) {
            dirs.push(app_dir.join("Contents").join("Resources").join("data"));
            if let Some(parent) = app_dir.parent() {
                dirs.push(parent.join("data"));
            }
        }
    }

    Ok(dirs)
}

fn copy_sqlite_sidecar(source_db: &Path, target_db: &Path, suffix: &str) -> Result<()> {
    let source = sidecar_path(source_db, suffix);
    if !source.exists() {
        return Ok(());
    }

    let target = sidecar_path(target_db, suffix);
    fs::copy(&source, &target).with_context(|| {
        format!(
            "导入 SQLite 附属文件失败: {} -> {}",
            source.display(),
            target.display()
        )
    })?;
    Ok(())
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut value = db_path.as_os_str().to_os_string();
    value.push(format!("-{suffix}"));
    PathBuf::from(value)
}

#[cfg(target_os = "macos")]
fn macos_app_dir(exe_dir: &Path) -> Option<PathBuf> {
    let mut current = Some(exe_dir);
    while let Some(path) = current {
        if path.extension().and_then(|value| value.to_str()) == Some("app") {
            return Some(path.to_path_buf());
        }
        current = path.parent();
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn macos_app_dir(_exe_dir: &Path) -> Option<PathBuf> {
    None
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    if !columns.contains(column) {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

pub fn default_advanced_config() -> Map<String, Value> {
    let proxy_port = default_proxy_port();
    Map::from_iter([
        ("proxy_port".to_string(), json!(proxy_port)),
        ("allow_lan".to_string(), json!(false)),
        ("inbound_auth_enabled".to_string(), json!(false)),
        ("inbound_auth_username".to_string(), json!("")),
        ("inbound_auth_password".to_string(), json!("")),
        ("periodic_test_interval".to_string(), json!(3 * 60 * 1000)),
        ("probe_recovery_interval".to_string(), json!(3 * 60 * 1000)),
        ("probe_concurrency".to_string(), json!(8)),
        ("probe_failure_threshold".to_string(), json!(2)),
        ("dns_refresh_interval".to_string(), json!(5 * 60 * 1000)),
        ("background_run".to_string(), json!(false)),
        ("start_minimized".to_string(), json!(false)),
        ("log_retention_days".to_string(), json!(7)),
        ("circuit_failure_threshold".to_string(), json!(5)),
        ("circuit_timeout".to_string(), json!(60000)),
        ("failfast_enabled".to_string(), json!(true)),
        ("failfast_max_attempts".to_string(), json!(3)),
        ("failfast_attempt_timeout".to_string(), json!(10000)),
        ("failfast_total_timeout".to_string(), json!(30000)),
    ])
}

fn default_proxy_port() -> i64 {
    static PORT: OnceLock<i64> = OnceLock::new();
    *PORT.get_or_init(|| match env::var("PROXY_PORT") {
        Ok(raw) => match raw.parse::<i64>() {
            Ok(port) if (1..=65_535).contains(&port) => port,
            _ => {
                eprintln!("忽略无效的 PROXY_PORT={raw:?}，端口必须在 1 到 65535 之间");
                5678
            }
        },
        Err(_) => 5678,
    })
}

fn proxy_from_row(row: &Row<'_>) -> rusqlite::Result<ProxyRecord> {
    Ok(ProxyRecord {
        id: row.get("id")?,
        name: row.get("name")?,
        proxy_type: row.get("type")?,
        host: row.get("host")?,
        port: row.get("port")?,
        username: row.get("username")?,
        password: row.get("password")?,
        status: row.get("status")?,
        last_test: row.get("last_test")?,
        response_time: row.get("response_time")?,
        success_count: row.get::<_, Option<i64>>("success_count")?.unwrap_or(0),
        fail_count: row.get::<_, Option<i64>>("fail_count")?.unwrap_or(0),
        priority: row.get::<_, Option<i64>>("priority")?.unwrap_or(999),
        enabled: row.get::<_, Option<i64>>("enabled")?.unwrap_or(1),
        skip_cert_verify: row.get::<_, Option<i64>>("skip_cert_verify")?.unwrap_or(0),
        test_url: row.get("test_url")?,
        test_timeout: row.get("test_timeout")?,
        score: None,
        active_connections: None,
    })
}

fn traffic_log_from_row(row: &Row<'_>) -> rusqlite::Result<TrafficLog> {
    Ok(TrafficLog {
        id: row.get("id")?,
        proxy_id: row.get("proxy_id")?,
        proxy_name: row.get("proxy_name")?,
        proxy_type: row.get("proxy_type")?,
        proxy_host: row.get("proxy_host")?,
        proxy_port: row.get("proxy_port")?,
        target_host: row.get("target_host")?,
        target_port: row.get("target_port")?,
        success: row.get("success")?,
        response_time: row.get("response_time")?,
        error_message: row.get("error_message")?,
        result_type: row.get("result_type")?,
        created_at: row.get("created_at")?,
    })
}

fn like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('%');
    for character in value.chars() {
        match character {
            '%' | '_' | '\\' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped.push('%');
    escaped
}

fn dns_from_row(row: &Row<'_>) -> rusqlite::Result<DnsMapping> {
    Ok(DnsMapping {
        id: row.get("id")?,
        domain: row.get("domain")?,
        ip: row.get("ip")?,
        description: row.get("description")?,
        enabled: row.get::<_, Option<i64>>("enabled")?.unwrap_or(1),
        dynamic: row.get::<_, Option<i64>>("dynamic")?.unwrap_or(0),
        last_resolved: row.get("last_resolved")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn query_group_domains(conn: &Connection, group_id: i64) -> Result<Vec<ProxyGroupDomain>> {
    let mut stmt = conn.prepare("SELECT * FROM proxy_group_domains WHERE group_id = ?")?;
    let rows = stmt.query_map(params![group_id], |row| {
        Ok(ProxyGroupDomain {
            id: row.get("id")?,
            group_id: row.get("group_id")?,
            domain: row.get("domain")?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn query_group_members(conn: &Connection, group_id: i64) -> Result<Vec<ProxyGroupMember>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT pgm.proxy_id, p.name, p.type, p.host, p.port, p.status, p.enabled
        FROM proxy_group_members pgm
        JOIN proxies p ON p.id = pgm.proxy_id
        WHERE pgm.group_id = ?
        "#,
    )?;
    let rows = stmt.query_map(params![group_id], |row| {
        Ok(ProxyGroupMember {
            proxy_id: row.get("proxy_id")?,
            name: row.get("name")?,
            proxy_type: row.get("type")?,
            host: row.get("host")?,
            port: row.get("port")?,
            status: row.get("status")?,
            enabled: row.get("enabled")?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn save_group_domains(conn: &Connection, group_id: i64, domains: Vec<String>) -> Result<()> {
    let mut seen = HashSet::new();
    for domain in domains {
        let domain = domain.trim().to_lowercase();
        if domain.is_empty() {
            continue;
        }
        validate_group_domain(&domain)?;
        if seen.insert(domain.clone()) {
            conn.execute(
                "INSERT INTO proxy_group_domains (group_id, domain) VALUES (?, ?)",
                params![group_id, domain],
            )?;
        }
    }
    Ok(())
}

fn save_group_members(conn: &Connection, group_id: i64, proxy_ids: Vec<i64>) -> Result<()> {
    for proxy_id in proxy_ids {
        conn.execute(
            "INSERT OR IGNORE INTO proxy_group_members (group_id, proxy_id) VALUES (?, ?)",
            params![group_id, proxy_id],
        )?;
    }
    Ok(())
}

fn proxy_connection_config_changed(existing: &ProxyRecord, input: &ProxyInput) -> bool {
    existing.proxy_type != input.proxy_type
        || existing.host != input.host
        || existing.port != input.port
        || existing.username != input.username
        || existing.password != input.password
        || existing.enabled != input.enabled.unwrap_or(1)
        || existing.test_url != input.test_url
        || existing.test_timeout != input.test_timeout
        || existing.skip_cert_verify != flag_from_value(input.skip_cert_verify.as_ref())
}

fn normalize_proxy_input(mut input: ProxyInput) -> Result<ProxyInput> {
    input.name = input.name.trim().to_string();
    input.host = input.host.trim().to_lowercase();
    input.proxy_type = normalize_proxy_type(&input.proxy_type)?;
    input.username = empty_credential_to_none(input.username);
    input.password = empty_credential_to_none(input.password);
    input.test_url = empty_to_none(input.test_url);
    input.enabled = Some(if input.enabled.unwrap_or(1) == 0 {
        0
    } else {
        1
    });
    input.skip_cert_verify = Some(Value::from(flag_from_value(
        input.skip_cert_verify.as_ref(),
    )));

    if input.name.trim().is_empty()
        || input.proxy_type.trim().is_empty()
        || input.host.trim().is_empty()
        || input.port <= 0
    {
        return Err(anyhow!("缺少必填字段"));
    }
    if input.port > 65535 {
        return Err(anyhow!("端口必须在 1 到 65535 之间"));
    }
    if input.host.contains("://")
        || input.host.chars().any(char::is_whitespace)
        || input.host.chars().any(char::is_control)
    {
        return Err(anyhow!(
            "代理主机只能填写域名或 IP 地址，不能包含协议或空白字符"
        ));
    }
    if let Some(username) = input.username.as_deref() {
        if username.contains(':') || username.chars().any(char::is_control) {
            return Err(anyhow!("代理用户名不能包含冒号或控制字符"));
        }
        if matches!(input.proxy_type.as_str(), "socks4" | "socks5") && username.len() > 255 {
            return Err(anyhow!("SOCKS 代理用户名不能超过 255 字节"));
        }
    }
    if let Some(password) = input.password.as_deref() {
        if password.chars().any(char::is_control) {
            return Err(anyhow!("代理密码不能包含控制字符"));
        }
        if input.proxy_type == "socks5" && password.len() > 255 {
            return Err(anyhow!("SOCKS5 代理密码不能超过 255 字节"));
        }
    }
    if input.password.is_some() && input.username.is_none() {
        return Err(anyhow!("配置代理密码时必须同时填写用户名"));
    }
    if input.proxy_type == "socks4" && input.password.is_some() {
        return Err(anyhow!("SOCKS4 协议不支持密码认证，请改用 SOCKS5"));
    }
    if let Some(test_url) = input.test_url.as_deref() {
        let parsed = url::Url::parse(test_url).with_context(|| "测试地址格式无效")?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(anyhow!("测试地址必须是包含主机名的 HTTP 或 HTTPS URL"));
        }
    }
    if let Some(timeout) = input.test_timeout {
        if !(1..=300).contains(&timeout) {
            return Err(anyhow!("代理测试超时必须在 1 到 300 秒之间"));
        }
    }
    Ok(input)
}

fn normalize_proxy_type(value: &str) -> Result<String> {
    match value.trim().to_lowercase().as_str() {
        "http" | "https" => Ok("http".to_string()),
        "socks4" => Ok("socks4".to_string()),
        "socks5" => Ok("socks5".to_string()),
        other => Err(anyhow!("不支持的代理类型: {other}")),
    }
}

fn validate_ipv4(ip: &str) -> Result<()> {
    let parts = ip.split('.').collect::<Vec<_>>();
    if parts.len() != 4
        || !parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.chars().all(|c| c.is_ascii_digit())
                && part.parse::<u8>().is_ok()
        })
    {
        return Err(anyhow!("IP地址格式不正确"));
    }
    Ok(())
}

fn normalize_dns_domain(domain: &str) -> Result<String> {
    let domain = domain.trim().to_lowercase();
    if domain.is_empty() {
        return Err(anyhow!("域名不能为空"));
    }
    if domain.contains('*')
        || domain.contains("://")
        || domain.contains('/')
        || domain.chars().any(char::is_whitespace)
        || domain.chars().any(char::is_control)
    {
        return Err(anyhow!("DNS 映射域名格式无效"));
    }
    Ok(domain)
}

fn validate_group_domain(domain: &str) -> Result<()> {
    let hostname = if domain == "*" {
        return Ok(());
    } else if let Some(suffix) = domain.strip_prefix("*.") {
        suffix
    } else {
        domain
    };
    if hostname.is_empty()
        || hostname.contains('*')
        || hostname.contains("://")
        || hostname.contains('/')
        || hostname.chars().any(char::is_whitespace)
        || hostname.chars().any(char::is_control)
    {
        return Err(anyhow!("分组域名规则只支持精确域名、*.example.com 或 *"));
    }
    Ok(())
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn empty_credential_to_none(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn flag_from_value(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Bool(true)) => 1,
        Some(Value::Number(number)) if number.as_i64().unwrap_or(0) != 0 => 1,
        Some(Value::String(value)) if value == "1" || value.eq_ignore_ascii_case("true") => 1,
        _ => 0,
    }
}

fn parse_setting_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| {
        raw.parse::<i64>()
            .map(Value::from)
            .or_else(|_| raw.parse::<f64>().map(Value::from))
            .unwrap_or_else(|_| Value::String(raw.to_string()))
    })
}

fn parse_bool_setting_value(key: &str, raw: &str) -> Result<Value> {
    if raw == "1" || raw.eq_ignore_ascii_case("true") {
        return Ok(Value::Bool(true));
    }
    if raw == "0" || raw.eq_ignore_ascii_case("false") {
        return Ok(Value::Bool(false));
    }
    Err(anyhow!("设置 {key} 的布尔值无效: {raw:?}"))
}

fn setting_value_to_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

fn sql_value_to_json(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    use rusqlite::types::ValueRef;
    match row.get_ref(index)? {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(value) => Ok(Value::from(value)),
        ValueRef::Real(value) => Ok(Value::from(value)),
        ValueRef::Text(value) => Ok(Value::String(String::from_utf8_lossy(value).to_string())),
        ValueRef::Blob(_) => Ok(Value::Null),
    }
}

fn domain_matches(host: &str, pattern: &str) -> bool {
    let pattern = pattern.to_lowercase();
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    host == pattern
}

#[cfg(test)]
mod tests {
    use super::{
        default_advanced_config, normalize_dns_domain, validate_group_domain, Database,
        RequestLogEntry,
    };
    use crate::models::{ProxyGroupInput, ProxyInput};
    use std::{
        collections::HashSet,
        sync::{Mutex, OnceLock},
    };

    fn test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn proxy_input(name: &str, host: &str, port: i64) -> ProxyInput {
        ProxyInput {
            name: name.to_string(),
            proxy_type: "socks5".to_string(),
            host: host.to_string(),
            port,
            username: None,
            password: None,
            enabled: Some(1),
            test_url: None,
            test_timeout: None,
            skip_cert_verify: None,
        }
    }

    #[test]
    fn default_probe_intervals_are_three_minutes() {
        let config = default_advanced_config();
        assert_eq!(config["periodic_test_interval"].as_i64(), Some(180_000));
        assert_eq!(config["probe_recovery_interval"].as_i64(), Some(180_000));
    }

    #[test]
    fn domain_settings_reject_rules_that_would_never_match() {
        assert!(normalize_dns_domain("api.example.com").is_ok());
        assert!(normalize_dns_domain("https://example.com").is_err());
        assert!(validate_group_domain("*.example.com").is_ok());
        assert!(validate_group_domain("api*.example.com").is_err());
    }

    #[test]
    fn advanced_credentials_remain_strings_when_they_look_like_json_values() {
        let _guard = test_lock().lock().unwrap();
        let data_dir = std::env::temp_dir().join(format!(
            "proxy-load-db-test-{}-credential-types",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::set_var("DATA_DIR", &data_dir);
        let db = Database::open().expect("打开测试数据库");
        db.save_settings(&serde_json::Map::from_iter([
            (
                "inbound_auth_username".to_string(),
                serde_json::json!("123"),
            ),
            (
                "inbound_auth_password".to_string(),
                serde_json::json!("true"),
            ),
        ]))
        .unwrap();

        let config = db.load_advanced_config().unwrap();
        assert_eq!(config["inbound_auth_username"].as_str(), Some("123"));
        assert_eq!(config["inbound_auth_password"].as_str(), Some("true"));

        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::remove_var("DATA_DIR");
    }

    #[test]
    fn changing_proxy_connection_config_invalidates_old_health_status() {
        let _guard = test_lock().lock().unwrap();
        let data_dir = std::env::temp_dir().join(format!(
            "proxy-load-db-test-{}-proxy-update",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::set_var("DATA_DIR", &data_dir);
        let db = Database::open().expect("打开测试数据库");

        let proxy = db
            .create_proxy(proxy_input("原代理", "10.0.0.1", 1080))
            .unwrap();
        db.update_proxy_status(proxy.id, "active", Some(20), 3, 1)
            .unwrap();

        let (updated, connection_changed) = db
            .update_proxy(proxy.id, proxy_input("新代理", "10.0.0.2", 1080))
            .unwrap();
        assert!(connection_changed);
        assert_eq!(updated.status.as_deref(), Some("unknown"));
        assert_eq!(updated.response_time, None);
        assert_eq!(updated.success_count, 0);
        assert_eq!(updated.fail_count, 0);
        assert!(!db
            .record_proxy_probe_result(&proxy, Some("active"), Some(5), true)
            .unwrap());
        assert_eq!(
            db.get_proxy(proxy.id).unwrap().unwrap().status.as_deref(),
            Some("unknown")
        );

        db.update_proxy_status(proxy.id, "active", Some(30), 1, 0)
            .unwrap();
        let (renamed, connection_changed) = db
            .update_proxy(proxy.id, proxy_input("仅改名称", "10.0.0.2", 1080))
            .unwrap();
        assert!(!connection_changed);
        assert_eq!(renamed.status.as_deref(), Some("active"));
        assert_eq!(renamed.response_time, Some(30));

        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::remove_var("DATA_DIR");
    }

    #[test]
    fn export_bundle_filters_group_members_by_selected_proxies() {
        let _guard = test_lock().lock().unwrap();
        let data_dir =
            std::env::temp_dir().join(format!("proxy-load-db-test-{}-export", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::set_var("DATA_DIR", &data_dir);
        let db = Database::open().expect("打开测试数据库");

        let kept = db
            .create_proxy(proxy_input("已勾选", "10.0.0.1", 1080))
            .unwrap();
        let dropped = db
            .create_proxy(proxy_input("未勾选", "10.0.0.2", 1080))
            .unwrap();
        let group = db
            .create_proxy_group(ProxyGroupInput {
                name: Some("测试分组".to_string()),
                domains: Some(vec!["*.example.com".to_string()]),
                proxy_ids: Some(vec![kept.id, dropped.id]),
                is_default: Some(0),
                enabled: Some(1),
            })
            .unwrap();

        // 只勾选其中一个代理：分组成员必须同步剔除未勾选代理，连地址引用也不能出现
        let bundle = db.export_bundle(&[kept.id], &[], &[group.id]).unwrap();
        assert_eq!(bundle.proxies.len(), 1);
        assert_eq!(bundle.proxy_groups.len(), 1);
        let members = &bundle.proxy_groups[0].members;
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].host, kept.host);
        assert!(!serde_json::to_string(&bundle)
            .unwrap()
            .contains(&dropped.host));

        // 一个代理都不勾选：允许导出仅含域名规则的空成员分组
        let bundle = db.export_bundle(&[], &[], &[group.id]).unwrap();
        assert!(bundle.proxies.is_empty());
        assert_eq!(bundle.proxy_groups.len(), 1);
        assert!(bundle.proxy_groups[0].members.is_empty());
        assert_eq!(
            bundle.proxy_groups[0].domains,
            vec!["*.example.com".to_string()]
        );

        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::remove_var("DATA_DIR");
    }

    #[test]
    fn traffic_log_search_and_clear_remove_visible_stats() {
        let _guard = test_lock().lock().unwrap();
        let data_dir =
            std::env::temp_dir().join(format!("proxy-load-db-test-{}-traffic", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::set_var("DATA_DIR", &data_dir);
        let db = Database::open().expect("打开测试数据库");

        let matched = db
            .create_proxy(proxy_input("宁波企服平台http", "10.68.201.207", 7890))
            .unwrap();
        let other = db
            .create_proxy(proxy_input("台州水务socks", "172.31.98.133", 1080))
            .unwrap();
        db.log_request(&RequestLogEntry {
            proxy_id: Some(matched.id),
            target_host: "www.tzswater.com",
            target_port: 443,
            success: false,
            response_time: None,
            error_message: Some("timeout"),
            result_type: "proxy_exhausted",
        })
        .unwrap();
        db.log_request(&RequestLogEntry {
            proxy_id: Some(other.id),
            target_host: "10.68.201.207",
            target_port: 30333,
            success: true,
            response_time: Some(101),
            error_message: None,
            result_type: "direct_success",
        })
        .unwrap();
        db.log_request(&RequestLogEntry {
            proxy_id: Some(other.id),
            target_host: "heartbeat.example.com",
            target_port: 443,
            success: true,
            response_time: Some(25),
            error_message: None,
            result_type: "health_success",
        })
        .unwrap();

        let (items, total) = db.traffic_logs(1, 25, Some("企服")).unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].proxy_id, Some(matched.id));
        let (items, total) = db.traffic_logs(1, 25, Some("172.31.98.133:1080")).unwrap();
        assert_eq!(total, 0);
        assert!(items.is_empty());

        let (_, total) = db.traffic_logs(1, 25, None).unwrap();
        assert_eq!(total, 2);
        assert_eq!(db.clear_traffic_logs().unwrap(), 2);
        let (items, total) = db.traffic_logs(1, 25, None).unwrap();
        assert_eq!(total, 0);
        assert!(items.is_empty());

        let overview = db.overview(60).unwrap();
        assert_eq!(overview["totalRequests"].as_i64(), Some(0));
        assert_eq!(overview["successRequests"].as_i64(), Some(0));
        assert_eq!(overview["failedRequests"].as_i64(), Some(0));

        db.log_request(&RequestLogEntry {
            proxy_id: Some(matched.id),
            target_host: "next.example.com",
            target_port: 80,
            success: true,
            response_time: Some(88),
            error_message: None,
            result_type: "direct_success",
        })
        .unwrap();
        let (items, total) = db.traffic_logs(1, 25, None).unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].target_host.as_deref(), Some("next.example.com"));

        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::remove_var("DATA_DIR");
    }

    #[test]
    fn group_selection_uses_default_and_preserves_empty_matched_groups() {
        let _guard = test_lock().lock().unwrap();
        let data_dir =
            std::env::temp_dir().join(format!("proxy-load-db-test-{}-groups", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::set_var("DATA_DIR", &data_dir);
        let db = Database::open().expect("打开测试数据库");

        let default_proxy = db
            .create_proxy(proxy_input("默认代理", "10.0.0.10", 1080))
            .unwrap();
        db.create_proxy_group(ProxyGroupInput {
            name: Some("默认分组".to_string()),
            domains: Some(Vec::new()),
            proxy_ids: Some(vec![default_proxy.id]),
            is_default: Some(1),
            enabled: Some(1),
        })
        .unwrap();
        db.create_proxy_group(ProxyGroupInput {
            name: Some("受限空分组".to_string()),
            domains: Some(vec!["*.restricted.example".to_string()]),
            proxy_ids: Some(Vec::new()),
            is_default: Some(0),
            enabled: Some(1),
        })
        .unwrap();

        let default_selection = db
            .group_proxy_selection("public.example")
            .unwrap()
            .expect("未命中域名时应使用默认分组");
        assert_eq!(default_selection.group_name, "默认分组");
        assert_eq!(
            default_selection.proxy_ids,
            HashSet::from([default_proxy.id])
        );

        let restricted_selection = db
            .group_proxy_selection("api.restricted.example")
            .unwrap()
            .expect("空成员分组也必须保留路由约束");
        assert_eq!(restricted_selection.group_name, "受限空分组");
        assert!(restricted_selection.proxy_ids.is_empty());

        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::remove_var("DATA_DIR");
    }
}
