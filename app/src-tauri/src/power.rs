//! Windows 电源与锁屏会话监听：
//! 1. 监听 `WM_WTSSESSION_CHANGE`（锁屏 / 解锁），通过 `system://lock-changed` 通知前端释放/恢复音频占用，
//!    避免锁屏后麦克风或 AudioContext 阻止电脑睡眠及后台误唤醒；
//! 2. 「会话或任务运行中则阻止睡眠」：专用后台线程维护 `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`
//!    （仅阻止系统睡眠，允许显示器息屏），通过 RAII `SleepGuard` 引用计数精确开关。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, OnceLock};

use serde_json::json;
use tauri::{AppHandle, Emitter};

pub const SETTING_PREVENT_SLEEP: &str = "system.prevent_sleep";
pub const EVT_LOCK_CHANGED: &str = "system://lock-changed";

static SCREEN_LOCKED: AtomicBool = AtomicBool::new(false);
static LOCK_APP: OnceLock<AppHandle> = OnceLock::new();

pub fn is_screen_locked() -> bool {
    SCREEN_LOCKED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn system_is_locked() -> bool {
    is_screen_locked()
}

fn set_screen_locked(locked: bool) {
    let prev = SCREEN_LOCKED.swap(locked, Ordering::SeqCst);
    if prev == locked {
        return;
    }
    log::info!(
        "[power] Windows 会话{}",
        if locked { "已锁屏，通知释放音频占用" } else { "已解锁" }
    );
    if let Some(app) = LOCK_APP.get() {
        let _ = app.emit(EVT_LOCK_CHANGED, json!({ "locked": locked }));
    }
}

/// 启动 Windows 锁屏/解锁监听器（仅启动一次，专用消息窗口线程）。
pub fn start_lock_monitor(app: AppHandle) {
    let _ = LOCK_APP.set(app);
    // 开机自启时可能本来就处于锁屏：WTS 只在"切换"时通知，不会补发当前状态，
    // 所以先探测一次，否则唤醒词 / 朗读会在锁屏背后重新武装
    #[cfg(windows)]
    {
        SCREEN_LOCKED.store(win_lock::probe_locked(), Ordering::SeqCst);
    }
    win_lock::spawn();
}

// ---------------------------------------------------------------- 阻止系统睡眠

/// 跨线程引用计数的系统睡眠抑制器。
/// Windows 的 `SetThreadExecutionState` 是按线程生效的，因此由一个常驻 OS 线程独占调用，
/// 避免在 tokio 工作线程上设置后无法在原线程清除导致系统永远无法睡眠。
pub struct SleepInhibitor {
    enabled: AtomicBool,
    active: AtomicUsize,
    inhibiting: AtomicBool,
    tx: Option<mpsc::Sender<bool>>,
    /// 串行化「改计数 + 同步状态」
    sync_lock: std::sync::Mutex<()>,
}

impl SleepInhibitor {
    pub fn new(enabled: bool) -> Arc<Self> {
        let tx = Self::spawn_worker();
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            active: AtomicUsize::new(0),
            inhibiting: AtomicBool::new(false),
            tx,
            sync_lock: std::sync::Mutex::new(()),
        })
    }

    #[cfg(test)]
    fn new_for_test(enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            active: AtomicUsize::new(0),
            inhibiting: AtomicBool::new(false),
            tx: None,
            sync_lock: std::sync::Mutex::new(()),
        })
    }

    fn spawn_worker() -> Option<mpsc::Sender<bool>> {
        #[cfg(windows)]
        {
            let (tx, rx) = mpsc::channel::<bool>();
            let _ = std::thread::Builder::new()
                .name("shidrive-sleep-inhibitor".into())
                .spawn(move || {
                    while let Ok(prevent) = rx.recv() {
                        win_sleep::apply(prevent);
                    }
                    // 退出时务必释放阻止睡眠状态
                    win_sleep::apply(false);
                });
            Some(tx)
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// 「读计数 → 决定 → 写状态」必须整体串行：否则并发 acquire / drop 可能交错成
    /// 计数已归零但状态仍是"阻止睡眠"（或反过来），前者会让电脑一直不休眠。
    fn sync(&self) {
        let _serial = self.sync_lock.lock().unwrap_or_else(|p| p.into_inner());
        self.sync_locked();
    }

    /// 调用方必须已持有 sync_lock
    fn sync_locked(&self) {
        let want = self.enabled.load(Ordering::SeqCst) && self.active.load(Ordering::SeqCst) > 0;
        let prev = self.inhibiting.swap(want, Ordering::SeqCst);
        if prev != want {
            log::debug!(
                "[power] 阻止系统睡眠：{}",
                if want { "开启（有运行中的会话/任务）" } else { "关闭" }
            );
            if let Some(tx) = &self.tx {
                let _ = tx.send(want);
            }
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
        self.sync();
    }

    pub fn is_inhibiting(&self) -> bool {
        self.inhibiting.load(Ordering::SeqCst)
    }

    /// 退出前强制释放阻止睡眠状态。
    pub fn release_all(&self) {
        self.enabled.store(false, Ordering::SeqCst);
        self.sync();
    }

    /// 持有期间计入一次活跃会话/任务，Drop 时自动释放。
    pub fn acquire(self: &Arc<Self>) -> SleepGuard {
        {
            let _serial = self.sync_lock.lock().unwrap_or_else(|p| p.into_inner());
            self.active.fetch_add(1, Ordering::SeqCst);
            self.sync_locked();
        }
        SleepGuard { inhibitor: self.clone() }
    }
}

pub struct SleepGuard {
    inhibitor: Arc<SleepInhibitor>,
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        let i = &self.inhibitor;
        let _serial = i.sync_lock.lock().unwrap_or_else(|p| p.into_inner());
        i.active.fetch_sub(1, Ordering::SeqCst);
        i.sync_locked();
    }
}

// ---------------------------------------------------------------- Win32 FFI

#[cfg(windows)]
mod win_sleep {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetThreadExecutionState(es_flags: u32) -> u32;
    }

    pub fn apply(prevent: bool) {
        let flags = if prevent { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS };
        unsafe {
            let _ = SetThreadExecutionState(flags);
        }
    }
}

