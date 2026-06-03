use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRecord {
    pub id: i64,
    pub name: String,
    #[serde(rename = "type")]
    pub proxy_type: String,
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
    pub status: Option<String>,
    pub last_test: Option<String>,
    pub response_time: Option<i64>,
    pub success_count: i64,
    pub fail_count: i64,
    pub priority: i64,
    pub enabled: i64,
    pub skip_cert_verify: i64,
    pub bandwidth_bps: Option<i64>,
    pub bandwidth_test_time: Option<String>,
    pub test_url: Option<String>,
    pub test_timeout: Option<i64>,
    pub current_weight: Option<f64>,
    #[serde(rename = "_score", skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(rename = "_activeConnections", skip_serializing_if = "Option::is_none")]
    pub active_connections: Option<i64>,
    #[serde(rename = "_recentTotal", skip_serializing_if = "Option::is_none")]
    pub recent_total: Option<i64>,
    #[serde(rename = "_recentSuccess", skip_serializing_if = "Option::is_none")]
    pub recent_success: Option<i64>,
    #[serde(rename = "_recentFails", skip_serializing_if = "Option::is_none")]
    pub recent_fails: Option<i64>,
    #[serde(rename = "_avgSuccRt", skip_serializing_if = "Option::is_none")]
    pub avg_success_rt: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyInput {
    pub name: String,
    #[serde(rename = "type")]
    pub proxy_type: String,
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
    pub enabled: Option<i64>,
    pub test_url: Option<String>,
    pub test_timeout: Option<i64>,
    pub skip_cert_verify: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsMapping {
    pub id: i64,
    pub domain: String,
    pub ip: String,
    pub description: Option<String>,
    pub enabled: i64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DnsInput {
    pub domain: String,
    pub ip: String,
    pub description: Option<String>,
    pub enabled: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyGroupDomain {
    pub id: i64,
    pub group_id: i64,
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyGroupMember {
    pub proxy_id: i64,
    pub name: String,
    #[serde(rename = "type")]
    pub proxy_type: String,
    pub host: String,
    pub port: i64,
    pub status: Option<String>,
    pub enabled: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyGroup {
    pub id: i64,
    pub name: String,
    pub is_default: i64,
    pub enabled: i64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub domains: Vec<ProxyGroupDomain>,
    pub members: Vec<ProxyGroupMember>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyGroupInput {
    pub name: Option<String>,
    pub domains: Option<Vec<String>>,
    pub proxy_ids: Option<Vec<i64>>,
    pub is_default: Option<i64>,
    pub enabled: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub success: bool,
    #[serde(rename = "responseTime")]
    pub response_time: i64,
    #[serde(rename = "statusCode", skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrafficLog {
    pub id: i64,
    pub proxy_id: Option<i64>,
    pub proxy_name: Option<String>,
    pub proxy_type: Option<String>,
    pub proxy_host: Option<String>,
    pub proxy_port: Option<i64>,
    pub target_host: Option<String>,
    pub target_port: Option<i64>,
    pub success: i64,
    pub response_time: Option<i64>,
    pub error_message: Option<String>,
    pub result_type: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Value,
    pub timestamp: i64,
}
