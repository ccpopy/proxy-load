use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{sync::broadcast, time};
use url::Url;

use crate::{
    database::Database,
    models::{ProxyRecord, ServerEvent, TestResult},
    proxy::{self, ProxyRuntime},
    proxy_tester,
};

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub events: broadcast::Sender<ServerEvent>,
    pub started_at: i64,
    pub proxy_port: u16,
    pub proxy_runtime: Arc<ProxyRuntime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
    pub proxy_port: u16,
    pub database_path: String,
    pub started_at: i64,
}

impl AppState {
    pub fn bootstrap() -> Result<Self> {
        let db = Database::open()?;
        let (events, _) = broadcast::channel(256);
        let started_at = now_millis();
        let advanced = db.load_advanced_config()?;
        let proxy_port = advanced
            .get("proxy_port")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(5678);
        let proxy_runtime = Arc::new(ProxyRuntime::new(db.clone(), events.clone(), proxy_port));

        let runtime_for_proxy = proxy_runtime.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = proxy::serve(runtime_for_proxy, proxy_port).await {
                eprintln!("代理服务启动失败: {error:#}");
            }
        });

        let state = Self {
            db,
            events,
            started_at,
            proxy_port,
            proxy_runtime,
        };
        state.spawn_periodic_proxy_tests();
        Ok(state)
    }

    pub fn uptime_seconds(&self) -> i64 {
        ((now_millis() - self.started_at) / 1000).max(0)
    }

    pub fn emit(&self, event_type: impl Into<String>, data: Value) {
        let _ = self.events.send(ServerEvent {
            event_type: event_type.into(),
            data,
            timestamp: now_millis(),
        });
    }

    pub fn service_info(&self) -> ServiceInfo {
        ServiceInfo {
            proxy_port: self.proxy_port,
            database_path: self.db.path().display().to_string(),
            started_at: self.started_at,
        }
    }

    pub async fn test_proxy_by_id(&self, id: i64) -> Result<TestResult> {
        let proxy = self
            .db
            .get_proxy(id)?
            .ok_or_else(|| anyhow!("代理不存在"))?;
        self.test_proxy_record(proxy).await
    }

    fn spawn_periodic_proxy_tests(&self) {
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            state.periodic_proxy_tests().await;
        });
    }

    async fn periodic_proxy_tests(self) {
        loop {
            if let Err(error) = self.test_enabled_proxies().await {
                eprintln!("定期代理测试失败: {error:#}");
            }

            let interval = match self.periodic_test_interval() {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("定期代理测试停止: {error:#}");
                    break;
                }
            };
            time::sleep(interval).await;
        }
    }

    async fn test_enabled_proxies(&self) -> Result<()> {
        let proxies = self.db.list_enabled_proxies()?;
        for proxy in proxies {
            if let Err(error) = self.test_proxy_record(proxy).await {
                eprintln!("代理定期测试失败: {error:#}");
            }
        }
        Ok(())
    }

    async fn test_proxy_record(&self, proxy: ProxyRecord) -> Result<TestResult> {
        let settings = self.db.settings_map()?;
        let global_url = settings
            .get("test_url")
            .cloned()
            .unwrap_or_else(|| "https://cms.zjzwfw.gov.cn/favicon.ico".to_string());
        let global_timeout = settings
            .get("timeout")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(10)
            * 1000;
        let test_url = proxy.test_url.clone().unwrap_or(global_url);
        let timeout = proxy
            .test_timeout
            .and_then(|value| u64::try_from(value).ok())
            .map(|value| value * 1000)
            .unwrap_or(global_timeout);
        let target = Url::parse(&test_url).with_context(|| format!("测试地址无效: {test_url}"))?;
        let target_host = target.host_str().unwrap_or_default().to_string();
        let target_port = target.port_or_known_default().unwrap_or(80);

        self.db
            .update_proxy_status(proxy.id, "testing", None, 0, 0)?;
        self.emit("proxy_testing", json!({ "id": proxy.id }));

        let result = proxy_tester::test_proxy(&proxy, &test_url, timeout).await;
        if result.success {
            self.db
                .update_proxy_status(proxy.id, "active", Some(result.response_time), 1, 0)?;
            self.db.log_request(
                Some(proxy.id),
                &target_host,
                i64::from(target_port),
                true,
                Some(result.response_time),
                None,
                "health_success",
            )?;
        } else {
            self.db
                .update_proxy_status(proxy.id, "inactive", None, 0, 1)?;
            self.db.log_request(
                Some(proxy.id),
                &target_host,
                i64::from(target_port),
                false,
                None,
                result.error.as_deref(),
                "health_failure",
            )?;
        }

        let updated = self.db.get_proxy(proxy.id)?;
        self.emit(
            "proxy_tested",
            json!({
                "proxy": updated,
                "result": result
            }),
        );
        Ok(result)
    }

    fn periodic_test_interval(&self) -> Result<Duration> {
        let config = self.db.load_advanced_config()?;
        let value = match config.get("periodic_test_interval") {
            Some(value) => value
                .as_u64()
                .ok_or_else(|| anyhow!("periodic_test_interval 必须是数字"))?,
            None => 5 * 60 * 1000,
        };
        if value == 0 {
            return Err(anyhow!("periodic_test_interval 必须大于 0"));
        }
        Ok(Duration::from_millis(value))
    }
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
