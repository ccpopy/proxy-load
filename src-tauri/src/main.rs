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

use anyhow::{anyhow, Result};
use models::ServerEvent;
use serde_json::Value;
use state::{AppState, ServiceInfo};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Emitter, Manager, WindowEvent,
};
use tokio::time::{self, Duration, MissedTickBehavior};

const BACKGROUND_RUN_KEY: &str = "background_run";
const START_MINIMIZED_KEY: &str = "start_minimized";
const TRAY_ID: &str = "main-tray";

#[tauri::command]
fn get_service_info(state: tauri::State<'_, Arc<AppState>>) -> ServiceInfo {
    state.service_info()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        show_main_window(app);
    }));

    let app = builder
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

            let tray_ready = match setup_tray(app, quit_flag.clone(), app_state.clone()) {
                Ok(()) => {
                    spawn_tray_tooltip_updates(app.handle().clone(), app_state.clone());
                    true
                }
                Err(error) => {
                    eprintln!("系统托盘初始化失败: {error:#}");
                    false
                }
            };

            if let Some(window) = app.get_webview_window("main") {
                // 窗口在配置里默认隐藏（visible=false），这里按“启动最小化”开关决定是否展示，
                // 既能实现启动即进托盘，也能避免默认启动时的白屏闪烁。
                if tray_ready && read_flag(&app_state, START_MINIMIZED_KEY) {
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
                        if tray_ready
                            && !close_flag.load(Ordering::SeqCst)
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

fn setup_tray(
    app: &App,
    quit_flag: Arc<AtomicBool>,
    app_state: Arc<AppState>,
) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text("show", "显示主界面")
        .separator()
        .text("quit", "退出")
        .build()?;

    let tray_state = app_state.clone();
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("代理状态加载中")
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
        .on_tray_icon_event(move |tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => show_main_window(tray.app_handle()),
            TrayIconEvent::Enter { .. } => {
                let app_handle = tray.app_handle().clone();
                let state = tray_state.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = update_tray_tooltip(&app_handle, &state).await {
                        eprintln!("系统托盘状态刷新失败: {error:#}");
                    }
                });
            }
            _ => {}
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn spawn_tray_tooltip_updates(app_handle: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = update_tray_tooltip(&app_handle, &state).await {
            eprintln!("系统托盘状态初始化失败: {error:#}");
        }

        let mut events = state.events.subscribe();
        let mut ticker = time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await;
        let mut request_refresh_pending = false;
        loop {
            tokio::select! {
                _ = ticker.tick(), if request_refresh_pending => {
                    request_refresh_pending = false;
                    if let Err(error) = update_tray_tooltip(&app_handle, &state).await {
                        eprintln!("系统托盘状态刷新失败: {error:#}");
                    }
                }
                message = events.recv() => match message {
                    Ok(event) if event.event_type == "request_logged" => {
                        request_refresh_pending = true;
                    }
                    Ok(event) if should_refresh_tray_tooltip(&event) => {
                        if let Err(error) = update_tray_tooltip(&app_handle, &state).await {
                            eprintln!("系统托盘状态刷新失败: {error:#}");
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        request_refresh_pending = true;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });
}

fn should_refresh_tray_tooltip(event: &ServerEvent) -> bool {
    matches!(
        event.event_type.as_str(),
        "proxy_service_status_changed"
            | "traffic_logs_cleared"
            | "proxy_testing"
            | "proxy_tested"
            | "proxy_created"
            | "proxy_updated"
            | "proxy_deleted"
            | "dns_mapping_updated"
    )
}

async fn update_tray_tooltip(app: &AppHandle, state: &AppState) -> Result<()> {
    let tooltip = build_tray_tooltip(state).await?;
    let tray = app
        .tray_by_id(TRAY_ID)
        .ok_or_else(|| anyhow!("找不到系统托盘图标: {TRAY_ID}"))?;
    tray.set_tooltip(Some(tooltip))?;
    Ok(())
}

async fn build_tray_tooltip(state: &AppState) -> Result<String> {
    let service = state.proxy_runtime.service_status().await;
    let overview = state.db.overview(state.uptime_seconds())?;
    let active_proxies = json_i64(&overview, "activeProxies")?;
    let total_requests = json_i64(&overview, "totalRequests")?;
    let avg_response_time = json_i64(&overview, "avgResponseTime")?;

    if !service.running {
        let label = if service.state == "starting" {
            "代理启动中"
        } else {
            "代理未运行"
        };
        return Ok(format!("{label}\n端口 {}", service.port));
    }

    let avg_label = if total_requests > 0 && avg_response_time > 0 {
        format!("平均建连 {avg_response_time}ms")
    } else {
        "平均建连 --".to_string()
    };

    Ok(format!(
        "代理运行中 · 在线 {}\n{}",
        active_proxies, avg_label
    ))
}

fn json_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("系统状态缺少字段: {key}"))
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
    #[cfg(target_os = "windows")]
    match commands::run_windows_update_helper_from_args() {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("Windows 更新辅助进程失败: {error:#}");
            std::process::exit(1);
        }
    }

    run();
}
