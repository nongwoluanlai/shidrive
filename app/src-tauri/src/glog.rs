//! 全局日志：应用级事件（定时工作流触发 / 错过计划时间 / 执行后销毁 / 定时发送等）。
//!
//! 内存环形缓冲（最多 MAX 条，重启清空——"上次启动以来"的漏执行在启动后首个调度
//! tick 里补记），每条新日志同时通过 `glog://entry` 事件推给前端标题栏的日志面板。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const EVT_GLOG: &str = "glog://entry";
const MAX: usize = 500;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: u64,
    pub time: String,
    /// info | warn | error | success
    pub level: String,
    /// workflow | schedule | chat | app ...
    pub category: String,
    pub message: String,
}

struct Store {
    next: u64,
    items: VecDeque<LogEntry>,
}

static STORE: Mutex<Store> = Mutex::new(Store { next: 1, items: VecDeque::new() });
static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

pub fn push(level: &str, category: &str, message: impl Into<String>) -> LogEntry {
    let message: String = message.into();
    let entry = {
        let mut s = STORE.lock().unwrap_or_else(|p| p.into_inner());
        let e = LogEntry {
            id: s.next,
            time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            level: level.to_string(),
            category: category.to_string(),
            message: message.chars().take(2000).collect(),
        };
        s.next += 1;
        s.items.push_back(e.clone());
        while s.items.len() > MAX {
            s.items.pop_front();
        }
        e
    };
    log::info!("[glog/{category}] {message}");
    if let Some(app) = APP.get() {
        let _ = app.emit(EVT_GLOG, &entry);
    }
    entry
}

pub fn info(category: &str, message: impl Into<String>) {
    push("info", category, message);
}
pub fn warn(category: &str, message: impl Into<String>) {
    push("warn", category, message);
}

pub fn list() -> Vec<LogEntry> {
    STORE.lock().unwrap_or_else(|p| p.into_inner()).items.iter().cloned().collect()
}

pub fn clear() {
    STORE.lock().unwrap_or_else(|p| p.into_inner()).items.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_is_bounded_and_ordered() {
        clear();
        for i in 0..(MAX + 20) {
            push("info", "test", format!("m{i}"));
        }
        let v = list();
        assert_eq!(v.len(), MAX);
        assert_eq!(v.last().unwrap().message, format!("m{}", MAX + 19));
        assert!(v.windows(2).all(|w| w[0].id < w[1].id));
        clear();
        assert!(list().is_empty());
    }
}
