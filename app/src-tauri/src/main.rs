#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod acp;
mod agents;
mod coding_mcp;
mod commands;
mod db;
mod engine;
mod fsops;
mod mcp;
mod manager;
mod models;
mod child_job;
mod remote_mcp;
mod remote_oauth;
mod schedule;
mod node_rt;
mod setup;
mod skins;

use std::sync::Arc;

use tauri::Manager;

/// 日志初始化：--debug 时 info 级别写文件+控制台，否则仅 warn 到控制台。
fn init_logger(debug: bool) {
    use log::{LevelFilter, Log, Metadata, Record};
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::Mutex;

    struct FileLogger {
        file: Option<Mutex<std::fs::File>>,
        stderr: bool,
        level: LevelFilter,
    }
    impl Log for FileLogger {
        fn enabled(&self, m: &Metadata) -> bool {
            m.level() <= self.level
        }
        fn log(&self, r: &Record) {
            if !self.enabled(r.metadata()) {
                return;
            }
            let ts = chrono::Local::now().format("%m-%d %H:%M:%S");
            let line = format!("[{ts} {}] {}: {}", r.level(), r.target(), r.args());
            if let Some(f) = &self.file {
                if let Ok(mut g) = f.lock() {
                    let _ = writeln!(g, "{line}");
                }
            }
            if self.stderr {
                eprintln!("{line}");
            }
        }
        fn flush(&self) {}
    }

    // 文件日志常开（Info 级）：无论是否 --debug 都能排查问题；--debug 额外输出 stderr
    let file = {
        let dir = std::env::var("APPDATA")
            .map(|a| std::path::PathBuf::from(a).join("com.shidrive.desktop").join("logs"))
            .unwrap_or_default();
        let _ = std::fs::create_dir_all(&dir);
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("shidrive.log"))
            .ok()
            .map(Mutex::new)
    };
    let _ = log::set_boxed_logger(Box::new(FileLogger {
        file,
        stderr: debug,
        level: LevelFilter::Info,
    }));
    log::set_max_level(LevelFilter::Info);
}

