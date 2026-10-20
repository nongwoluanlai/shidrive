//! 使驾自有通知弹窗：替代 PowerShell + WinForms 托盘气泡。
//!
//! 每条通知是一个无边框、置顶、不抢焦点、不进任务栏的小窗口（标签 `toast-<n>`），
//! 从主显示器工作区右下角向上堆叠；内容页为独立的 `toast.html` 入口，经
//! `toast_info` 取文案与主题色，用户点击 / 关闭 / 超时后经 `toast_action` 回报。
//! 点击行为：Session = 唤出主窗口并跳到对应会话；Open = 资源管理器打开；Url = 浏览器打开。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 92.0;
const GAP: f64 = 10.0;
const MARGIN: f64 = 14.0;
const MAX_VISIBLE: usize = 4;

#[derive(Clone, Debug, PartialEq)]
pub enum ToastClick {
    None,
    Session { context_id: String, agent_type: String },
    Open(String),
    Url(String),
}

pub struct Toast {
    pub title: String,
    pub body: String,
    pub click: ToastClick,
    pub sound: bool,
    pub timeout_secs: u32,
}

struct Entry {
    title: String,
    body: String,
    click: ToastClick,
    timeout_secs: u32,
    slot: usize,
    seq: u64,
}

#[derive(Default)]
pub struct ToastState {
    next: AtomicU64,
    items: Mutex<HashMap<String, Entry>>,
}

impl ToastState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// 系统提示音（不经 PowerShell）。
#[cfg(windows)]
pub fn play_sound() {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBeep(utype: u32) -> i32;
    }
    // MB_ICONASTERISK：与旧气泡节点 SystemSounds.Asterisk 一致
    unsafe {
        MessageBeep(0x40);
    }
}

#[cfg(not(windows))]
pub fn play_sound() {}

/// 聊天「执行后动作 → 提示音」：保持旧版两声短促提示（kernel32 Beep，阻塞，放后台线程）。
pub fn play_chime() {
    std::thread::spawn(|| {
        #[cfg(windows)]
        {
            #[link(name = "kernel32")]
            extern "system" {
                fn Beep(freq: u32, duration: u32) -> i32;
            }
            unsafe {
                if Beep(880, 180) == 0 {
                    play_sound();
                    return;
                }
                Beep(1175, 240);
            }
        }
    });
}

fn first_free_slot(used: &[usize]) -> usize {
    (0..).find(|i| !used.contains(i)).unwrap_or(0)
}

/// 主题色（桌面端保存的 ui.theme_vars）：弹窗与网页端共用。
pub fn theme_vars(app: &AppHandle) -> Value {
    let db = app.state::<std::sync::Arc<crate::db::Db>>();
    db.get_setting("ui.theme_vars")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn parse_hex(c: &str) -> Option<(u8, u8, u8)> {
    let h = c.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// 显示一条通知（立即返回）。最多同时显示 4 条，超出时关闭最早的一条。
pub fn show(app: &AppHandle, t: Toast) -> Result<(), String> {
    if t.sound {
        play_sound();
    }
    let state = app.state::<ToastState>();
    let seq = state.next.fetch_add(1, Ordering::SeqCst) + 1;
    let label = format!("toast-{seq}");
    let timeout_secs = t.timeout_secs.clamp(3, 120);
    let (slot, evict) = {
        let mut items = state.items.lock().unwrap_or_else(|p| p.into_inner());
        let mut evict = None;
        if items.len() >= MAX_VISIBLE {
            if let Some((k, _)) = items.iter().min_by_key(|(_, e)| e.seq) {
                let k = k.clone();
                items.remove(&k);
                evict = Some(k);
            }
        }
        let used: Vec<usize> = items.values().map(|e| e.slot).collect();
        let slot = first_free_slot(&used);
        items.insert(
            label.clone(),
            Entry { title: t.title.clone(), body: t.body.clone(), click: t.click.clone(), timeout_secs, slot, seq },
        );
        (slot, evict)
    };
    if let Some(old) = evict {
        if let Some(w) = app.get_webview_window(&old) {
            let _ = w.destroy();
        }
    }

    // 位置：主显示器工作区（不含任务栏）右下角，按槽位向上堆叠
    let monitor = app
        .get_webview_window("main")
        .and_then(|w| w.primary_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let (x, y) = match &monitor {
        Some(m) => {
            let scale = m.scale_factor().max(0.5);
            let wa = m.work_area();
            let right = (wa.position.x as f64 + wa.size.width as f64) / scale;
            let bottom = (wa.position.y as f64 + wa.size.height as f64) / scale;
            (right - WIDTH - MARGIN, bottom - MARGIN - (slot as f64 + 1.0) * HEIGHT - slot as f64 * GAP)
        }
        None => (40.0, 40.0 + slot as f64 * (HEIGHT + GAP)),
    };
    let bg = theme_vars(app)
        .get("--bg-panel")
        .and_then(Value::as_str)
        .and_then(parse_hex)
        .unwrap_or((20, 23, 34));

    let built = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("toast.html".into()))
        .title("ShiDrive")
        .inner_size(WIDTH, HEIGHT)
        .position(x, y)
        .decorations(false)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .shadow(true)
        .background_color(tauri::window::Color(bg.0, bg.1, bg.2, 255))
        .additional_browser_args(crate::WEBVIEW_ARGS)
        .build();
    if let Err(e) = built {
        state.items.lock().unwrap_or_else(|p| p.into_inner()).remove(&label);
        return Err(format!("通知窗口创建失败: {e}"));
    }
    // 兜底：页面未能加载 / 前端计时失效时，到点后强制回收
    let app2 = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(timeout_secs as u64 + 60));
        close(&app2, &label);
    });
    Ok(())
}

