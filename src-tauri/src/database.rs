use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
    pub domain_pattern: Option<String>,
    pub is_default: bool,
    pub proxy_ids: HashSet<i64>,
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
              bandwidth_bps INTEGER DEFAULT NULL,
              bandwidth_test_time DATETIME DEFAULT NULL,
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

            CREATE TABLE IF NOT EXISTS load_stats (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              proxy_id INTEGER,
              weight REAL,
              success_rate REAL,
              avg_response_time INTEGER,
              requests_count INTEGER,
              timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
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
            CREATE INDEX IF NOT EXISTS idx_load_stats_proxy ON load_stats (proxy_id, timestamp);
            CREATE INDEX IF NOT EXISTS idx_dns_domain ON dns_mappings (domain);
            CREATE INDEX IF NOT EXISTS idx_dns_ip ON dns_mappings (ip);
            CREATE INDEX IF NOT EXISTS idx_group_domains_group ON proxy_group_domains (group_id);
            CREATE INDEX IF NOT EXISTS idx_group_domains_domain ON proxy_group_domains (domain);
            CREATE INDEX IF NOT EXISTS idx_group_members_group ON proxy_group_members (group_id);
            CREATE INDEX IF NOT EXISTS idx_group_members_proxy ON proxy_group_members (proxy_id);
            "#,
        )?;

        add_column_if_missing(&conn, "proxies", "bandwidth_bps", "INTEGER DEFAULT NULL")?;
        add_column_if_missing(
            &conn,
            "proxies",
            "bandwidth_test_time",
            "DATETIME DEFAULT NULL",
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
                "INSERT INTO settings (key, value) VALUES ('load_mode', 'auto')",
                [],
            )?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('algorithm', 'adaptive')",
                [],
            )?;
        }

        Ok(())
    }

    pub fn list_proxies(&self) -> Result<Vec<ProxyRecord>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT p.*,
              (SELECT weight FROM load_stats WHERE proxy_id = p.id ORDER BY timestamp DESC LIMIT 1) as current_weight
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
            SELECT p.*,
              (SELECT weight FROM load_stats WHERE proxy_id = p.id ORDER BY timestamp DESC LIMIT 1) as current_weight
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
            SELECT p.*,
              (SELECT weight FROM load_stats WHERE proxy_id = p.id ORDER BY timestamp DESC LIMIT 1) as current_weight
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
        validate_proxy_input(&input)?;
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

    pub fn update_proxy(&self, id: i64, input: ProxyInput) -> Result<ProxyRecord> {
        validate_proxy_input(&input)?;
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE proxies
            SET name = ?, type = ?, host = ?, port = ?, username = ?, password = ?, enabled = ?,
                test_url = ?, test_timeout = ?, skip_cert_verify = ?
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
                id
            ],
        )?;
        drop(conn);
        self.get_proxy(id)?.ok_or_else(|| anyhow!("代理不存在"))
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

    pub fn update_proxy_status(
        &self,
        id: i64,
        status: &str,
        response_time: Option<i64>,
        success_delta: i64,
        fail_delta: i64,
    ) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
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
        Ok(())
    }

    pub fn settings_map(&self) -> Result<HashMap<String, String>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<HashMap<_, _>>>()
            .map_err(Into::into)
    }

    pub fn save_settings(&self, settings: &Map<String, Value>) -> Result<()> {
        let conn = self.connection()?;
        for (key, value) in settings {
            let value = setting_value_to_string(value);
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)",
                params![key, value],
            )?;
        }
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
                Some(Value::Bool(_)) => parse_bool_setting_value(&raw),
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
        let conn = self.connection()?;
        for key in keys {
            conn.execute("DELETE FROM settings WHERE key = ?", params![key])?;
        }
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

        for proxy in &bundle.proxies {
            if proxy.name.trim().is_empty()
                || proxy.proxy_type.trim().is_empty()
                || proxy.host.trim().is_empty()
                || proxy.port <= 0
                || proxy.port > 65535
            {
                summary.proxies.skipped += 1;
                continue;
            }
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT id FROM proxies WHERE host = ? AND port = ? AND type = ?",
                    params![proxy.host.trim(), proxy.port, proxy.proxy_type.trim()],
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
                    proxy.name.trim(),
                    proxy.proxy_type.trim(),
                    proxy.host.trim(),
                    proxy.port,
                    proxy.username,
                    proxy.password,
                    i64::from(proxy.enabled != 0),
                    proxy.priority,
                    empty_to_none(proxy.test_url.clone()),
                    proxy.test_timeout,
                    i64::from(proxy.skip_cert_verify != 0)
                ],
            )?;
            summary.proxies.added += 1;
        }

        for mapping in &bundle.dns_mappings {
            let domain = mapping.domain.trim().to_lowercase();
            if domain.is_empty() || validate_ipv4(mapping.ip.trim()).is_err() {
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
            if name.is_empty() {
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
                let proxy_id: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM proxies WHERE host = ? AND port = ? AND type = ?",
                        params![member.host.trim(), member.port, member.proxy_type.trim()],
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
        validate_ipv4(&input.ip)?;
        if input.domain.trim().is_empty() {
            return Err(anyhow!("域名不能为空"));
        }
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO dns_mappings (domain, ip, description, enabled, dynamic) VALUES (?, ?, ?, ?, ?)",
            params![
                input.domain.trim().to_lowercase(),
                input.ip.trim(),
                input.description,
                input.enabled.unwrap_or(1),
                input.dynamic.unwrap_or(0)
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
        validate_ipv4(&input.ip)?;
        if input.domain.trim().is_empty() {
            return Err(anyhow!("域名不能为空"));
        }
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE dns_mappings
            SET domain = ?, ip = ?, description = ?, enabled = ?, dynamic = ?, updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            params![
                input.domain.trim().to_lowercase(),
                input.ip.trim(),
                input.description,
                input.enabled.unwrap_or(1),
                input.dynamic.unwrap_or(0),
                id
            ],
        )?;
        drop(conn);
        self.get_dns_mapping(id)?
            .ok_or_else(|| anyhow!("DNS 映射不存在"))
    }

    /// 动态解析后仅更新 IP 与解析时间，不改动域名/说明等其它字段。
    pub fn update_dns_ip(&self, id: i64, ip: &str) -> Result<()> {
        validate_ipv4(ip)?;
        let conn = self.connection()?;
        conn.execute(
            "UPDATE dns_mappings SET ip = ?, last_resolved = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![ip.trim(), id],
        )?;
        Ok(())
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
        let is_default = input.is_default.unwrap_or(0);
        let enabled = input.enabled.unwrap_or(1);
        let conn = self.connection()?;
        if is_default == 1 {
            conn.execute("UPDATE proxy_groups SET is_default = 0", [])?;
        }
        conn.execute(
            "INSERT INTO proxy_groups (name, is_default, enabled) VALUES (?, ?, ?)",
            params![name, is_default, enabled],
        )?;
        let id = conn.last_insert_rowid();
        save_group_domains(&conn, id, input.domains.unwrap_or_default())?;
        save_group_members(&conn, id, input.proxy_ids.unwrap_or_default())?;
        drop(conn);
        self.get_proxy_group(id)?
            .ok_or_else(|| anyhow!("代理分组创建后无法读取"))
    }

    pub fn update_proxy_group(&self, id: i64, input: ProxyGroupInput) -> Result<ProxyGroup> {
        let conn = self.connection()?;
        let existing: Option<(String, i64, i64)> = conn
            .query_row(
                "SELECT name, is_default, enabled FROM proxy_groups WHERE id = ?",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (current_name, current_default, current_enabled) =
            existing.ok_or_else(|| anyhow!("分组不存在"))?;
        let next_default = input.is_default.unwrap_or(current_default);
        if next_default == 1 {
            conn.execute("UPDATE proxy_groups SET is_default = 0", [])?;
        }

        conn.execute(
            r#"
            UPDATE proxy_groups
            SET name = ?, is_default = ?, enabled = ?, updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            params![
                input.name.unwrap_or(current_name),
                next_default,
                input.enabled.unwrap_or(current_enabled),
                id
            ],
        )?;

        if let Some(domains) = input.domains {
            conn.execute(
                "DELETE FROM proxy_group_domains WHERE group_id = ?",
                params![id],
            )?;
            save_group_domains(&conn, id, domains)?;
        }
        if let Some(proxy_ids) = input.proxy_ids {
            conn.execute(
                "DELETE FROM proxy_group_members WHERE group_id = ?",
                params![id],
            )?;
            save_group_members(&conn, id, proxy_ids)?;
        }
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
        let groups = self.list_proxy_groups()?;
        let mut best_match: Option<(usize, ProxyGroupSelection)> = None;
        let host = target_host.to_lowercase();

        for group in groups.into_iter().filter(|group| group.enabled == 1) {
            let members = group
                .members
                .iter()
                .map(|m| m.proxy_id)
                .collect::<HashSet<_>>();
            if members.is_empty() {
                continue;
            }
            for domain in group.domains {
                if domain_matches(&host, &domain.domain) {
                    let specificity = domain.domain.replace("*", "").len();
                    if best_match
                        .as_ref()
                        .map(|(current, _)| specificity > *current)
                        .unwrap_or(true)
                    {
                        best_match = Some((
                            specificity,
                            ProxyGroupSelection {
                                group_name: group.name.clone(),
                                domain_pattern: Some(domain.domain),
                                is_default: group.is_default == 1,
                                proxy_ids: members.clone(),
                            },
                        ));
                    }
                }
            }
        }

        Ok(best_match.map(|(_, selection)| selection))
    }

    pub fn log_request(
        &self,
        proxy_id: Option<i64>,
        target_host: &str,
        target_port: i64,
        success: bool,
        response_time: Option<i64>,
        error_message: Option<&str>,
        result_type: &str,
    ) -> Result<i64> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO request_logs
              (proxy_id, target_host, target_port, success, response_time, error_message, result_type)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                proxy_id,
                target_host,
                target_port,
                if success { 1 } else { 0 },
                response_time,
                error_message,
                result_type
            ],
        )?;
        Ok(conn.last_insert_rowid())
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
        let visible_after_id = traffic_log_visible_after_id(&conn)?;
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
                WHERE rl.id > ?
                  AND COALESCE(p.name, '') LIKE ? ESCAPE '\'
                "#,
                params![visible_after_id, pattern],
                |row| row.get(0),
            )?;
            let mut stmt = conn.prepare(
                r#"
                SELECT rl.id, rl.proxy_id, p.name AS proxy_name, p.type AS proxy_type,
                  p.host AS proxy_host, p.port AS proxy_port, rl.target_host, rl.target_port,
                  rl.success, rl.response_time, rl.error_message, rl.result_type, rl.created_at
                FROM request_logs rl
                LEFT JOIN proxies p ON p.id = rl.proxy_id
                WHERE rl.id > ?
                  AND COALESCE(p.name, '') LIKE ? ESCAPE '\'
                ORDER BY rl.id DESC
                LIMIT ? OFFSET ?
                "#,
            )?;
            let rows = stmt.query_map(
                params![visible_after_id, pattern, page_size, offset],
                traffic_log_from_row,
            )?;
            (total, rows.collect::<rusqlite::Result<Vec<_>>>()?)
        } else {
            let total = conn.query_row(
                "SELECT COUNT(*) FROM request_logs WHERE id > ?",
                params![visible_after_id],
                |row| row.get(0),
            )?;
            let mut stmt = conn.prepare(
                r#"
                SELECT rl.id, rl.proxy_id, p.name AS proxy_name, p.type AS proxy_type,
                  p.host AS proxy_host, p.port AS proxy_port, rl.target_host, rl.target_port,
                  rl.success, rl.response_time, rl.error_message, rl.result_type, rl.created_at
                FROM request_logs rl
                LEFT JOIN proxies p ON p.id = rl.proxy_id
                WHERE rl.id > ?
                ORDER BY rl.id DESC
                LIMIT ? OFFSET ?
                "#,
            )?;
            let rows = stmt.query_map(
                params![visible_after_id, page_size, offset],
                traffic_log_from_row,
            )?;
            (total, rows.collect::<rusqlite::Result<Vec<_>>>()?)
        };

        Ok((items, total))
    }

    pub fn clear_traffic_logs(&self) -> Result<i64> {
        let conn = self.connection()?;
        let visible_after_id = traffic_log_visible_after_id(&conn)?;
        let (visible_count, max_visible_id): (i64, Option<i64>) = conn.query_row(
            "SELECT COUNT(*), MAX(id) FROM request_logs WHERE id > ?",
            params![visible_after_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let next_visible_after_id = max_visible_id.unwrap_or(visible_after_id);
        conn.execute(
            r#"
            INSERT OR REPLACE INTO settings (key, value)
            VALUES (?, ?)
            "#,
            params![
                TRAFFIC_LOG_VISIBLE_AFTER_ID_KEY,
                next_visible_after_id.to_string()
            ],
        )?;
        Ok(visible_count)
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
            for i in 0..column_count {
                let value = sql_value_to_json(row, i)?;
                map.insert(column_names[i].clone(), value);
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
            "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let avg: Option<f64> = conn.query_row(
            r#"
            SELECT AVG(response_time)
            FROM request_logs
            WHERE success = 1 AND created_at >= datetime('now', '-24 hours')
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
    let proxy_port = env::var("PROXY_PORT")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(5678);
    Map::from_iter([
        ("proxy_port".to_string(), json!(proxy_port)),
        ("periodic_test_interval".to_string(), json!(3 * 60 * 1000)),
        ("probe_recovery_interval".to_string(), json!(60 * 1000)),
        ("probe_concurrency".to_string(), json!(8)),
        ("dns_refresh_interval".to_string(), json!(5 * 60 * 1000)),
        ("background_run".to_string(), json!(false)),
        ("start_minimized".to_string(), json!(false)),
        ("log_retention_days".to_string(), json!(7)),
        ("stats_retention_days".to_string(), json!(30)),
        ("pool_max_size".to_string(), json!(50)),
        ("pool_idle_timeout".to_string(), json!(30000)),
        ("pool_wait_timeout".to_string(), json!(10000)),
        ("circuit_failure_threshold".to_string(), json!(5)),
        ("circuit_timeout".to_string(), json!(60000)),
        ("circuit_half_open_attempts".to_string(), json!(2)),
        ("health_check_interval".to_string(), json!(30000)),
        ("health_degrade_threshold".to_string(), json!(0.5)),
        ("health_recover_threshold".to_string(), json!(0.8)),
        ("failfast_enabled".to_string(), json!(true)),
        ("failfast_max_attempts".to_string(), json!(3)),
        ("failfast_attempt_timeout".to_string(), json!(10000)),
        ("failfast_total_timeout".to_string(), json!(30000)),
        (
            "algorithm_weights".to_string(),
            json!({
                "responseTime": 0.30,
                "successRate": 0.25,
                "connections": 0.20,
                "stability": 0.15,
                "recentPerf": 0.10
            }),
        ),
    ])
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
        bandwidth_bps: row.get("bandwidth_bps")?,
        bandwidth_test_time: row.get("bandwidth_test_time")?,
        test_url: row.get("test_url")?,
        test_timeout: row.get("test_timeout")?,
        current_weight: row.get("current_weight")?,
        score: None,
        active_connections: None,
        recent_total: None,
        recent_success: None,
        recent_fails: None,
        avg_success_rt: None,
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

fn traffic_log_visible_after_id(conn: &Connection) -> Result<i64> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?",
            params![TRAFFIC_LOG_VISIBLE_AFTER_ID_KEY],
            |row| row.get(0),
        )
        .optional()?;
    match value {
        Some(value) => value.parse::<i64>().with_context(|| {
            format!("{TRAFFIC_LOG_VISIBLE_AFTER_ID_KEY} 不是有效的日志游标: {value}")
        }),
        None => Ok(0),
    }
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
    for domain in domains {
        let domain = domain.trim().to_lowercase();
        if !domain.is_empty() {
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

fn validate_proxy_input(input: &ProxyInput) -> Result<()> {
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
    Ok(())
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

fn parse_bool_setting_value(raw: &str) -> Value {
    Value::Bool(raw == "1" || raw.eq_ignore_ascii_case("true"))
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
    use super::Database;
    use crate::models::{ProxyGroupInput, ProxyInput};
    use std::sync::{Mutex, OnceLock};

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
    fn traffic_log_search_and_clear_keep_stats() {
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
        db.log_request(
            Some(matched.id),
            "www.tzswater.com",
            443,
            false,
            None,
            Some("timeout"),
            "health_failure",
        )
        .unwrap();
        db.log_request(
            Some(other.id),
            "10.68.201.207",
            30333,
            true,
            Some(101),
            None,
            "health_success",
        )
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
        assert_eq!(overview["totalRequests"].as_i64(), Some(2));
        assert_eq!(overview["successRequests"].as_i64(), Some(1));
        assert_eq!(overview["failedRequests"].as_i64(), Some(1));

        db.log_request(
            Some(matched.id),
            "next.example.com",
            80,
            true,
            Some(88),
            None,
            "health_success",
        )
        .unwrap();
        let (items, total) = db.traffic_logs(1, 25, None).unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].target_host.as_deref(), Some("next.example.com"));

        let _ = std::fs::remove_dir_all(&data_dir);
        std::env::remove_var("DATA_DIR");
    }
}