#[cfg(windows)]
mod win_lock {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::Once;

    const HWND_MESSAGE: *mut c_void = -3isize as *mut c_void;
    const NOTIFY_FOR_THIS_SESSION: u32 = 0;
    const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
    const WTS_SESSION_LOCK: usize = 0x7;
    const WTS_SESSION_UNLOCK: usize = 0x8;

    #[repr(C)]
    struct WNDCLASSW {
        style: u32,
        lpfn_wnd_proc: Option<unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: *mut c_void,
        h_icon: *mut c_void,
        h_cursor: *mut c_void,
        hbr_background: *mut c_void,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
    }

    #[repr(C)]
    struct POINT {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct MSG {
        hwnd: *mut c_void,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        pt: POINT,
        l_private: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(lp_wnd_class: *const WNDCLASSW) -> u16;
        fn CreateWindowExW(
            dw_ex_style: u32,
            lp_class_name: *const u16,
            lp_window_name: *const u16,
            dw_style: u32,
            x: i32,
            y: i32,
            n_width: i32,
            n_height: i32,
            h_wnd_parent: *mut c_void,
            h_menu: *mut c_void,
            h_instance: *mut c_void,
            lp_param: *mut c_void,
        ) -> *mut c_void;
        fn DefWindowProcW(hwnd: *mut c_void, msg: u32, w_param: usize, l_param: isize) -> isize;
        fn GetMessageW(lp_msg: *mut MSG, hwnd: *mut c_void, msg_filter_min: u32, msg_filter_max: u32) -> i32;
        fn TranslateMessage(lp_msg: *const MSG) -> i32;
        fn DispatchMessageW(lp_msg: *const MSG) -> isize;
    }