fn close(app: &AppHandle, label: &str) -> Option<Entry> {
    let entry = app.state::<ToastState>().items.lock().unwrap_or_else(|p| p.into_inner()).remove(label);
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.destroy();
    }
    entry
}

fn run_click(app: &AppHandle, click: &ToastClick) {
    match click {
        ToastClick::None => {}
        ToastClick::Session { context_id, agent_type } => {
            crate::raise_main_window(app);
            let db = app.state::<std::sync::Arc<crate::db::Db>>();
            let project_id = db.get_context_row(context_id).map(|c| c.project_id);
            let _ = app.emit(
                "notify://clicked",
                json!({ "contextId": context_id, "agentType": agent_type, "projectId": project_id }),
            );
        }
        ToastClick::Open(target) => {
            let mut c = std::process::Command::new("explorer.exe");
            c.arg(target);
            if let Err(e) = c.spawn() {
                log::warn!("通知点击打开失败: {e}");
            }
        }
        ToastClick::Url(url) => {
            if let Err(e) = open::that_detached(url) {
                log::warn!("通知点击打开链接失败: {e}");
            }
        }
    }
}

/// 通知页初始化：按窗口标签取文案、超时与主题色。
#[tauri::command]
pub async fn toast_info(app: AppHandle, window: tauri::WebviewWindow) -> Result<Value, String> {
    let label = window.label().to_string();
    let items = app.state::<ToastState>();
    let items = items.items.lock().unwrap_or_else(|p| p.into_inner());
    let e = items.get(&label).ok_or("通知已失效")?;
    Ok(json!({
        "title": e.title,
        "body": e.body,
        "timeoutMs": e.timeout_secs as u64 * 1000,
        "clickable": e.click != ToastClick::None,
        "theme": theme_vars(&app),
    }))
}

/// 通知页回报：click = 执行点击行为并关闭；其它 = 仅关闭。
#[tauri::command]
pub async fn toast_action(app: AppHandle, window: tauri::WebviewWindow, action: String) -> Result<(), String> {
    let label = window.label().to_string();
    if !label.starts_with("toast-") {
        return Err("无效的通知窗口".into());
    }
    if let Some(entry) = close(&app, &label) {
        if action == "click" {
            run_click(&app, &entry.click);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn webview_args_match_main_window() {
        // 主窗口（tauri.conf.json）与通知窗口的 WebView2 参数必须一致，否则通知窗口创建失败
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["app"]["windows"][0]["additionalBrowserArgs"].as_str(), Some(crate::WEBVIEW_ARGS));
        assert!(crate::WEBVIEW_ARGS.contains("--autoplay-policy=no-user-gesture-required"));
    }

    use super::*;

    #[test]
    fn slots_fill_gaps() {
        assert_eq!(first_free_slot(&[]), 0);
        assert_eq!(first_free_slot(&[0, 1]), 2);
        assert_eq!(first_free_slot(&[1, 2]), 0);
        assert_eq!(first_free_slot(&[0, 2]), 1);
    }

    #[test]
    fn hex_parse() {
        assert_eq!(parse_hex("#141722"), Some((0x14, 0x17, 0x22)));
        assert_eq!(parse_hex("rgba(0,0,0,.5)"), None);
        assert_eq!(parse_hex("#fff"), None);
    }
}
