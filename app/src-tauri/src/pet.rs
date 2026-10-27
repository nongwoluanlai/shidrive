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

/// 按鼠标位置切换穿透：在可交互区域（气泡 / 输入条 / 头像）上接收点击，其余整窗穿透。
/// tao 的顶层 WS_EX_LAYERED|WS_EX_TRANSPARENT 实测可让真实点击完全穿到下面的程序
/// （曾尝试把样式加到 WebView2 子窗口：直接把渲染打成黑框，不可行）。
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
            // 左键按住（拖动窗口 / 正在点按）时不要切到穿透，否则拖拽会中途断掉
            #[cfg(windows)]
            let lmb_down = {
                #[link(name = "user32")]
                unsafe extern "system" { fn GetAsyncKeyState(vk: i32) -> i16; }
                unsafe { GetAsyncKeyState(0x01) < 0 }
            };
            #[cfg(not(windows))]
            let lmb_down = false;
            let want = if lmb_down { over } else { !over };
            if ignoring != Some(want) {
                ignoring = Some(want);
                let _ = w.set_ignore_cursor_events(want);
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
