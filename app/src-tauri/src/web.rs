//! 网站接入：内置轻量 Web 服务（默认端口 5165），让其它设备经用户自备的反代隧道
//! （frp / Cloudflare Tunnel / Nginx 等）或局域网，在浏览器里使用使驾的核心能力：
//! 选择项目 / 上下文、ACP Agent 对话、查看与运行工作流、目录树与轻量文件预览。
//!
//! 设计要点：
//! - 与远程 MCP 相同的 std 线程服务模型（非阻塞 accept + stop_flag、每连接线程、读写超时、并发上限）；
//! - 访客码登录：成功后发放随机会话令牌（HttpOnly Cookie，仅内存保存其 SHA-256，7 天过期），
//!   失败按 IP 与全局双重限流；修改访客码 / 一键注销会使所有已登录访客失效；
//! - 写操作要求自定义请求头 `X-SD-Web: 1`（跨站表单与无预检请求无法携带），且不返回任何 CORS 允许头；
//! - 实时推送走 SSE（/api/events）：后端事件总线转发 acp:// 与 wf:// 事件，桌面端与网页端同步；
//! - 文件接口只允许访问已登记项目的根目录之内（canonicalize 后前缀校验），只读；
//! - 静态资源来自打包进 exe 的前端产物（web.html + /assets），不额外占用磁盘与依赖。

use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{Emitter, Listener, Manager};

use crate::db::Db;
use crate::engine::Engine;
use crate::manager::AgentManager;

pub const DEFAULT_PORT: u16 = 5165;
const KEY_ENABLED: &str = "web.enabled";
const KEY_PORT: &str = "web.port";
const KEY_PUBLIC_URL: &str = "web.public_url";
const KEY_GUEST_CODE: &str = "web.guest_code";
const KEY_LAN: &str = "web.lan";

const COOKIE: &str = "sd_web";
const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_SESSIONS: usize = 64;
const MAX_CONNS: usize = 96;
const MAX_SSE: usize = 24;
const MAX_BODY: usize = 2 * 1024 * 1024;
const FAIL_WINDOW: Duration = Duration::from_secs(10 * 60);
const FAIL_PER_IP: usize = 5;
const FAIL_GLOBAL: usize = 30;
const MAX_PREVIEW: u64 = 2 * 1024 * 1024;
const MAX_RAW: u64 = 10 * 1024 * 1024;
const MIN_CODE_LEN: usize = 4;

/// 转发给网页端的后端事件（与桌面前端监听的同名事件一致）
const FORWARD_EVENTS: &[&str] = &[
    "acp://update",
    "acp://binding-status",
    "acp://session-ready",
    "acp://permission",
    "acp://requests-cancelled",
    "acp://status",
    "wf://status",
    "wf://log",
    "wf://deleted",
    "chat://prompt",
];

static OPEN_CONNS: AtomicUsize = AtomicUsize::new(0);
struct ConnDec;
impl Drop for ConnDec {
    fn drop(&mut self) {
        OPEN_CONNS.fetch_sub(1, Ordering::SeqCst);
    }
}

fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn sha256_hex(s: &str) -> String {
    crate::remote_mcp::sha256_hex(s)
}

/// 常量时间比较（访客码 / 令牌哈希）
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff: usize = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

fn random_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// 生成易读的访客码（去掉 0/O/1/I/L 等易混字符），8 位
pub fn generate_guest_code() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let seed = uuid::Uuid::new_v4();
    seed.as_bytes()
        .iter()
        .take(8)
        .map(|b| CHARS[(*b as usize) % CHARS.len()] as char)
        .collect()
}

// ------------------------------------------------------------------ config

#[derive(Debug, Clone, serde::Serialize)]
pub struct WebConfig {
    pub enabled: bool,
    pub port: u16,
    pub public_url: String,
    pub guest_code: String,
    pub lan: bool,
}

pub fn load_config(db: &Db) -> WebConfig {
    let get = |k: &str| db.get_setting(k).ok().flatten().unwrap_or_default();
    WebConfig {
        enabled: get(KEY_ENABLED) == "1",
        port: get(KEY_PORT).trim().parse::<u16>().ok().filter(|p| *p > 0).unwrap_or(DEFAULT_PORT),
        public_url: get(KEY_PUBLIC_URL).trim().trim_end_matches('/').to_string(),
        guest_code: get(KEY_GUEST_CODE),
        lan: get(KEY_LAN) == "1",
    }
}

/// 开放地址规范化：允许填域名 / IP:端口 / 完整 URL；无协议时默认 http://
pub fn normalize_public_url(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() {
        return String::new();
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("http://{s}")
    }
}

// ------------------------------------------------------------------ hub

struct Session {
    created: Instant,
    last_seen: Instant,
    ip: String,
    ua: String,
}

struct Client {
    id: u64,
    tx: SyncSender<String>,
}

