//! 桌宠助手：唤醒时使驾不在前台，就在屏幕右下角浮出一个透明、置顶的小窗（pet.html），
//! 显示皮肤人物与对话气泡。它只是视图：对话 / 语音 / 朗读仍由主窗口的全局助手完成，
//! 两边通过 pet://state（主 → 桌宠）与 pet://cmd（桌宠 → 主）事件同步。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "pet";
const WIDTH: f64 = 340.0;
const HEIGHT: f64 = 500.0;
const MARGIN: f64 = 8.0;

/// 唤出桌宠那一刻的前台窗口（用户正在看的东西）；截图提问时作为说明附上
static SUMMON_FG: Mutex<String> = Mutex::new(String::new());

/// 桌宠里可交互的区域（窗口内 CSS 像素：x, y, w, h），其余透明部分让鼠标穿透到下面的程序
static REGIONS: Mutex<Vec<[f64; 4]>> = Mutex::new(Vec::new());
static WATCHING: AtomicBool = AtomicBool::new(false);

/// 按鼠标位置切换穿透：在可交互区域上接收点击，否则穿透
fn watch_cursor(app: AppHandle) {
    if WATCHING.swap(true, Ordering::SeqCst) { return; }
    std::thread::spawn(move || {
        let mut ignoring: Option<bool> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let Some(w) = app.get_webview_window(LABEL) else { break };
            if !w.is_visible().unwrap_or(false) { ignoring = None; continue; }
            let (Ok(cur), Ok(pos), Ok(scale)) = (app.cursor_position(), w.outer_position(), w.scale_factor()) else { continue };
            let (x, y) = ((cur.x - pos.x as f64) / scale, (cur.y - pos.y as f64) / scale);
            let over = REGIONS.lock().unwrap_or_else(|p| p.into_inner()).iter()
                .any(|r| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3]);
            if ignoring != Some(!over) {
                ignoring = Some(!over);
                let _ = w.set_ignore_cursor_events(!over);
            }
        }
        WATCHING.store(false, Ordering::SeqCst);
    });
}

#[tauri::command]
pub fn pet_regions(regions: Vec<[f64; 4]>) {
    *REGIONS.lock().unwrap_or_else(|p| p.into_inner()) = regions.into_iter().take(16).collect();
}

pub fn summon_foreground() -> String {
    SUMMON_FG.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn show(app: &AppHandle, focus: bool) -> Result<(), String> {
    let fg = crate::desktop::foreground_window(std::process::id());
    if !fg.is_empty() { *SUMMON_FG.lock().unwrap_or_else(|p| p.into_inner()) = fg; }
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.show();
        let _ = w.set_always_on_top(true);
        if focus { let _ = w.set_focus(); }
        watch_cursor(app.clone());
        let _ = app.emit("pet://shown", ());
        return Ok(());
    }
    let monitor = app.get_webview_window("main")
        .and_then(|w| w.primary_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let (x, y) = match &monitor {
        Some(m) => {
            let scale = m.scale_factor().max(0.5);
            let wa = m.work_area();
            let right = (wa.position.x as f64 + wa.size.width as f64) / scale;
            let bottom = (wa.position.y as f64 + wa.size.height as f64) / scale;
            (right - WIDTH - MARGIN, bottom - HEIGHT)
        }
        None => (40.0, 40.0),
    };
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("pet.html".into()))
        .title("使驾助手")
        .inner_size(WIDTH, HEIGHT)
        .position(x, y)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(focus)
        .shadow(false)
        .additional_browser_args(crate::WEBVIEW_ARGS)
        .build()
        .map_err(|e| format!("桌宠窗口创建失败: {e}"))?;
    watch_cursor(app.clone());
    Ok(())
}

pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) { let _ = w.hide(); }
    let _ = app.emit("pet://hidden", ());
}

#[tauri::command]
pub fn pet_show(app: AppHandle, focus: Option<bool>) -> Result<(), String> { show(&app, focus.unwrap_or(false)) }

#[tauri::command]
pub fn pet_hide(app: AppHandle) { hide(&app) }

/// 桌宠里点「打开使驾」：收起桌宠，主窗口到前台
#[tauri::command]
pub fn pet_open_main(app: AppHandle) {
    hide(&app);
    crate::raise_main_window(&app);
}