    #[link(name = "wtsapi32")]
    unsafe extern "system" {
        fn WTSRegisterSessionNotification(hwnd: *mut c_void, dw_flags: u32) -> i32;
        fn WTSUnRegisterSessionNotification(hwnd: *mut c_void) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> *mut c_void;
        fn CloseDesktop(h: *mut c_void) -> i32;
    }

    /// 当前是否处于锁屏：拿不到输入桌面（或拿不到其切换权限）即视为锁屏。
    /// 探测失败时保守返回 false（宁可多武装一次，也不要永久静音）。
    pub fn probe_locked() -> bool {
        const DESKTOP_SWITCHDESKTOP: u32 = 0x0100;
        unsafe {
            let h = OpenInputDesktop(0, 0, DESKTOP_SWITCHDESKTOP);
            if h.is_null() {
                // 锁屏时 OpenInputDesktop 会失败
                return true;
            }
            CloseDesktop(h);
            false
        }
    }

    unsafe extern "system" fn wnd_proc(hwnd: *mut c_void, msg: u32, w_param: usize, l_param: isize) -> isize {
        if msg == WM_WTSSESSION_CHANGE {
            match w_param {
                WTS_SESSION_LOCK => super::set_screen_locked(true),
                WTS_SESSION_UNLOCK => super::set_screen_locked(false),
                _ => {}
            }
        }
        unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn spawn() {
        static STARTED: Once = Once::new();
        STARTED.call_once(|| {
            let _ = std::thread::Builder::new()
                .name("shidrive-lock-monitor".into())
                .spawn(|| unsafe {
                    let hinst = GetModuleHandleW(ptr::null());
                    let cls = wide("ShiDriveLockMonitorWnd");
                    let wc = WNDCLASSW {
                        style: 0,
                        lpfn_wnd_proc: Some(wnd_proc),
                        cb_cls_extra: 0,
                        cb_wnd_extra: 0,
                        h_instance: hinst,
                        h_icon: ptr::null_mut(),
                        h_cursor: ptr::null_mut(),
                        hbr_background: ptr::null_mut(),
                        lpsz_menu_name: ptr::null(),
                        lpsz_class_name: cls.as_ptr(),
                    };
                    RegisterClassW(&wc);
                    let hwnd = CreateWindowExW(
                        0,
                        cls.as_ptr(),
                        cls.as_ptr(),
                        0,
                        0,
                        0,
                        0,
                        0,
                        HWND_MESSAGE,
                        ptr::null_mut(),
                        hinst,
                        ptr::null_mut(),
                    );
                    if hwnd.is_null() {
                        log::warn!("[power] 创建锁屏监听消息窗口失败");
                        return;
                    }
                    if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) == 0 {
                        log::warn!("[power] WTSRegisterSessionNotification 注册失败");
                        return;
                    }
                    let mut msg: MSG = std::mem::zeroed();
                    while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    let _ = WTSUnRegisterSessionNotification(hwnd);
                });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_inhibitor_tracks_guards_and_toggle() {
        let inh = SleepInhibitor::new_for_test(true);
        assert!(!inh.is_inhibiting());

        let g1 = inh.acquire();
        assert!(inh.is_inhibiting());

        let g2 = inh.acquire();
        assert!(inh.is_inhibiting());
        drop(g1);
        assert!(inh.is_inhibiting(), "still one active guard");

        // Disabling while active releases immediately
        inh.set_enabled(false);
        assert!(!inh.is_inhibiting());

        // Re-enabling while still active re-engages immediately
        inh.set_enabled(true);
        assert!(inh.is_inhibiting());

        drop(g2);
        assert!(!inh.is_inhibiting());

        // Disabled inhibitor ignores new guards
        inh.set_enabled(false);
        let _g3 = inh.acquire();
        assert!(!inh.is_inhibiting());
    }
}