/// 会话、限流、SSE 客户端与运行日志：服务重启不清空会话（访客无需重复登录），
/// 修改访客码 / 一键注销 / 应用退出时清空。
pub struct Hub {
    sessions: Mutex<HashMap<String, Session>>,
    fails: Mutex<HashMap<String, VecDeque<Instant>>>,
    global_fails: Mutex<VecDeque<Instant>>,
    clients: Mutex<Vec<Client>>,
    next_client: AtomicU64,
    logs: Mutex<VecDeque<String>>,
    /// 待处理的权限请求（requestId → 事件负载）：网页端连接晚于请求时也能看到
    permissions: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Hub {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            fails: Mutex::new(HashMap::new()),
            global_fails: Mutex::new(VecDeque::new()),
            clients: Mutex::new(Vec::new()),
            next_client: AtomicU64::new(1),
            logs: Mutex::new(VecDeque::new()),
            permissions: Mutex::new(HashMap::new()),
        }
    }

    fn log(&self, line: impl Into<String>) {
        let line = format!("[{}] {}", chrono::Local::now().format("%m-%d %H:%M:%S"), line.into());
        let mut g = self.logs.lock().unwrap_or_else(|p| p.into_inner());
        g.push_back(line);
        while g.len() > 200 {
            g.pop_front();
        }
    }

    fn session_valid(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        let key = sha256_hex(token);
        let mut g = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        match g.get_mut(&key) {
            Some(s) if s.created.elapsed() < SESSION_TTL => {
                s.last_seen = Instant::now();
                true
            }
            Some(_) => {
                g.remove(&key);
                false
            }
            None => false,
        }
    }

    fn create_session(&self, ip: &str, ua: &str) -> String {
        let token = random_token();
        let mut g = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        g.retain(|_, s| s.created.elapsed() < SESSION_TTL);
        if g.len() >= MAX_SESSIONS {
            if let Some(oldest) = g.iter().min_by_key(|(_, s)| s.last_seen).map(|(k, _)| k.clone()) {
                g.remove(&oldest);
            }
        }
        g.insert(
            sha256_hex(&token),
            Session { created: Instant::now(), last_seen: Instant::now(), ip: ip.to_string(), ua: ua.chars().take(120).collect() },
        );
        token
    }

    fn remove_session(&self, token: &str) {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner()).remove(&sha256_hex(token));
    }

    pub fn revoke_all(&self) -> usize {
        let n = {
            let mut g = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let n = g.len();
            g.clear();
            n
        };
        // SSE 循环在下一次唤醒时发现会话失效并断开；这里主动推一条让客户端立刻回到登录页
        self.broadcast("web://logout", "{}");
        n
    }

    fn session_list(&self) -> Vec<Value> {
        let g = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<_> = g
            .values()
            .filter(|s| s.created.elapsed() < SESSION_TTL)
            .map(|s| {
                json!({
                    "ip": s.ip, "ua": s.ua,
                    "loginSecsAgo": s.created.elapsed().as_secs(),
                    "activeSecsAgo": s.last_seen.elapsed().as_secs(),
                })
            })
            .collect();
        v.sort_by_key(|x| x["activeSecsAgo"].as_u64().unwrap_or(0));
        v
    }

    /// 登录限流：返回需等待的秒数（None = 允许尝试）
    fn login_blocked(&self, ip: &str) -> Option<u64> {
        let now = Instant::now();
        {
            let mut g = self.global_fails.lock().unwrap_or_else(|p| p.into_inner());
            while g.front().is_some_and(|t| now.duration_since(*t) > FAIL_WINDOW) {
                g.pop_front();
            }
            if g.len() >= FAIL_GLOBAL {
                let wait = FAIL_WINDOW.saturating_sub(now.duration_since(*g.front().unwrap()));
                return Some(wait.as_secs().max(1));
            }
        }
        let mut f = self.fails.lock().unwrap_or_else(|p| p.into_inner());
        f.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) <= FAIL_WINDOW));
        if let Some(q) = f.get_mut(ip) {
            while q.front().is_some_and(|t| now.duration_since(*t) > FAIL_WINDOW) {
                q.pop_front();
            }
            if q.len() >= FAIL_PER_IP {
                let wait = FAIL_WINDOW.saturating_sub(now.duration_since(*q.front().unwrap()));
                return Some(wait.as_secs().max(1));
            }
        }
        None
    }

    fn login_failed(&self, ip: &str) -> usize {
        let now = Instant::now();
        self.global_fails.lock().unwrap_or_else(|p| p.into_inner()).push_back(now);
        let mut f = self.fails.lock().unwrap_or_else(|p| p.into_inner());
        let q = f.entry(ip.to_string()).or_default();
        q.push_back(now);
        FAIL_PER_IP.saturating_sub(q.len())
    }

    fn login_ok(&self, ip: &str) {
        self.fails.lock().unwrap_or_else(|p| p.into_inner()).remove(ip);
    }

    fn add_client(&self) -> Option<(u64, Receiver<String>)> {
        let mut g = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        if g.len() >= MAX_SSE {
            return None;
        }
        let (tx, rx) = sync_channel::<String>(1024);
        let id = self.next_client.fetch_add(1, Ordering::SeqCst);
        g.push(Client { id, tx });
        Some((id, rx))
    }

    fn remove_client(&self, id: u64) {
        self.clients.lock().unwrap_or_else(|p| p.into_inner()).retain(|c| c.id != id);
    }

    fn client_count(&self) -> usize {
        self.clients.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// 广播一条 SSE 事件。队列满（客户端太慢）直接断开它，网页端重连后会重新拉取状态。
    fn broadcast(&self, event: &str, payload: &str) {
        let mut g = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        if g.is_empty() {
            return;
        }
        let frame = sse_frame(event, payload);
        g.retain(|c| match c.tx.try_send(frame.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        });
    }

    fn on_event(&self, event: &str, payload: &str) {
        match event {
            "acp://permission" => {
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    if let Some(id) = v.get("requestId").and_then(Value::as_str) {
                        let mut g = self.permissions.lock().unwrap_or_else(|p| p.into_inner());
                        g.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(3600));
                        g.insert(id.to_string(), (Instant::now(), v.clone()));
                    }
                }
            }
            "acp://requests-cancelled" => {
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    if let Some(ids) = v.get("requestIds").and_then(Value::as_array) {
                        let mut g = self.permissions.lock().unwrap_or_else(|p| p.into_inner());
                        for id in ids.iter().filter_map(Value::as_str) {
                            g.remove(id);
                        }
                    }
                }
            }
            "acp://binding-status" => {
                // 回合结束：同会话残留的权限请求已无意义
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    let status = v.get("status").and_then(Value::as_str).unwrap_or("");
                    if status != "running" {
                        if let Some(sid) = v.get("sessionId").and_then(Value::as_str) {
                            self.permissions
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .retain(|_, (_, p)| p.get("sessionId").and_then(Value::as_str) != Some(sid));
                        }
                    }
                }
            }
            _ => {}
        }
        // 工具调用的大段输入/输出（读文件、终端输出、diff）对网页端只需预览：
        // 转发前截短长字符串，减少移动端流量与解析/渲染开销
        if event == "acp://update" && payload.len() > SLIM_EVENT_BYTES {
            if let Ok(mut v) = serde_json::from_str::<Value>(payload) {
                slim_value(&mut v, SLIM_STR, 0);
                let slim = v.to_string();
                if slim.len() <= 1024 * 1024 {
                    self.broadcast(event, &slim);
                }
            }
            return;
        }
        // 单条事件过大（极端的工具输出）不转发，避免拖垮移动端；网页端可「刷新历史」补齐
        if payload.len() > 1024 * 1024 {
            return;
        }
        self.broadcast(event, payload);
    }

    fn take_permission(&self, id: &str) {
        self.permissions.lock().unwrap_or_else(|p| p.into_inner()).remove(id);
    }

    fn pending_permissions(&self) -> Vec<Value> {
        let g = self.permissions.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<_> = g.values().filter(|(t, _)| t.elapsed() < Duration::from_secs(3600)).cloned().collect();
        v.sort_by_key(|(t, _)| std::cmp::Reverse(t.elapsed()));
        v.into_iter().map(|(_, p)| p).collect()
    }
}

fn sse_frame(event: &str, payload: &str) -> String {
    let mut s = format!("event: {event}\n");
    for line in payload.split('\n') {
        s.push_str("data: ");
        s.push_str(line.trim_end_matches('\r'));
        s.push('\n');
    }
    s.push('\n');
    s
}

// ------------------------------------------------------------------ manager

struct Runtime {
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    port: u16,
    host: String,
    started_at: String,
}

pub struct WebManager {
    hub: Arc<Hub>,
    runtime: Mutex<Option<Runtime>>,
    wired: AtomicBool,
    last_error: Mutex<String>,
}

// ---------- 网页端负载瘦身 ----------

/// 超过该体积的 acp://update 才做瘦身（普通文本分片原样转发）
const SLIM_EVENT_BYTES: usize = 8 * 1024;
/// 工具输入/输出中单个字符串保留的最大字符数（网页端只做预览）
const SLIM_STR: usize = 4000;
/// 首屏默认返回的最近消息条数（其余按需「加载更早」）
const CHAT_TAIL_DEFAULT: usize = 150;

/// 递归截短 JSON 中的长字符串（按字符边界），并限制数组长度
pub(crate) fn slim_value(v: &mut Value, max: usize, depth: usize) {
    match v {
        Value::String(s) => {
            if s.len() > max {
                let total = s.chars().count();
                if total > max {
                    let mut t: String = s.chars().take(max).collect();
                    t.push_str(&format!("\n…（网页端预览已截断，共 {total} 字符）"));
                    *s = t;
                }
            }
        }
        Value::Array(a) => {
            if a.len() > 400 {
                a.truncate(400);
            }
            if depth < 12 {
                for x in a.iter_mut() {
                    slim_value(x, max, depth + 1);
                }
            }
        }
        Value::Object(o) => {
            if depth < 12 {
                for (_, x) in o.iter_mut() {
                    slim_value(x, max, depth + 1);
                }
            }
        }
        _ => {}
    }
}

