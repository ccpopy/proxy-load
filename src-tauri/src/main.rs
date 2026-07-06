#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod database;
mod models;
mod proxy;
mod proxy_tester;
mod state;
mod version;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use state::{AppState, ServiceInfo};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Emitter, Manager, WindowEvent,
};

const BACKGROUND_RUN_KEY: &str = "background_run";
const START_MINIMIZED_KEY: &str = "start_minimized";

#[tauri::command]
fn get_service_info(state: tauri::State<'_, Arc<AppState>>) -> ServiceInfo {
    state.service_info()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_state = Arc::new(AppState::bootstrap()?);
            app.manage(app_state.clone());

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

            // 标记“用户主动退出”：托盘退出菜单会置位，避免退出流程被后台运行逻辑拦截。
            let quit_flag = Arc::new(AtomicBool::new(false));

            if let Err(error) = setup_tray(app, quit_flag.clone()) {
                eprintln!("系统托盘初始化失败: {error:#}");
            }

            if let Some(window) = app.get_webview_window("main") {
                // 窗口在配置里默认隐藏（visible=false），这里按“启动最小化”开关决定是否展示，
                // 既能实现启动即进托盘，也能避免默认启动时的白屏闪烁。
                if read_flag(&app_state, START_MINIMIZED_KEY) {
                    let _ = window.hide();
                    #[cfg(not(target_os = "macos"))]
                    let _ = window.set_skip_taskbar(true);
                } else {
                    let _ = window.show();
                    let _ = window.set_focus();
                }

                let close_state = app_state.clone();
                let close_flag = quit_flag.clone();
                let close_window = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        if !close_flag.load(Ordering::SeqCst)
                            && read_flag(&close_state, BACKGROUND_RUN_KEY)
                        {
                            // 后台运行：拦截关闭，仅把窗口隐藏到托盘，进程继续运行。
                            api.prevent_close();
                            let _ = close_window.hide();
                            #[cfg(not(target_os = "macos"))]
                            let _ = close_window.set_skip_taskbar(true);
                        }
                    }
                });
            }

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
            commands::refresh_dns_mapping,
            commands::test_urls,
            commands::traffic_logs,
            commands::clear_traffic_logs,
            commands::version_info,
            commands::check_for_updates,
            commands::install_update
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用启动失败");

    app.run(|_app_handle, _event| {
        // macOS：从程序坞点击图标（applicationShouldHandleReopen）时重新显示主窗口。
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            show_main_window(_app_handle);
        }
    });
}

fn setup_tray(app: &App, quit_flag: Arc<AtomicBool>) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text("show", "显示主界面")
        .separator()
        .text("quit", "退出")
        .build()?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("代理管理系统")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => {
                quit_flag.store(true, Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(not(target_os = "macos"))]
        let _ = window.set_skip_taskbar(false);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn read_flag(state: &AppState, key: &str) -> bool {
    state
        .db
        .settings_map()
        .ok()
        .and_then(|map| map.get(key).cloned())
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn main() {
    run();
}
