mod api;
mod database;
mod models;
mod proxy;
mod proxy_tester;
mod state;
mod version;

use std::sync::Arc;

use state::{AppState, ServiceInfo};
use tauri::Manager;

#[tauri::command]
fn get_service_info(state: tauri::State<'_, Arc<AppState>>) -> ServiceInfo {
    state.service_info()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_state = Arc::new(AppState::bootstrap()?);
            let managed_state = app_state.clone();
            app.manage(managed_state);

            tauri::async_runtime::spawn(async move {
                if let Err(error) = api::serve(app_state.clone()).await {
                    eprintln!("管理 API 启动失败: {error:#}");
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_service_info])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}

fn main() {
    run();
}