/// 聊天快照 items：只瘦身工具调用部分（正文保持完整）
fn slim_items(items: &mut Value) {
    if let Value::Array(list) = items {
        for it in list.iter_mut() {
            if let Some(tools) = it.get_mut("tools") {
                slim_value(tools, SLIM_STR, 0);
            }
        }
    }
}

/// 取末尾 limit 条（limit=0 表示全部），返回 (被省略的条数)
fn keep_tail<T>(list: &mut Vec<T>, limit: usize) -> usize {
    if limit == 0 || list.len() <= limit {
        return 0;
    }
    let skip = list.len() - limit;
    list.drain(..skip);
    skip
}

fn tail_limit(raw: &str) -> usize {
    match raw.trim() {
        "" => CHAT_TAIL_DEFAULT,
        v => v.parse::<usize>().unwrap_or(CHAT_TAIL_DEFAULT),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WebStatus {
    pub running: bool,
    pub enabled: bool,
    pub port: u16,
    pub listen: String,
    pub lan: bool,
    pub public_url: String,
    pub guest_code: String,
    pub started_at: Option<String>,
    pub sessions: Vec<Value>,
    pub clients: usize,
    pub local_urls: Vec<String>,
    pub logs: Vec<String>,
    pub assets_ready: bool,
    pub last_error: String,
}

impl WebManager {
    pub fn new() -> Self {
        Self { hub: Arc::new(Hub::new()), runtime: Mutex::new(None), wired: AtomicBool::new(false), last_error: Mutex::new(String::new()) }
    }

    /// 后端事件 → SSE 转发（只注册一次）
    fn wire_events(&self, app: &tauri::AppHandle) {
        if self.wired.swap(true, Ordering::SeqCst) {
            return;
        }
        for name in FORWARD_EVENTS {
            let hub = self.hub.clone();
            let ev = name.to_string();
            app.listen_any(*name, move |e| hub.on_event(&ev, e.payload()));
        }
    }

    pub fn is_running(&self) -> bool {
        self.runtime.lock().unwrap_or_else(|p| p.into_inner()).is_some()
    }

    pub fn start(&self, app: &tauri::AppHandle, db: &Arc<Db>) -> Result<(), String> {
        let cfg = load_config(db);
        if cfg.guest_code.trim().chars().count() < MIN_CODE_LEN {
            return Err(format!("请先设置访客码（至少 {MIN_CODE_LEN} 位）"));
        }
        let mut guard = self.runtime.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            return Ok(());
        }
        self.wire_events(app);
        let host = if cfg.lan { "0.0.0.0" } else { "127.0.0.1" };
        let listener = TcpListener::bind((host, cfg.port)).map_err(|e| {
            let msg = format!("绑定 {host}:{} 失败: {e}（端口被占用或无权限，可在设置里换一个端口）", cfg.port);
            *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = msg.clone();
            msg
        })?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(Ctx { app: app.clone(), db: db.clone(), hub: self.hub.clone(), stop: stop.clone() });
        let accept_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("web-accept".into())
            .spawn(move || accept_loop(listener, ctx, accept_stop))
            .map_err(|e| e.to_string())?;
        self.hub.log(format!("服务已启动：{host}:{}", cfg.port));
        crate::glog::info("web", format!("网站接入已启动：{host}:{}", cfg.port));
        *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = String::new();
        *guard = Some(Runtime { stop, handle: Mutex::new(Some(handle)), port: cfg.port, host: host.to_string(), started_at: now_str() });
        Ok(())
    }

    pub fn stop(&self) {
        let rt = self.runtime.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(rt) = rt {
            rt.stop.store(true, Ordering::SeqCst);
            // 断开全部 SSE：清空发送端，客户端线程在下一次 recv 时退出
            self.hub.clients.lock().unwrap_or_else(|p| p.into_inner()).clear();
            if let Some(h) = rt.handle.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = h.join();
            }
            self.hub.log("服务已停止");
            crate::glog::info("web", "网站接入已停止");
        }
    }

    pub fn revoke_all(&self) -> usize {
        let n = self.hub.revoke_all();
        self.hub.log(format!("已注销全部访客会话（{n} 个）"));
        n
    }

    pub fn status(&self, app: &tauri::AppHandle, db: &Db) -> WebStatus {
        let cfg = load_config(db);
        let rt = self.runtime.lock().unwrap_or_else(|p| p.into_inner());
        let (running, port, listen, started_at) = match rt.as_ref() {
            Some(r) => (true, r.port, format!("{}:{}", r.host, r.port), Some(r.started_at.clone())),
            None => (false, cfg.port, String::new(), None),
        };
        let lan_now = rt.as_ref().map(|r| r.host == "0.0.0.0").unwrap_or(cfg.lan);
        drop(rt);
        let mut local_urls = vec![format!("http://127.0.0.1:{port}")];
        if lan_now {
            if let Some(ip) = lan_ip() {
                local_urls.push(format!("http://{ip}:{port}"));
            }
        }
        let logs = self.hub.logs.lock().unwrap_or_else(|p| p.into_inner()).iter().rev().take(60).cloned().collect();
        WebStatus {
            running,
            enabled: cfg.enabled,
            port,
            listen,
            lan: cfg.lan,
            public_url: cfg.public_url,
            guest_code: cfg.guest_code,
            started_at,
            sessions: self.hub.session_list(),
            clients: self.hub.client_count(),
            local_urls,
            logs,
            assets_ready: web_html(app).is_some(),
            last_error: self.last_error.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        }
    }

    /// 保存配置。访客码变化时注销所有访客；端口 / 局域网变化且服务在运行时自动重启服务。
    pub fn configure(
        &self,
        app: &tauri::AppHandle,
        db: &Arc<Db>,
        port: Option<u16>,
        public_url: Option<String>,
        guest_code: Option<String>,
        lan: Option<bool>,
    ) -> Result<(), String> {
        let before = load_config(db);
        if let Some(p) = port {
            if p == 0 {
                return Err("端口需为 1-65535 的整数".into());
            }
            db.set_setting(KEY_PORT, &p.to_string())?;
        }
        if let Some(u) = public_url {
            db.set_setting(KEY_PUBLIC_URL, &normalize_public_url(&u))?;
        }
        if let Some(code) = guest_code {
            let code = code.trim().to_string();
            if code.chars().count() < MIN_CODE_LEN {
                return Err(format!("访客码至少 {MIN_CODE_LEN} 位"));
            }
            if code.chars().count() > 64 {
                return Err("访客码最长 64 位".into());
            }
            if code != before.guest_code {
                db.set_setting(KEY_GUEST_CODE, &code)?;
                let n = self.hub.revoke_all();
                self.hub.log(format!("访客码已更新，已注销 {n} 个访客会话"));
            }
        }
        if let Some(l) = lan {
            db.set_setting(KEY_LAN, if l { "1" } else { "0" })?;
        }
        let after = load_config(db);
        if self.is_running() && (after.port != before.port || after.lan != before.lan) {
            self.stop();
            self.start(app, db)?;
        }
        Ok(())
    }

    pub fn set_enabled(&self, db: &Db, on: bool) -> Result<(), String> {
        db.set_setting(KEY_ENABLED, if on { "1" } else { "0" })
    }

    /// 应用启动时：开关为开则自动启动
    pub fn autostart(&self, app: &tauri::AppHandle, db: &Arc<Db>) {
        if load_config(db).enabled {
            if let Err(e) = self.start(app, db) {
                self.hub.log(format!("自动启动失败：{e}"));
                crate::glog::warn("web", format!("网站接入自动启动失败：{e}"));
            }
        }
    }
}

