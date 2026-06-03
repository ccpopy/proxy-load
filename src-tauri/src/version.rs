use serde_json::json;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn version_info() -> serde_json::Value {
    json!({
        "version": VERSION,
        "name": env!("CARGO_PKG_NAME"),
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "author": env!("CARGO_PKG_AUTHORS"),
        "buildTime": chrono::Utc::now().to_rfc3339(),
        "environment": if cfg!(debug_assertions) { "development" } else { "production" },
        "runtime": "rust-tauri",
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH
    })
}