/// 退出前清理：停外部编程服务（含隧道）、断开全部 ACP 适配器（进程树终止）。
/// Job Object 兜底保证任何退出路径（含强杀）最终不残留子进程；这里做的是
/// 优雅路径——服务日志落"已停止"、pending 请求被正确拒绝、数据库正常关闭。
fn graceful_exit(app: &tauri::AppHandle) {
    if let Some(remote) = app.try_state::<crate::remote_mcp::RemoteManager>() {
        remote.stop();
    }
    if let Some(mgr) = app.try_state::<std::sync::Arc<crate::manager::AgentManager>>() {
        let conns = mgr.connection_ids();
        for id in conns {
            tauri::async_runtime::block_on(mgr.disconnect(&id));
        }
    }
    // 最后显式终结子进程 Job（工作流 shell、隧道残留等；本机实测
    // kill-on-close 标志读回不符，不依赖句柄关闭语义）
    crate::child_job::terminate_all();
    app.exit(0);
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn main() {
    // 独立模式：shidrive.exe --coding-mcp [--port N] [--root DIR] [--token T] [--bind ADDR]
    let argv: Vec<String> = std::env::args().collect();
    if argv.iter().any(|a| a == "--coding-mcp") {
        std::process::exit(coding_mcp::run(&argv));
    }
    // --debug：输出调试面板 + 日志写入文件（%APPDATA%\com.shidrive.desktop\logs\shidrive.log）
    let debug_mode = argv.iter().any(|a| a == "--debug" || a == "--debug=true");
    init_logger(debug_mode);
    std::env::set_var("SHIDRIVE_DEBUG", if debug_mode { "1" } else { "0" });

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            use tauri::{
                menu::{Menu, MenuItem},
                tray::{TrayIconBuilder, TrayIconEvent},
            };
            if std::env::var("SHIDRIVE_DEBUG").as_deref() == Ok("1") {
                #[cfg(debug_assertions)]
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.open_devtools();
                }
            }
            // 托盘常驻：左键恢复主窗，右键菜单 打开主页面 / 退出
            let open = MenuItem::with_id(app, "open", "打开主页面", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            // 托盘用独立的 32px 原生渲染图标：默认图标是 128px，被系统缩到
            // 16/20px 会产生锯齿；32px 原生绘制在小尺寸下边缘干净得多
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))
                .ok()
                .or_else(|| app.default_window_icon().cloned());
            let mut tray = TrayIconBuilder::with_id("main-tray")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "quit" => graceful_exit(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                });
            if let Some(icon) = tray_icon {
                tray = tray.icon(icon);
            }
            tray.build(app)?;

            let data_dir = app.path().app_data_dir().expect("no app data dir");
            std::fs::create_dir_all(&data_dir).ok();
            let db_path = data_dir.join("shidrive.db");
            let db = Arc::new(db::Db::open(&db_path).map_err(|e| format!("打开数据库失败: {e}"))?);

            let handle = app.handle().clone();
            let agents = Arc::new(manager::AgentManager::new(handle, db.clone()));

            let engine = Arc::new(engine::Engine::new(app.handle().clone(), db.clone(), agents.clone()));
            engine.start();

            // MCP server for shared context + skills
            mcp::start(app.handle().clone(), db.clone(), engine.clone());

            app.manage(db);
            app.manage(agents);
            app.manage(engine);
            app.manage(crate::remote_mcp::RemoteManager::new());
            app.manage(std::sync::Arc::new(crate::remote_oauth::OAuthState::new()));
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭即隐藏到托盘，保持后台运行（MCP / 定时工作流不中断）
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::projects_list,
            commands::projects_create,
            commands::projects_update,
            commands::projects_delete,
            commands::contexts_list,
            commands::contexts_create,
            commands::contexts_update,
            commands::contexts_delete,
            commands::context_entries_list,
            commands::context_entry_add,
            commands::context_entry_update,
            commands::context_entry_delete,
            commands::context_commits_list,
            commands::binding_get,
            commands::binding_unbind,
            commands::acp_status,
            commands::acp_connect,
            commands::acp_disconnect,
            commands::acp_session_new,
            commands::acp_prompt,
            commands::acp_cancel,
            commands::acp_set_mode,
            commands::acp_set_config_option,
            commands::acp_respond_permission,
            commands::acp_respond_elicitation,
            commands::acp_sessions_list,
            commands::acp_session_bind,
            commands::bindings_all,
            commands::binding_set_title,
            commands::binding_set_model,
            commands::workflow_move,
            commands::contexts_all,
            commands::active_runs,
            commands::fs_list,
            commands::fs_read,
            commands::fs_write,
            commands::fs_create_file,
            commands::fs_create_dir,
            commands::fs_rename,
            commands::fs_delete,
            commands::fs_open_explorer,
            commands::fs_open_terminal,
            commands::fs_open_cmd,
            commands::fs_open_vscode,
            commands::fs_open_default,
            commands::fs_copy_to_clipboard,
            commands::fs_paste_from_clipboard,
            commands::fs_desktop_dir,
            commands::chat_store_get,
            commands::chat_store_set,
            commands::chat_store_delete,
            commands::workflows_list,
            commands::workflow_create,
            commands::workflow_update,
            commands::workflow_delete,
            commands::workflow_run,
            commands::workflow_stop,
            commands::runs_list,
            commands::settings_get,
            commands::settings_set,
            commands::setup_status,
            commands::remote_mcp_start,
            commands::remote_mcp_stop,
            commands::remote_mcp_status,
            commands::remote_grants_list,
            commands::remote_grant_create,
            commands::remote_grant_revoke,
            commands::remote_grant_delete,
            commands::remote_grant_rotate_token,
            commands::remote_grant_token,
            commands::remote_grant_pause,
            commands::remote_grant_resume,
            commands::remote_timer_set,
            commands::system_after_action,
            commands::remote_oauth_pending_list,
            commands::remote_oauth_decide,
            commands::remote_oauth_tokens_list,
            commands::remote_oauth_token_revoke,
            commands::remote_cloudflared_install,
            commands::remote_cloudflared_status,
            commands::remote_cloudflared_set_path,
            commands::remote_tunnel_start,
            commands::remote_tunnel_stop,
            commands::skin_import,
            commands::skins_list,
            commands::skin_asset_data,
            commands::clipboard_write_text,
            commands::clipboard_read_text,
            commands::ui_log,
            commands::data_export,
            commands::data_import,
            commands::node_status,
            commands::node_download,
            commands::agent_config_get,
            commands::agent_config_set,
            commands::agents_registry,
            commands::agents_enabled_get,
            commands::agents_enabled_set,
            commands::agents_bootstrap,
            commands::agents_uninstall,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