/// 本机局域网 IP（UDP connect 不发包，只用于取出站网卡地址）
fn lan_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        None
    } else {
        Some(ip.to_string())
    }
}

// ------------------------------------------------------------------ server

struct Ctx {
    app: tauri::AppHandle,
    db: Arc<Db>,
    hub: Arc<Hub>,
    stop: Arc<AtomicBool>,
}

fn accept_loop(listener: TcpListener, ctx: Arc<Ctx>, stop: Arc<AtomicBool>) {
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((mut stream, peer)) => {
                stream.set_nonblocking(false).ok();
                stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
                stream.set_nodelay(true).ok();
                if OPEN_CONNS.load(Ordering::SeqCst) >= MAX_CONNS {
                    let _ = respond(&mut stream, 503, "application/json", br#"{"error":"busy"}"#, "");
                    continue;
                }
                OPEN_CONNS.fetch_add(1, Ordering::SeqCst);
                let ctx = ctx.clone();
                let _ = std::thread::Builder::new().name("web-conn".into()).spawn(move || {
                    let _dec = ConnDec;
                    if let Err(e) = handle_conn(stream, peer, &ctx) {
                        log::debug!("[web] conn: {e}");
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                log::warn!("[web] accept: {e}");
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    ip: String,
}

impl Req {
    fn header(&self, k: &str) -> &str {
        self.headers.get(k).map(String::as_str).unwrap_or("")
    }
    fn q(&self, k: &str) -> String {
        self.query.get(k).cloned().unwrap_or_default()
    }
    fn cookie(&self, name: &str) -> String {
        for part in self.header("cookie").split(';') {
            if let Some((k, v)) = part.trim().split_once('=') {
                if k.trim() == name {
                    return v.trim().to_string();
                }
            }
        }
        String::new()
    }
    fn json(&self) -> Result<Value, String> {
        if self.body.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&self.body).map_err(|e| format!("请求体不是有效 JSON: {e}"))
    }
    fn https(&self) -> bool {
        self.header("x-forwarded-proto").eq_ignore_ascii_case("https") || self.header("cf-visitor").contains("https")
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_req(stream: &mut TcpStream, peer: std::net::SocketAddr) -> Result<Option<Req>, String> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 32 * 1024 {
            return Err("请求头过大".into());
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut rl = lines.next().unwrap_or_default().split_whitespace();
    let method = rl.next().unwrap_or_default().to_ascii_uppercase();
    let target = rl.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_length: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if content_length > MAX_BODY {
        let _ = respond(stream, 413, "application/json", "{\"error\":\"请求体过大\"}".as_bytes(), "");
        return Ok(None);
    }
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    let (path, qs) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    let mut query = HashMap::new();
    for kv in qs.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        query.insert(pct_decode(k), pct_decode(v));
    }
    // 真实来源 IP：仅当连接来自本机（反代 / 隧道客户端）时才信任转发头
    let mut ip = peer.ip().to_string();
    if peer.ip().is_loopback() {
        let fwd = headers
            .get("cf-connecting-ip")
            .or_else(|| headers.get("x-real-ip"))
            .cloned()
            .or_else(|| headers.get("x-forwarded-for").and_then(|v| v.split(',').next()).map(|s| s.trim().to_string()));
        if let Some(f) = fwd.filter(|s| !s.is_empty() && s.len() < 64) {
            ip = f;
        }
    }
    Ok(Some(Req { method, path: pct_decode(&path), query, headers, body, ip }))
}

const SEC_HEADERS: &str = "X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\n";

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

fn respond(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8], extra: &str) -> Result<(), String> {
    let mut head = format!("HTTP/1.1 {status} {}\r\nContent-Type: {ctype}\r\n{SEC_HEADERS}", reason(status));
    head.push_str(extra);
    if !extra.to_ascii_lowercase().contains("cache-control:") {
        head.push_str("Cache-Control: no-store\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    stream.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

fn respond_json(stream: &mut TcpStream, status: u16, v: &Value, extra: &str) -> Result<(), String> {
    let body = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    respond(stream, status, "application/json; charset=utf-8", &body, extra)
}

fn handle_conn(mut stream: TcpStream, peer: std::net::SocketAddr, ctx: &Ctx) -> Result<(), String> {
    let Some(req) = read_req(&mut stream, peer)? else { return Ok(()) };
    if ctx.stop.load(Ordering::SeqCst) {
        return respond(&mut stream, 503, "text/plain", b"stopped", "");
    }
    if req.path.starts_with("/api/") {
        return handle_api(&mut stream, &req, ctx);
    }
    if req.method != "GET" && req.method != "HEAD" {
        return respond(&mut stream, 405, "text/plain", b"method not allowed", "");
    }
    serve_static(&mut stream, &req, ctx)
}

// ------------------------------------------------------------------ static

fn web_html(app: &tauri::AppHandle) -> Option<Vec<u8>> {
    let a = app.asset_resolver().get("/web.html".to_string())?;
    let bytes = a.bytes().to_vec();
    // 资源缺失时 Tauri 会回退到桌面端 index.html：用网页端入口的挂载点识别
    if find(&bytes, b"id=\"sd-web\"").is_some() {
        Some(bytes)
    } else {
        None
    }
}

const NOT_BUILT_HTML: &str = r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>ShiDrive Web</title><body style="font-family:system-ui;padding:32px;line-height:1.7;color:#333"><h2>网页端资源未打包</h2><p>当前运行的使驾未包含网页端前端产物（开发模式请先执行 <code>pnpm build</code> 生成 dist；正式版请重新打包）。</p></body>"#;

fn serve_static(stream: &mut TcpStream, req: &Req, ctx: &Ctx) -> Result<(), String> {
    let p = req.path.as_str();
    if p == "/favicon.ico" {
        return respond(stream, 204, "image/x-icon", b"", "");
    }
    if p.starts_with("/assets/") && !p.contains("..") {
        if let Some(a) = ctx.app.asset_resolver().get(p.to_string()) {
            let mime = a.mime_type().to_string();
            // 不存在的资源会被回退成 index.html，此时按 404 处理
            if !(mime.starts_with("text/html") && !p.ends_with(".html")) {
                let body: &[u8] = if req.method == "HEAD" { b"" } else { a.bytes() };
                return respond(stream, 200, &mime, body, "Cache-Control: public, max-age=31536000, immutable\r\n");
            }
        }
        return respond(stream, 404, "text/plain", b"not found", "");
    }
    match web_html(&ctx.app) {
        Some(html) => respond(stream, 200, "text/html; charset=utf-8", if req.method == "HEAD" { b"" } else { &html }, "Cache-Control: no-cache\r\n"),
        None => respond(stream, 503, "text/html; charset=utf-8", NOT_BUILT_HTML.as_bytes(), ""),
    }
}

// ------------------------------------------------------------------ api

fn err_json(msg: impl Into<String>) -> Value {
    json!({ "error": msg.into() })
}

fn handle_api(stream: &mut TcpStream, req: &Req, ctx: &Ctx) -> Result<(), String> {
    let path = req.path.as_str();
    let token = req.cookie(COOKIE);
    let authed = ctx.hub.session_valid(&token);
    let is_post = req.method == "POST";
    if req.method != "GET" && !is_post {
        return respond_json(stream, 405, &err_json("method not allowed"), "");
    }
    // 写操作统一要求自定义头：跨站页面无法在不触发预检的情况下携带它，而本服务不响应预检
    if is_post && req.header("x-sd-web") != "1" {
        return respond_json(stream, 403, &err_json("缺少请求头"), "");
    }
    match (req.method.as_str(), path) {
        ("GET", "/api/me") => {
            return respond_json(stream, 200, &json!({ "authed": authed, "version": env!("CARGO_PKG_VERSION") }), "");
        }
        ("POST", "/api/login") => return login(stream, req, ctx),
        // 桌面端当前配色（仅颜色变量，不含敏感信息）：登录页也沿用同一主题
        ("GET", "/api/theme") => {
            return respond_json(stream, 200, &crate::toast::theme_vars(&ctx.app), "Cache-Control: no-cache\r\n");
        }
        ("POST", "/api/logout") => {
            ctx.hub.remove_session(&token);
            let cookie = format!("Set-Cookie: {COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0\r\n");
            return respond_json(stream, 200, &json!({ "ok": true }), &cookie);
        }
        _ => {}
    }
    if !authed {
        return respond_json(stream, 401, &err_json("未登录或登录已过期"), "");
    }
    if path == "/api/events" {
        return sse(stream, ctx, token);
    }
    if path == "/api/fs/raw" {
        return fs_raw(stream, req, ctx);
    }
    let result = route(req, ctx);
    match result {
        Ok(v) => respond_json(stream, 200, &v, ""),
        Err(ApiErr(code, msg)) => respond_json(stream, code, &err_json(msg), ""),
    }
}

fn login(stream: &mut TcpStream, req: &Req, ctx: &Ctx) -> Result<(), String> {
    if let Some(wait) = ctx.hub.login_blocked(&req.ip) {
        ctx.hub.log(format!("登录被限流：{}", req.ip));
        return respond_json(stream, 429, &json!({ "error": format!("尝试次数过多，请 {} 分钟后再试", (wait + 59) / 60), "retryAfter": wait }), "");
    }
    let code = req.json().ok().and_then(|v| v.get("code").and_then(Value::as_str).map(|s| s.trim().to_string())).unwrap_or_default();
    let expected = load_config(&ctx.db).guest_code.trim().to_string();
    if expected.chars().count() < MIN_CODE_LEN || code.is_empty() || !ct_eq(&code, &expected) {
        let left = ctx.hub.login_failed(&req.ip);
        ctx.hub.log(format!("访客码错误：{}（剩余 {left} 次）", req.ip));
        crate::glog::warn("web", format!("网站接入：访客码错误（来源 {}）", req.ip));
        return respond_json(stream, 401, &json!({ "error": "访客码错误", "left": left }), "");
    }
    ctx.hub.login_ok(&req.ip);
    let token = ctx.hub.create_session(&req.ip, req.header("user-agent"));
    ctx.hub.log(format!("访客登录：{}", req.ip));
    crate::glog::info("web", format!("网站接入：访客已登录（来源 {}）", req.ip));
    let secure = if req.https() { "; Secure" } else { "" };
    let cookie = format!("Set-Cookie: {COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}\r\n", SESSION_TTL.as_secs());
    respond_json(stream, 200, &json!({ "ok": true }), &cookie)
}

/// SSE：先推 hello，之后转发事件；15 秒心跳；会话失效 / 服务停止时断开
fn sse(stream: &mut TcpStream, ctx: &Ctx, token: String) -> Result<(), String> {
    let Some((id, rx)) = ctx.hub.add_client() else {
        return respond_json(stream, 503, &err_json("实时连接数已达上限"), "");
    };
    struct Rm<'a>(&'a Hub, u64);
    impl Drop for Rm<'_> {
        fn drop(&mut self) {
            self.0.remove_client(self.1);
        }
    }
    let _rm = Rm(&ctx.hub, id);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache, no-transform\r\nX-Accel-Buffering: no\r\n{SEC_HEADERS}Connection: keep-alive\r\n\r\nretry: 3000\n\n"
    );
    stream.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    stream.write_all(sse_frame("hello", &json!({ "time": now_str() }).to_string()).as_bytes()).map_err(|e| e.to_string())?;
    stream.flush().ok();
    loop {
        if ctx.stop.load(Ordering::SeqCst) || !ctx.hub.session_valid(&token) {
            let _ = stream.write_all(sse_frame("web://logout", "{}").as_bytes());
            break;
        }
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(frame) => {
                let mut out = frame;
                // 合并积压的事件，减少小包写入
                while out.len() < 256 * 1024 {
                    match rx.try_recv() {
                        Ok(f) => out.push_str(&f),
                        Err(_) => break,
                    }
                }
                stream.write_all(out.as_bytes()).map_err(|e| e.to_string())?;
                stream.flush().map_err(|e| e.to_string())?;
            }
            Err(RecvTimeoutError::Timeout) => {
                stream.write_all(b": ping\n\n").map_err(|e| e.to_string())?;
                stream.flush().map_err(|e| e.to_string())?;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

struct ApiErr(u16, String);
impl From<String> for ApiErr {
    fn from(s: String) -> Self {
        ApiErr(400, s)
    }
}
impl From<&str> for ApiErr {
    fn from(s: &str) -> Self {
        ApiErr(400, s.to_string())
    }
}
type ApiResult = Result<Value, ApiErr>;

fn not_found(what: &str) -> ApiErr {
    ApiErr(404, format!("{what}不存在"))
}

fn agents(ctx: &Ctx) -> Arc<AgentManager> {
    ctx.app.state::<Arc<AgentManager>>().inner().clone()
}

fn engine(ctx: &Ctx) -> Arc<Engine> {
    ctx.app.state::<Arc<Engine>>().inner().clone()
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tauri::async_runtime::block_on(f)
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn context_of(ctx: &Ctx, id: &str) -> Result<crate::models::Context, ApiErr> {
    ctx.db.get_context_row(id).ok_or_else(|| not_found("上下文"))
}

fn check_agent(ctx: &Ctx, agent: &str) -> Result<(), ApiErr> {
    if crate::agents::enabled_agents(&ctx.db).iter().any(|a| a == agent) {
        Ok(())
    } else {
        Err(ApiErr(400, format!("Agent「{agent}」未启用")))
    }
}

fn route(req: &Req, ctx: &Ctx) -> ApiResult {
    let db = &ctx.db;
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/projects") => {
            let list = db.list_projects()?;
            Ok(json!(list
                .into_iter()
                .map(|p| json!({ "id": p.id, "name": p.name, "root_path": p.root_path, "description": p.description }))
                .collect::<Vec<_>>()))
        }
        ("GET", "/api/contexts") => Ok(json!(db.list_contexts(&req.q("project"))?)),
        ("GET", "/api/context") => {
            let c = context_of(ctx, &req.q("id"))?;
            let entries = db.list_entries(&c.id)?;
            let commits: Vec<Value> = db
                .sc_list_commits(&c.id, 20)?
                .into_iter()
                .map(|x| json!({ "seq": x.seq, "agent": x.agent_type, "summary": x.summary, "files": serde_json::from_str::<Value>(&x.files).unwrap_or(Value::Null), "time": x.created_at }))
                .collect();
            Ok(json!({ "context": c, "entries": entries, "commits": commits }))
        }
        ("GET", "/api/agents") => {
            let mgr = agents(ctx);
            let enabled = crate::agents::enabled_agents(db);
            let reg = crate::agents::registry_status(&mgr.db, &mgr.tools);
            let mut out = Vec::new();
            for id in enabled {
                let name = reg.iter().find(|r| r.id == id).map(|r| r.name.clone()).unwrap_or_else(|| id.clone());
                let status = block(mgr.status(&id));
                out.push(json!({ "id": id, "name": name, "status": status }));
            }
            Ok(json!(out))
        }
        ("GET", "/api/chat") => {
            let c = context_of(ctx, &req.q("context"))?;
            let agent = req.q("agent");
            check_agent(ctx, &agent)?;
            let binding = db.get_binding(&c.id, &agent)?;
            let sid = binding.as_ref().and_then(|b| b.session_id.clone());
            let mut items = Value::Null;
            if let Some(sid) = &sid {
                if let Ok(Some(raw)) = db.chat_store_get(&format!("{}:{}", c.id, agent)) {
                    if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                        if v.get("version").and_then(Value::as_i64) == Some(2) && v.get("sessionId").and_then(Value::as_str) == Some(sid.as_str()) {
                            items = v.get("items").cloned().unwrap_or(Value::Null);
                        }
                    }
                }
            }
            // 长会话只回最近 N 条（limit=0 取全部），工具详情截短，首屏更快
            let mut omitted = 0usize;
            if let Value::Array(list) = &mut items {
                omitted = keep_tail(list, tail_limit(&req.q("limit")));
            }
            slim_items(&mut items);
            Ok(json!({ "binding": binding, "sessionId": sid, "items": items, "omitted": omitted }))
        }
        ("POST", "/api/chat/history") => {
            let b = req.json()?;
            let c = context_of(ctx, &s(&b, "contextId"))?;
            let agent = s(&b, "agent");
            check_agent(ctx, &agent)?;
            let mgr = agents(ctx);
            let (sid, mut rows) = block(mgr.replay_history(&c, &agent))?;
            let limit = match b.get("limit") {
                Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
                _ => CHAT_TAIL_DEFAULT,
            };
            let omitted = keep_tail(&mut rows, limit);
            let rows: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    let content = if r.role == "tools" {
                        match serde_json::from_str::<Value>(&r.content) {
                            Ok(mut v) => {
                                slim_value(&mut v, SLIM_STR, 0);
                                v.to_string()
                            }
                            Err(_) => r.content,
                        }
                    } else {
                        r.content
                    };
                    json!({ "role": r.role, "content": content })
                })
                .collect();
            Ok(json!({ "sessionId": sid, "rows": rows, "omitted": omitted }))
        }
        ("POST", "/api/chat/send") => {
            let b = req.json()?;
            let c = context_of(ctx, &s(&b, "contextId"))?;
            let agent = s(&b, "agent");
            check_agent(ctx, &agent)?;
            let text = s(&b, "text");
            if text.trim().is_empty() {
                return Err("消息不能为空".into());
            }
            if text.len() > 200_000 {
                return Err("消息过长".into());
            }
            // 客户端可自带请求 ID（用于去重本地已显示的用户气泡），只接受安全字符
            let client_id = s(&b, "requestId");
            let request_id = if (8..=64).contains(&client_id.len()) && client_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                client_id
            } else {
                uuid::Uuid::new_v4().to_string()
            };
            let _ = ctx.app.emit(
                "chat://prompt",
                json!({ "contextId": c.id, "agentType": agent, "text": text, "origin": "web", "requestId": request_id, "time": now_str() }),
            );
            let mgr = agents(ctx);
            let hub = ctx.hub.clone();
            let rid = request_id.clone();
            tauri::async_runtime::spawn(async move {
                let res = mgr.prompt_from_ui(&c, &agent, &text, &[], Some(&rid)).await;
                let payload = match &res {
                    Ok(v) => json!({ "requestId": rid, "contextId": c.id, "agentType": agent, "ok": true, "stopReason": v.get("stopReason") }),
                    Err(e) => json!({ "requestId": rid, "contextId": c.id, "agentType": agent, "ok": false, "error": e }),
                };
                hub.broadcast("web://prompt-done", &payload.to_string());
            });
            Ok(json!({ "requestId": request_id }))
        }
        ("POST", "/api/chat/cancel") => {
            let b = req.json()?;
            let agent = s(&b, "agent");
            block(agents(ctx).cancel(&s(&b, "contextId"), &agent))?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/chat/new") => {
            let b = req.json()?;
            let c = context_of(ctx, &s(&b, "contextId"))?;
            let agent = s(&b, "agent");
            check_agent(ctx, &agent)?;
            let sid = block(agents(ctx).create_fresh_session(&c, &agent))?;
            Ok(json!({ "sessionId": sid }))
        }
        ("GET", "/api/permissions") => Ok(json!(ctx.hub.pending_permissions())),
        ("POST", "/api/permission") => {
            let b = req.json()?;
            let id = s(&b, "requestId");
            let res = block(agents(ctx).respond_permission(&id, s(&b, "optionId")));
            ctx.hub.take_permission(&id);
            res?;
            Ok(json!({ "ok": true }))
        }
        ("GET", "/api/workflows") => {
            let list = db.list_workflows(&req.q("project"))?;
            let active = engine(ctx).active_runs();
            Ok(json!(list
                .into_iter()
                .map(|w| {
                    let running = active.iter().find(|a| a.1 == w.id).map(|a| a.0.clone());
                    let steps: Vec<Value> = serde_json::to_value(&w.steps)
                        .ok()
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|st| json!({ "kind": st.get("kind").cloned().unwrap_or(Value::Null), "name": st.get("name").cloned().unwrap_or(Value::Null) }))
                        .collect();
                    json!({
                        "id": w.id, "name": w.name, "description": w.description, "enabled": w.enabled,
                        "trigger_type": w.trigger_type, "schedule": w.schedule, "steps": steps,
                        "last_run_at": w.last_run_at, "next_run_at": w.next_run_at, "running": running,
                    })
                })
                .collect::<Vec<_>>()))
        }
        ("GET", "/api/runs") => {
            let limit = req.q("limit").parse::<i64>().unwrap_or(20).clamp(1, crate::db::MAX_RUNS_PER_WORKFLOW);
            let runs = db.list_runs(&req.q("workflow"), limit)?;
            Ok(json!(runs
                .into_iter()
                .map(|r| json!({ "id": r.id, "workflow_id": r.workflow_id, "trigger": r.trigger, "status": r.status, "started_at": r.started_at, "finished_at": r.finished_at, "log_len": r.log.len() }))
                .collect::<Vec<_>>()))
        }
        ("GET", "/api/run") => {
            let r = db.get_run(&req.q("id"))?.ok_or_else(|| not_found("运行记录"))?;
            let log = tail_str(&r.log, 512 * 1024);
            Ok(json!({ "id": r.id, "workflow_id": r.workflow_id, "trigger": r.trigger, "status": r.status, "started_at": r.started_at, "finished_at": r.finished_at, "log": log }))
        }
        ("POST", "/api/workflow/run") => {
            let b = req.json()?;
            let run_id = engine(ctx).run_now(&s(&b, "id")).map_err(|e| ApiErr(409, e))?;
            Ok(json!({ "runId": run_id }))
        }
        ("POST", "/api/workflow/stop") => {
            let b = req.json()?;
            engine(ctx).stop_run(&s(&b, "runId"));
            Ok(json!({ "ok": true }))
        }
        ("GET", "/api/fs/list") => {
            let (root, target) = resolve_path(ctx, &req.q("project"), &req.q("path"))?;
            let mut list = crate::fsops::list_dir(&target.to_string_lossy())?;
            list.truncate(5000);
            Ok(json!({ "root": root.to_string_lossy(), "path": target.to_string_lossy(), "entries": list }))
        }
        ("GET", "/api/fs/read") => {
            let (_, target) = resolve_path(ctx, &req.q("project"), &req.q("path"))?;
            let meta = std::fs::metadata(&target).map_err(|e| format!("无法读取文件: {e}"))?;
            if meta.is_dir() {
                return Err("这是一个目录".into());
            }
            if meta.len() > MAX_PREVIEW {
                return Ok(json!({ "tooLarge": true, "size": meta.len() }));
            }
            let (content, binary) = crate::fsops::read_file(&target.to_string_lossy())?;
            Ok(json!({ "content": content, "binary": binary, "size": meta.len() }))
        }
        ("GET", "/api/fs/search") => {
            let (root, _) = resolve_path(ctx, &req.q("project"), "")?;
            let q = req.q("q");
            if q.trim().is_empty() {
                return Ok(json!({ "files": [], "content": [] }));
            }
            let mode = if req.q("mode") == "content" { "content" } else { "files" };
            let opts: crate::search::SearchOpts = serde_json::from_value(json!({
                "root": strip_verbatim(&root), "query": q, "mode": mode, "max_results": 200,
            }))
            .map_err(|e| e.to_string())?;
            let r = crate::search::search(opts)?;
            Ok(serde_json::to_value(r).unwrap_or(Value::Null))
        }
        _ if req.path.starts_with("/api/remote/") => remote_api(req, ctx),
        _ => Err(ApiErr(404, "接口不存在".into())),
    }
}

// ------------------------------------------------------------------ 外部编程接入（MCP）

fn setting(db: &Db, key: &str) -> String {
    db.get_setting(key).ok().flatten().unwrap_or_default()
}

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// 与桌面端「设置 → 外部编程接入」等价的网页端接口（均需登录；写操作另需 X-SD-Web 头）。
/// 服务启停沿用桌面端保存的接入方式与端口；cloudflared 安装 / 路径仍在桌面端操作。
fn remote_api(req: &Req, ctx: &Ctx) -> ApiResult {
    use crate::remote_mcp as rm;
    let db = &ctx.db;
    let remote = ctx.app.state::<rm::RemoteManager>();
    let oauth = ctx.app.state::<Arc<crate::remote_oauth::OAuthState>>().inner().clone();
    let id_of = |v: &Value| -> Result<String, ApiErr> {
        let id = s(v, "id");
        if id.is_empty() { Err("缺少 id".into()) } else { Ok(id) }
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/remote/state") => {
            let exposure = match setting(db, "remote.exposure").as_str() {
                "custom" => "custom",
                _ => "quick_tunnel",
            };
            let port = setting(db, "remote.port").parse::<u16>().ok().filter(|p| *p > 0).unwrap_or(51688);
            let hours = setting(db, "remote.auto_stop_hours").parse::<f64>().ok().filter(|h| *h > 0.0).unwrap_or(6.0);
            Ok(json!({
                "status": remote.status(db),
                "grants": rm::grants_list(db)?,
                "oauthTokens": crate::remote_oauth::tokens_list(db)?,
                "oauthPending": crate::remote_oauth::pending_list(&oauth),
                "cloudflared": rm::cloudflared_status(db),
                "exposure": exposure,
                "customUrl": setting(db, "remote.custom_url"),
                "port": port,
                "execAllowed": setting(db, "remote.exec_allowed") == "1",
                "timerHours": hours,
            }))
        }
        ("POST", "/api/remote/start") => {
            let quick = setting(db, "remote.exposure") != "custom";
            if quick && !rm::cloudflared_status(db).installed {
                return Err("未找到 cloudflared：请先在电脑端「外部编程接入」中快捷安装或指定已有程序".into());
            }
            let port = setting(db, "remote.port").parse::<u16>().ok().filter(|p| *p > 0).unwrap_or(51688);
            remote.start(ctx.app.clone(), db.clone(), engine(ctx), "0.0.0.0", port, quick, oauth)?;
            ctx.hub.log("外部编程服务已由网页端启动");
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/stop") => {
            remote.stop();
            ctx.hub.log("外部编程服务已由网页端停止");
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/exposure") => {
            let v = req.json()?;
            let exposure = if s(&v, "exposure") == "custom" { "custom" } else { "quick_tunnel" };
            db.set_setting("remote.exposure", exposure)?;
            if remote.status(db).running {
                if exposure == "quick_tunnel" {
                    if !rm::cloudflared_status(db).installed {
                        return Err("未找到 cloudflared：请先在电脑端快捷安装或指定已有程序".into());
                    }
                    remote.tunnel_start(ctx.app.clone(), db.clone())?;
                } else {
                    remote.tunnel_stop();
                }
            }
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/options") => {
            let v = req.json()?;
            if let Some(u) = v.get("customUrl").and_then(Value::as_str) {
                let u = u.trim().trim_end_matches('/');
                if !u.is_empty() && !(u.starts_with("http://") || u.starts_with("https://")) {
                    return Err("自定义地址需以 http:// 或 https:// 开头".into());
                }
                db.set_setting("remote.custom_url", u)?;
            }
            if let Some(p) = v.get("port").and_then(Value::as_i64) {
                if !(1..=65535).contains(&p) {
                    return Err("端口需为 1-65535 的整数".into());
                }
                db.set_setting("remote.port", &p.to_string())?;
            }
            if let Some(e) = v.get("execAllowed").and_then(Value::as_bool) {
                db.set_setting("remote.exec_allowed", if e { "1" } else { "0" })?;
            }
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/timer") => {
            let v = req.json()?;
            let hours = v.get("hours").and_then(Value::as_f64).unwrap_or(-1.0);
            if !hours.is_finite() || hours < 0.0 {
                return Err("小时数无效".into());
            }
            if hours == 0.0 {
                db.set_setting("remote.auto_stop_hours", "0")?;
                rm::RemoteManager::arm_timer(&ctx.app, 0.0);
            } else {
                let h = hours.clamp(0.05, 24.0 * 7.0);
                db.set_setting("remote.auto_stop_hours", &format!("{h}"))?;
                rm::RemoteManager::arm_timer(&ctx.app, h);
            }
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/grant/create") => {
            let v = req.json()?;
            let project_id = s(&v, "projectId");
            let project = db
                .list_projects()?
                .into_iter()
                .find(|p| p.id == project_id)
                .ok_or_else(|| not_found("项目"))?;
            let context_id = s(&v, "contextId");
            let context = if context_id.is_empty() {
                None
            } else {
                let c = context_of(ctx, &context_id)?;
                if c.project_id != project.id {
                    return Err("上下文不属于该项目".into());
                }
                Some(c)
            };
            let input = rm::RemoteGrantInput {
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                project_root: project.root_path.clone(),
                context_id: context.as_ref().map(|c| c.id.clone()),
                context_name: context.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                context_enabled: context.is_some(),
                fs_write: b(&v, "fsWrite"),
                exec_allowed: b(&v, "execAllowed"),
            };
            let (grant, token) = rm::grant_create(db, &input)?;
            ctx.hub.log(format!("网页端新建外部编程授权：{}", project.name));
            Ok(json!({ "grant": grant, "token": token }))
        }
        ("POST", "/api/remote/grant/token") => Ok(json!({ "token": rm::grant_token_get(db, &id_of(&req.json()?)?)? })),
        ("POST", "/api/remote/grant/pause") => {
            rm::grant_pause(db, &id_of(&req.json()?)?)?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/grant/resume") => {
            rm::grant_resume(db, &id_of(&req.json()?)?)?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/grant/delete") => {
            rm::grant_delete(db, &id_of(&req.json()?)?)?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/oauth/revoke") => {
            crate::remote_oauth::token_revoke(db, &id_of(&req.json()?)?)?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/remote/oauth/decide") => {
            let v = req.json()?;
            let txn = s(&v, "txnId");
            if txn.is_empty() {
                return Err("缺少 txnId".into());
            }
            crate::remote_oauth::oauth_decide(db, &oauth, &txn, b(&v, "approve"))?;
            Ok(json!({ "ok": true }))
        }
        _ => Err(ApiErr(404, "接口不存在".into())),
    }
}

