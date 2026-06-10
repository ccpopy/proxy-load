#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod database;
mod models;
mod proxy;
mod proxy_tester;
mod state;
mod version;

use std::sync::Arc;

use state::{AppState, ServiceInfo};
use tauri::{Emitter, Manager};

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

            let app_handle = app.handle().clone();
            let mut events = app_state.events.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            if let Err(error) = app_handle.emit("server-event", event) {
                                eprintln!("应用事件发送失败: {error:#}");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_service_info,
            commands::list_proxies,
            commands::get_proxy,
            commands::create_proxy,
            commands::update_proxy,
            commands::delete_proxy,
            commands::update_proxy_priority,
            commands::update_proxy_priorities,
            commands::test_proxy,
            commands::proxy_service_status,
            commands::list_proxy_groups,
            commands::create_proxy_group,
            commands::update_proxy_group,
            commands::delete_proxy_group,
            commands::get_settings,
            commands::save_settings,
            commands::get_advanced_config,
            commands::save_advanced_config,
            commands::reset_advanced_config,
            commands::export_selected_config,
            commands::import_config_file,
            commands::stats_overview,
            commands::stats_hourly,
            commands::stats_proxy_usage,
            commands::stats_targets,
            commands::stats_failed_targets,
            commands::stats_circuit_breakers,
            commands::stats_connection_pools,
            commands::list_dns_mappings,
            commands::create_dns_mapping,
            commands::update_dns_mapping,
            commands::delete_dns_mapping,
            commands::toggle_dns_mapping,
            commands::test_urls,
            commands::traffic_logs,
            commands::clear_traffic_logs,
            commands::version_info,
            commands::check_for_updates,
            commands::install_update
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}

fn main() {
    run();
}
