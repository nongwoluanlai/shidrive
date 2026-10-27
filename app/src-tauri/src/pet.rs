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

/// 穿透 / 恢复点击。Tauri 的 set_ignore_cursor_events 只给顶层窗口加
/// WS_EX_TRANSPARENT，而鼠标命中的是 WebView2 的子窗口，所以必须连所有后代一起改。
#[cfg(windows)]
fn set_click_through(w: &tauri::WebviewWindow, ignore: bool) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumChildWindows(h: isize, f: unsafe extern "system" fn(isize, isize) -> i32, l: isize) -> i32;
        fn GetWindowLongW(h: isize, i: i32) -> i32;
        fn SetWindowLongW(h: isize, i: i32, v: i32) -> i32;
    }
    unsafe extern "system" fn collect(h: isize, l: isize) -> i32 {
        unsafe { (*(l as *mut Vec<isize>)).push(h); 1 }
    }
    let Ok(root) = w.hwnd() else { return };
    let root = root.0 as isize;
    let mut hs = vec![root];
    unsafe { EnumChildWindows(root, collect, &mut hs as *mut Vec<isize> as isize) };
    for h in hs {
        unsafe {
            let ex = GetWindowLongW(h, -20);
            // WS_EX_LAYERED | WS_EX_TRANSPARENT
            let new = if ignore { ex | 0x8_0020 } else { ex & !0x8_0020 };
            if new != ex { SetWindowLongW(h, -20, new); }
        }
    }
}

#[cfg(not(windows))]
fn set_click_through(w: &tauri::WebviewWindow, ignore: bool) {
    let _ = w.set_ignore_cursor_events(ignore);
}

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
                set_click_through(&w, !over);
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
    if !fg.is_empty() { *SUMMON_FG.lock().unwrap_or_else(|p| p.into_inner()) = fg.clone(); }
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
    let fg_desc = fg.clone();
    log::info!("[pet] 唤出桌宠窗口（focus={focus}，唤出时前台：{fg_desc}）");
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
        .map_err(|e| { log::warn!("[pet] 桌宠窗口创建失败: {e}"); format!("桌宠窗口创建失败: {e}") })?;
    log::info!("[pet] 桌宠窗口已创建");
    watch_cursor(app.clone());
    Ok(())
}

pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) { let _ = w.hide(); }
    let _ = app.emit("pet://hidden", ());
}

/// 必须是 async：同步命令在主线程执行，而在主线程里同步创建 WebView 会卡死事件循环
#[tauri::command]
pub async fn pet_show(app: AppHandle, focus: Option<bool>) -> Result<(), String> { show(&app, focus.unwrap_or(false)) }

#[tauri::command]
pub fn pet_hide(app: AppHandle) { hide(&app) }

/// 桌宠里点「打开使驾」：收起桌宠，主窗口到前台
#[tauri::command]
pub fn pet_open_main(app: AppHandle) {
    hide(&app);
    crate::raise_main_window(&app);
}