fn tail_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…（仅显示最后 {} KB）\n{}", max / 1024, &s[start..])
}

/// 去掉 Windows canonicalize 的 `\\?\` 前缀，便于展示与复用现有接口
fn strip_verbatim(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s
    }
}

/// 解析并校验路径：必须位于该项目根目录之内（符号链接 / .. 均先 canonicalize）
fn resolve_path(ctx: &Ctx, project_id: &str, path: &str) -> Result<(PathBuf, PathBuf), ApiErr> {
    let project = ctx
        .db
        .list_projects()?
        .into_iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| not_found("项目"))?;
    let root_raw = crate::fsops::normalize_dir(&project.root_path);
    if root_raw.is_empty() {
        return Err("该项目未设置根目录".into());
    }
    let root = std::fs::canonicalize(&root_raw).map_err(|e| format!("项目目录不可访问: {e}"))?;
    let target_raw = if path.trim().is_empty() { PathBuf::from(&root_raw) } else { PathBuf::from(path.trim()) };
    let target = std::fs::canonicalize(&target_raw).map_err(|e| format!("路径不可访问: {e}"))?;
    if !target.starts_with(&root) {
        return Err(ApiErr(403, "只能访问项目目录内的文件".into()));
    }
    Ok((PathBuf::from(strip_verbatim(&root)), PathBuf::from(strip_verbatim(&target))))
}

fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// 图片原样输出（仅常见位图；SVG 可能内嵌脚本，不在此列）
fn fs_raw(stream: &mut TcpStream, req: &Req, ctx: &Ctx) -> Result<(), String> {
    let (_, target) = match resolve_path(ctx, &req.q("project"), &req.q("path")) {
        Ok(v) => v,
        Err(ApiErr(code, msg)) => return respond_json(stream, code, &err_json(msg), ""),
    };
    let Some(mime) = image_mime(&target) else {
        return respond_json(stream, 415, &err_json("仅支持预览常见图片格式"), "");
    };
    let len = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
    if len > MAX_RAW {
        return respond_json(stream, 413, &err_json("图片过大"), "");
    }
    match std::fs::read(&target) {
        Ok(bytes) => respond(stream, 200, mime, &bytes, "Cache-Control: private, max-age=60\r\nContent-Security-Policy: sandbox\r\n"),
        Err(e) => respond_json(stream, 404, &err_json(format!("读取失败: {e}")), ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slim_truncates_long_strings_on_char_boundary() {
        let mut v = json!({ "a": "中".repeat(5000), "b": ["x".repeat(10)], "n": 1 });
        slim_value(&mut v, 100, 0);
        let a = v["a"].as_str().unwrap();
        assert!(a.starts_with(&"中".repeat(100)));
        assert!(a.contains("共 5000 字符"));
        assert_eq!(v["b"][0].as_str().unwrap().len(), 10);
        assert_eq!(v["n"], 1);
    }

    #[test]
    fn keep_tail_limits() {
        let mut v: Vec<i32> = (0..10).collect();
        assert_eq!(keep_tail(&mut v, 3), 7);
        assert_eq!(v, vec![7, 8, 9]);
        let mut w: Vec<i32> = (0..5).collect();
        assert_eq!(keep_tail(&mut w, 0), 0);
        assert_eq!(w.len(), 5);
        assert_eq!(tail_limit(""), CHAT_TAIL_DEFAULT);
        assert_eq!(tail_limit("0"), 0);
    }

    #[test]
    fn ct_eq_works() {
        assert!(ct_eq("abcd", "abcd"));
        assert!(!ct_eq("abcd", "abce"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "a"));
    }

    #[test]
    fn pct_decode_works() {
        assert_eq!(pct_decode("D%3A%5Cplay%20x+y"), "D:\\play x y");
        assert_eq!(pct_decode("%E4%BD%A0"), "你");
        assert_eq!(pct_decode("100%"), "100%");
    }

    #[test]
    fn normalize_url() {
        assert_eq!(normalize_public_url(" example.com/ "), "http://example.com");
        assert_eq!(normalize_public_url("https://a.b:8443/"), "https://a.b:8443");
        assert_eq!(normalize_public_url(""), "");
    }

    #[test]
    fn sse_frame_multiline() {
        assert_eq!(sse_frame("e", "a\nb"), "event: e\ndata: a\ndata: b\n\n");
    }

    #[test]
    fn guest_code_shape() {
        let c = generate_guest_code();
        assert_eq!(c.len(), 8);
        assert!(c.chars().all(|ch| ch.is_ascii_alphanumeric()));
    }

    #[test]
    fn rate_limit() {
        let hub = Hub::new();
        for _ in 0..FAIL_PER_IP {
            assert!(hub.login_blocked("1.1.1.1").is_none());
            hub.login_failed("1.1.1.1");
        }
        assert!(hub.login_blocked("1.1.1.1").is_some());
        assert!(hub.login_blocked("2.2.2.2").is_none());
    }

    #[test]
    fn sessions() {
        let hub = Hub::new();
        let t = hub.create_session("ip", "ua");
        assert!(hub.session_valid(&t));
        assert!(!hub.session_valid("nope"));
        hub.revoke_all();
        assert!(!hub.session_valid(&t));
    }
}
