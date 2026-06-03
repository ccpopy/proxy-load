use std::{
    env,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::{
    database::Database,
    models::ServerEvent,
    proxy::{self, ProxyRuntime},
};

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub events: broadcast::Sender<ServerEvent>,
    pub started_at: i64,
    pub api_port: u16,
    pub proxy_port: u16,
    pub proxy_runtime: Arc<ProxyRuntime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
    pub api_port: u16,
    pub proxy_port: u16,
    pub database_path: String,
    pub started_at: i64,
}

impl AppState {
    pub fn bootstrap() -> Result<Self> {
        let db = Database::open()?;
        let (events, _) = broadcast::channel(256);
        let started_at = now_millis();
        let api_port = env::var("PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(3333);
        let advanced = db.load_advanced_config()?;
        let proxy_port = advanced
            .get("proxy_port")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(5678);
        let proxy_runtime = Arc::new(ProxyRuntime::new(db.clone(), events.clone()));

        let runtime_for_proxy = proxy_runtime.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = proxy::serve(runtime_for_proxy, proxy_port).await {
                eprintln!("代理服务启动失败: {error:#}");
            }
        });

        Ok(Self {
            db,
            events,
            started_at,
            api_port,
            proxy_port,
            proxy_runtime,
        })
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
            api_port: self.api_port,
            proxy_port: self.proxy_port,
            database_path: self.db.path().display().to_string(),
            started_at: self.started_at,
        }
    }
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
