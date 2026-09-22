//! 外部编程接入（MCP）：独立于工作流引擎的远端服务管理器。
//!
//! 职责：管理监听生命周期；按「授权（RemoteGrant）」做 Token 鉴权与作用域裁剪
//! （项目目录/上下文/文件权限/命令权限）；复用 coding_mcp 的文件与命令工具、
//! mcp.rs 的共享上下文工具；支持自定义 frp/反代入口与 Cloudflare Quick Tunnel。

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::db::Db;
use tauri::Emitter;

#[derive(Debug, Clone, Serialize)]
pub struct RemoteGrant {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub project_root: String,
    pub context_id: Option<String>,
    pub context_name: String,
    pub context_enabled: bool,
    pub fs_write: bool,
    pub exec_allowed: bool,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub last_used_at: Option<String>,
    /// 仅内部使用（sha256）；不序列化到前端
    #[serde(skip_serializing)]
    pub token_hash: String,
}

pub struct RemoteGrantInput {
    pub project_id: String,
    pub project_name: String,
    pub project_root: String,
    pub context_id: Option<String>,
    pub context_name: String,
    pub context_enabled: bool,
    pub fs_write: bool,
    pub exec_allowed: bool,
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

pub fn sha256_hex(data: &str) -> String {
    let mut h = Sha256::new();
    h.update(data.as_bytes());
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn new_grant_token() -> String {
    let a = uuid::Uuid::new_v4().simple().to_string();
    let b = uuid::Uuid::new_v4().simple().to_string();
    format!("sdv1_{}{}", &a[..16], &b[..24])
}

// ---------------- DB-backed grant store ----------------

const GRANT_COLS: &str =
    "id,project_id,project_name,project_root,context_id,context_name,context_enabled,fs_write,exec_allowed,created_at,revoked_at,last_used_at,token_hash";

fn row_to_grant(r: &rusqlite::Row) -> rusqlite::Result<RemoteGrant> {
    Ok(RemoteGrant {
        id: r.get(0)?,
        project_id: r.get(1)?,
        project_name: r.get(2)?,
        project_root: r.get(3)?,
        context_id: r.get(4)?,
        context_name: r.get(5)?,
        context_enabled: r.get::<_, i64>(6)? != 0,
        fs_write: r.get::<_, i64>(7)? != 0,
        exec_allowed: r.get::<_, i64>(8)? != 0,
        created_at: r.get(9)?,
        revoked_at: r.get(10)?,
        last_used_at: r.get(11)?,
        token_hash: r.get(12)?,
    })
}

pub fn grants_list(db: &Arc<Db>) -> Result<Vec<RemoteGrant>, String> {
    db.with(|c| {
        let mut st = c.prepare(&format!(
            "SELECT {GRANT_COLS} FROM remote_grants ORDER BY created_at DESC"
        ))?;
        let rows = st
            .query_map([], row_to_grant)?
            .collect::<rusqlite::Result<Vec<RemoteGrant>>>()?;
        Ok(rows)
    })
}

pub fn grant_create(db: &Arc<Db>, input: &RemoteGrantInput) -> Result<(RemoteGrant, String), String> {
    if input.project_root.trim().is_empty() {
        return Err("项目目录不能为空".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let token = new_grant_token();
    let hash = sha256_hex(&token);
    let ts = now();
    db.with(|c| {
        c.execute(
            &format!("INSERT INTO remote_grants ({GRANT_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"),
            rusqlite::params![
                id,
                input.project_id,
                input.project_name,
                input.project_root,
                input.context_id,
                input.context_name,
                input.context_enabled as i64,
                input.fs_write as i64,
                input.exec_allowed as i64,
                ts,
                Option::<String>::None,
                Option::<String>::None,
                hash,
            ],
        )?;
        Ok(())
    })?;
    let grant = RemoteGrant {
        id,
        project_id: input.project_id.clone(),
        project_name: input.project_name.clone(),
        project_root: input.project_root.clone(),
        context_id: input.context_id.clone(),
        context_name: input.context_name.clone(),
        context_enabled: input.context_enabled,
        fs_write: input.fs_write,
        exec_allowed: input.exec_allowed,
        created_at: ts,
        revoked_at: None,
        last_used_at: None,
        token_hash: hash.clone(),
    };
    Ok((grant, token))
}

pub fn grant_revoke(db: &Arc<Db>, id: &str) -> Result<(), String> {
    db.with(|c| {
        c.execute(
            "UPDATE remote_grants SET revoked_at=?2 WHERE id=?1 AND revoked_at IS NULL",
            rusqlite::params![id, now()],
        )?;
        Ok(())
    })
}

pub fn grant_delete(db: &Arc<Db>, id: &str) -> Result<(), String> {
    db.with(|c| {
        c.execute("DELETE FROM remote_grants WHERE id=?1", rusqlite::params![id])?;
        Ok(())
    })
}

fn grant_touch(db: &Arc<Db>, id: &str) {
    let _ = db.with(|c| {
        c.execute(
            "UPDATE remote_grants SET last_used_at=?2 WHERE id=?1",
            rusqlite::params![id, now()],
        )?;
        Ok(())
    });
}

// ---------------- runtime manager ----------------

fn rt_log_into(logs: &Arc<Mutex<VecDeque<String>>>, line: String) {
    let mut q = logs.lock().unwrap_or_else(|p| p.into_inner());
    q.push_back(chrono::Local::now().format("[%H:%M:%S] ").to_string() + &line);
    while q.len() > 120 {
        q.pop_front();
    }
}

fn rt_log(rt: &RemoteRuntime, line: String) {
    rt_log_into(&rt.logs, line);
}

fn rt_log_logs(logs: &Arc<Mutex<VecDeque<String>>>, line: String) {
    rt_log_into(logs, line);
}


pub struct RemoteRuntime {
    pub stop_flag: Arc<std::sync::atomic::AtomicBool>,
    pub handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub started_at: String,
    pub public_url: Arc<RwLock<Option<String>>>,
    pub logs: Arc<Mutex<VecDeque<String>>>,
    pub cloudflared_pid: Arc<Mutex<Option<u32>>>,
    pub tunnel_generation: Arc<AtomicU64>,
    pub port: u16,
}

#[derive(Clone, Serialize)]
pub struct RemoteStatus {
    pub running: bool,
    pub port: u16,
    pub started_at: Option<String>,
    pub public_url: Option<String>,
    pub grants_active: i64,
    pub tunnel_running: bool,
    pub logs: Vec<String>,
}

#[derive(Default)]
pub struct RemoteManager {
    pub runtime: RwLock<Option<Arc<RemoteRuntime>>>,
}

impl RemoteManager {
    pub fn new() -> Self {
        Self { runtime: RwLock::new(None) }
    }

    fn log_into(logs: &Arc<Mutex<VecDeque<String>>>, line: String) {
        let mut q = logs.lock().unwrap_or_else(|p| p.into_inner());
        q.push_back(chrono::Local::now().format("[%H:%M:%S] ").to_string() + &line);
        while q.len() > 120 {
            q.pop_front();
        }
    }

    fn rt_log(rt: &RemoteRuntime, line: String) {
        Self::log_into(&rt.logs, line);
    }

    pub fn status(&self, db: &Arc<Db>) -> RemoteStatus {
        let guard = self.runtime.read().unwrap_or_else(|p| p.into_inner());
        let grants_active = grants_list(db)
            .map(|g| g.iter().filter(|g| g.revoked_at.is_none()).count() as i64)
            .unwrap_or(0);
        match guard.as_ref() {
            Some(rt) => RemoteStatus {
                running: true,
                port: rt.port,
                started_at: Some(rt.started_at.clone()),
                public_url: rt.public_url.read().map(|u| u.clone()).unwrap_or(None),
                grants_active,
                tunnel_running: rt.cloudflared_pid.lock().map(|c| c.is_some()).unwrap_or(false),
                logs: rt.logs.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default(),
            },
            None => RemoteStatus {
                running: false,
                port: 0,
                started_at: None,
                public_url: None,
                grants_active,
                tunnel_running: false,
                logs: Vec::new(),
            },
        }
    }

    /// 启动远端服务（幂等）：绑定监听 → 服务线程 →（可选）cloudflared 临时隧道。
    pub fn start(
        &self,
        app: tauri::AppHandle,
        db: Arc<Db>,
        engine: Arc<crate::engine::Engine>,
        listen_host: &str,
        port: u16,
        quick_tunnel: bool,
    ) -> Result<(), String> {
        {
            let guard = self.runtime.read().unwrap_or_else(|p| p.into_inner());
            if let Some(rt) = guard.as_ref() {
                // 已在运行：切到 Cloudflare 后再次点「启动」时补起隧道（其余情况幂等返回）
                let tunnel_up = rt.cloudflared_pid.lock().map(|c| c.is_some()).unwrap_or(false);
                if quick_tunnel && !tunnel_up {
                    let rt = rt.clone();
                    drop(guard);
                    return start_quick_tunnel(rt, app, db);
                }
                return Ok(());
            }
        }
        let listener = TcpListener::bind((listen_host, port))
            .map_err(|e| format!("绑定 {listen_host}:{port} 失败: {e}（端口被占用或无权限）"))?;
        let state = Arc::new(crate::mcp::McpState {
            db: db.clone(),
            engine: std::sync::OnceLock::from(engine),
            commit_lock: Mutex::new(()),
        });
        let logs = Arc::new(Mutex::new(VecDeque::new()));
        let public_url = Arc::new(RwLock::new(None));
        let cloudflared_pid = Arc::new(Mutex::new(None::<u32>));
        let generation = Arc::new(AtomicU64::new(0));
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started_at = now();
        Self::log_into(&logs, format!("服务已启动：{listen_host}:{port}（提示：监听 0.0.0.0 时同网络设备可能访问到）"));

        // accept 循环线程：stop_flag 置位后以自身连接唤醒并退出
        let accept_logs = logs.clone();
        let accept_state = state.clone();
        let accept_app = app.clone();
        let accept_db = db.clone();
        let accept_stop = stop_flag.clone();
        let handle = std::thread::spawn(move || {
            listener.set_nonblocking(true).ok();
            loop {
                if accept_stop.load(Ordering::SeqCst) { break; }
                match listener.accept() {
                    Ok((stream, _)) => {
                        if accept_stop.load(Ordering::SeqCst) { drop(stream); break; }
                        let state = accept_state.clone();
                        let app = accept_app.clone();
                        let db = accept_db.clone();
                        let logs = accept_logs.clone();
                        std::thread::spawn(move || {
                            if let Err(e) = handle_remote_http(stream, logs, state, app, db) {
                                log::warn!("[remote-mcp] conn: {e}");
                            }
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(150));
                    }
                    Err(_) => break,
                }
            }
        });

        let runtime = Arc::new(RemoteRuntime {
            stop_flag: stop_flag.clone(),
            handle: Mutex::new(Some(handle)),
            started_at: started_at.clone(),
            public_url,
            logs,
            cloudflared_pid,
            tunnel_generation: generation,
            port,
        });

        if quick_tunnel {
            let t_rt = runtime.clone();
            let t_app = app.clone();
            let t_db = db.clone();
            std::thread::spawn(move || {
                if let Err(e) = start_quick_tunnel(t_rt.clone(), t_app.clone(), t_db.clone()) {
                    Self::log_into(&t_rt.logs, format!("隧道启动失败：{e}"));
                }
            });
        }

        *self.runtime.write().unwrap_or_else(|p| p.into_inner()) = Some(runtime);
        Ok(())
    }

    /// 运行中单独启动 Quick Tunnel（服务未启动时报错；已在跑则幂等）。
    pub fn tunnel_start(&self, app: tauri::AppHandle, db: Arc<Db>) -> Result<(), String> {
        let rt = {
            let guard = self.runtime.read().unwrap_or_else(|p| p.into_inner());
            guard.as_ref().ok_or("服务未启动：请先启动外部编程 MCP 服务")?.clone()
        };
        let tunnel_up = rt.cloudflared_pid.lock().map(|c| c.is_some()).unwrap_or(false);
        if tunnel_up {
            return Ok(());
        }
        Self::log_into(&rt.logs, "正在启动 Cloudflare 临时隧道…".into());
        start_quick_tunnel(rt, app, db)
    }

    /// 运行中单独停止 Quick Tunnel（清 pid/地址，旧读线程按 generation 静默失效）。
    pub fn tunnel_stop(&self) {
        let guard = self.runtime.read().unwrap_or_else(|p| p.into_inner());
        if let Some(rt) = guard.as_ref() {
            rt.tunnel_generation.fetch_add(1, Ordering::SeqCst);
            let pid = rt.cloudflared_pid.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Ok(mut u) = rt.public_url.write() {
                *u = None;
            }
            if let Some(pid) = pid {
                let _ = crate::setup::hide_console(&mut std::process::Command::new("taskkill")).args(["/F", "/T", "/PID", &pid.to_string()]).output();
                Self::log_into(&rt.logs, "隧道已停止，接入地址已清空".into());
            }
        }
    }

    pub fn stop(&self) {
        let rt = self.runtime.write().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(rt) = rt {
            rt.tunnel_generation.fetch_add(1, Ordering::SeqCst); // 旧读线程静默退出
            rt.stop_flag.store(true, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect(("127.0.0.1", rt.port));
            if let Some(h) = rt.handle.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = h.join();
            }
            if let Some(pid) = rt.cloudflared_pid.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = crate::setup::hide_console(&mut std::process::Command::new("taskkill")).args(["/F", "/T", "/PID", &pid.to_string()]).output();
            }
            Self::log_into(&rt.logs, "服务已停止".into());
        }
    }
}


/// 启动 cloudflared Quick Tunnel；从输出流严格校验并提取临时地址。
fn start_quick_tunnel(rt: Arc<RemoteRuntime>, app: tauri::AppHandle, db: Arc<Db>) -> Result<(), String> {
    let logs = rt.logs.clone();
    let port = rt.port;
    let exe = resolve_cloudflared(db.as_ref())?;
    let gen = rt.tunnel_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["tunnel", "--no-autoupdate", "--url", &format!("http://127.0.0.1:{port}")])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::setup::hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("启动 cloudflared 失败: {e}"))?;
    let pid = child.id();
    if let Some(mut slot) = rt.cloudflared_pid.lock().ok() {
        *slot = Some(pid);
    }
    rt_log(&rt, format!("cloudflared 已启动（pid {pid}），正在获取临时地址…"));

    let out = child.stdout.take();
    let err = child.stderr.take();
    let rt2 = rt.clone();
    let app2 = app.clone();
    let logs2 = logs.clone();
    let pid2 = pid;
    std::thread::spawn(move || {
        use std::io::Read;
        // 每个流独立线程读取：stdout/stderr 都是阻塞管道，单线程顺序读会
        // 永久卡在无输出的 stdout 上，stderr 里的隧道地址永远轮不到。
        let text = Arc::new(Mutex::new(String::new()));
        let streams: Vec<Option<Box<dyn Read + Send>>> = vec![
            out.map(|p| Box::new(p) as Box<dyn Read + Send>),
            err.map(|p| Box::new(p) as Box<dyn Read + Send>),
        ];
        let mut readers = Vec::new();
        for s in streams.into_iter().flatten() {
            let text = text.clone();
            readers.push(std::thread::spawn(move || {
                let mut r = s;
                let mut buf = [0u8; 4096];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut t = text.lock().unwrap_or_else(|p| p.into_inner());
                            t.push_str(&String::from_utf8_lossy(&buf[..n]));
                        }
                    }
                }
            }));
        }
        // 轮询拼装文本：提取地址 → 成功上报；进程退出/超时 → 诊断 + 失败上报。
        // cloudflared 的原始输出增量转发到运行日志，便于网络问题自诊。
        // generation 守卫：服务停止/隧道重启后，旧线程的迟到输出不再生效或误报。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut logged = 0usize;
        let still_current = || rt2.tunnel_generation.load(Ordering::SeqCst) == gen;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if !still_current() {
                break;
            }
            let snapshot = text.lock().map(|t| t.clone()).unwrap_or_default();
            if snapshot.len() > logged {
                for line in snapshot[logged..].lines().filter(|l| !l.trim().is_empty()) {
                    rt_log_logs(&logs2, format!("cloudflared：{}", line.trim()));
                }
                logged = snapshot.len();
            }
            if let Some(url) = extract_quick_tunnel_url(&snapshot) {
                if let Some(mut u) = rt2.public_url.write().ok() {
                    *u = Some(url.clone());
                }
                rt_log_logs(&logs2, format!("隧道地址：{url}"));
                let _ = app2.emit("remote://tunnel", serde_json::json!({ "publicUrl": url, "generation": gen }));
                break;
            }
            if readers.iter().all(|h| h.is_finished()) {
                // 进程已退出：清 pid/地址让状态回到真实（隧道已断）
                if let Some(mut p) = rt2.cloudflared_pid.lock().ok() {
                    if *p == Some(pid2) { *p = None; }
                }
                if let Ok(mut u) = rt2.public_url.write() {
                    *u = None;
                }
                rt_log_logs(&logs2, "隧道进程已退出且未提供地址".into());
                let _ = app2.emit("remote://tunnel", serde_json::json!({ "error": "exited_without_url", "generation": gen }));
                break;
            }
            if std::time::Instant::now() > deadline {
                rt_log_logs(&logs2, "获取隧道地址超时（90s），已终止 cloudflared".into());
                let _ = crate::setup::hide_console(&mut std::process::Command::new("taskkill")).args(["/F", "/T", "/PID", &pid2.to_string()]).output();
                if let Some(mut p) = rt2.cloudflared_pid.lock().ok() {
                    if *p == Some(pid2) { *p = None; }
                }
                let _ = app2.emit("remote://tunnel", serde_json::json!({ "error": "url_timeout", "generation": gen }));
                break;
            }
        }
    });
    Ok(())
}


/// 严格校验 trycloudflare 临时地址：https、主机恰为 `<label>.trycloudflare.com`。
fn extract_quick_tunnel_url(text: &str) -> Option<String> {
    let mut search = 0usize;
    while let Some(pos) = text[search..].find("https://") {
        let start = search + pos;
        let rest = &text[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '`' || c == ',' || c == ')')
            .unwrap_or(rest.len());
        let candidate = &rest[..end];
        let host = candidate.strip_prefix("https://")?.split('/').next()?.to_string();
        let labels: Vec<&str> = host.split('.').collect();
        let ok = labels.len() == 3
            && labels[1] == "trycloudflare"
            && labels[2] == "com"
            && !labels[0].is_empty()
            && labels[0].chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if ok {
            return Some(candidate.to_string());
        }
        search = start + 8;
    }
    None
}

/// cloudflared 解析顺序：设置指定路径 → 用户数据 tools 目录 → PATH。
fn resolve_cloudflared(db: &Db) -> Result<PathBuf, String> {
    if let Ok(Some(p)) = db.get_setting("remote.cloudflared_path") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = PathBuf::from(&appdata)
            .join("com.shidrive.desktop")
            .join("tools")
            .join("cloudflared")
            .join("cloudflared.exe");
        if p.is_file() {
            return Ok(p);
        }
    }
    crate::setup::which("cloudflared.exe")
        .ok_or_else(|| "未找到 cloudflared：请在设置中快捷安装或选择已有程序".into())
}

// ---------------- per-connection HTTP handling ----------------

/// cloudflared 环境状态（设置 → 外部编程接入 的检测展示）。
#[derive(Serialize)]
pub struct CloudflaredStatus {
    pub installed: bool,
    pub path: String,
    pub version: String,
}

/// 非致命版解析：设置指定路径 → 用户数据 tools 目录 → PATH。
fn resolve_cloudflared_opt(db: &Db) -> Option<PathBuf> {
    resolve_cloudflared(db).ok()
}

/// 检测 cloudflared：解析可执行文件并尝试读取 `--version`（最多等 3 秒）。
pub fn cloudflared_status(db: &Db) -> CloudflaredStatus {
    let Some(exe) = resolve_cloudflared_opt(db) else {
        return CloudflaredStatus { installed: false, path: String::new(), version: String::new() };
    };
    let mut vc = std::process::Command::new(&exe);
    vc.arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    crate::setup::hide_console(&mut vc);
    let version = vc
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Read;
            let mut out = child.stdout.take().map(|mut p| {
                std::thread::spawn(move || {
                    let mut s = String::new();
                    let _ = p.read_to_string(&mut s);
                    s
                })
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let done = loop {
                match child.try_wait() {
                    Ok(Some(_)) => break true,
                    Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
                    _ => break false,
                }
            };
            if !done {
                let _ = child.kill();
            }
            let text = out.take().and_then(|h| h.join().ok()).unwrap_or_default();
            let line = text.lines().next().unwrap_or("").trim().to_string();
            (!line.is_empty()).then_some(line)
        })
        .unwrap_or_default();
    CloudflaredStatus { installed: true, path: exe.to_string_lossy().to_string(), version }
}

/// 保存用户手动指定的 cloudflared 路径（空串=清除，回到自动解析）。
pub fn cloudflared_set_path(db: &Arc<Db>, path: &str) -> Result<(), String> {
    let p = path.trim();
    if !p.is_empty() && !PathBuf::from(p).is_file() {
        return Err(format!("文件不存在：{p}"));
    }
    db.set_setting("remote.cloudflared_path", p)
}

fn handle_remote_http(
    mut stream: std::net::TcpStream,
    logs: Arc<Mutex<VecDeque<String>>>,
    state: Arc<crate::mcp::McpState>,
    app: tauri::AppHandle,
    db: Arc<Db>,
) -> Result<(), String> {
    use std::io::{Read as _, Write as _};
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            return Err("请求头过大".into());
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 8 * 1024 * 1024 {
        return write_http(stream, 413, "application/json", "{\"error\":\"请求体过大（上限 8MB）\"}");
    }

    // 鉴权前置于读 body：未授权连接在这里就被挡掉，不再消耗读体资源
    // （Authorization: Bearer / X-ShiDrive-Token / ?passcode=）
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    let mut token: Option<String> = None;
    for line in head.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("authorization: bearer ") {
            token = Some(line[21..].trim().to_string());
        } else if lower.starts_with("x-shidrive-token:") {
            token = Some(line[17..].trim().to_string());
        }
    }
    if token.is_none() {
        for pair in query.split('&') {
            if let Some(v) = pair.strip_prefix("passcode=") {
                token = Some(v.to_string());
            }
        }
    }
    let Some(token) = token else {
        return write_http(stream, 401, "application/json", "{\"error\":\"需要凭据：Authorization: Bearer <token> 或 ?passcode=<token>\"}");
    };
    let hash = sha256_hex(&token);
    let grants = grants_list(&db).map_err(|e| e.to_string())?;
    let Some(grant) = grants.iter().find(|g| g.token_hash == hash && g.revoked_at.is_none()) else {
        return write_http(stream, 401, "application/json", "{\"error\":\"凭据无效或已撤销\"}");
    };
    grant_touch(&db, &grant.id);

    // 鉴权通过后再读 body
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();

    // OPTIONS 预检
    if method == "OPTIONS" {
        let resp = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: authorization,content-type,x-shidrive-token\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(resp.as_bytes()).map_err(|e| e.to_string())?;
        return Ok(());
    }

    // MCP 端点（仅 POST /mcp）
    if path.ends_with("/mcp") && method == "POST" {
        let msg: serde_json::Value = serde_json::from_str(body.trim()).map_err(|e| format!("JSON 解析失败: {e}"))?;
        let method_name = msg.get("method").and_then(|m| m.as_str()).unwrap_or_default().to_string();
        let id = msg.get("id").cloned().unwrap_or(serde_json::json!(1));
        let resp = match method_name.as_str() {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "shidrive-remote-mcp", "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            "notifications/initialized" | "notifications/cancelled" => serde_json::Value::Null,
            "ping" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => {
                let tools = scoped_tools(grant);
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools } })
            }
            "tools/call" => {
                let name = msg.pointer("/params/name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let mut args = msg.pointer("/params/arguments").cloned().unwrap_or(serde_json::json!({}));
                // 授权边界：context_* 仅在勾选共享上下文时放行，context_id 由服务端固定
                if name.starts_with("context_") {
                    if !grant.context_enabled {
                        return write_http(stream, 403, "application/json", "{\"error\":\"此授权未开放共享上下文\"}");
                    }
                    if let Some(cid) = &grant.context_id {
                        args["context_id"] = serde_json::json!(cid);
                    }
                }
                if name == "pc.process.exec" {
                    let global = state
                        .db
                        .get_setting("remote.exec_allowed")
                        .ok()
                        .flatten()
                        .map(|v| v == "1")
                        .unwrap_or(false);
                    if !(global && grant.exec_allowed) {
                        return write_http(stream, 403, "application/json", "{\"error\":\"命令执行未授权\"}");
                    }
                }
                if name.starts_with("pc.fs.write") && !grant.fs_write {
                    return write_http(stream, 403, "application/json", "{\"error\":\"文件写入未授权（只读）\"}");
                }
                let result = if name.starts_with("context_") {
                    crate::mcp::call_tool(&state, &app, &name, &args)
                } else if name.starts_with("pc.") {
                    let cfg = crate::coding_mcp::Cfg {
                        root: PathBuf::from(&grant.project_root),
                        token: String::new(),
                        auth_user: String::new(),
                        auth_pass: String::new(),
                        exec_enabled: grant.exec_allowed,
                    };
                    crate::coding_mcp::call_tool(&cfg, &name, &args)
                } else {
                    Err(format!("未知工具 {name}"))
                };
                match result {
                    Ok(v) => serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": { "content": [{ "type": "text", "text": v.to_string() }] }
                    }),
                    Err(e) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": e } }),
                }
            }
            other => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("未知方法 {other}") } }),
        };
        if resp.is_null() {
            // notification：无响应体
            return Ok(());
        }
        let body_text = serde_json::to_string(&resp).unwrap_or_default();
        let resp_text = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body_text}",
            body_text.len()
        );
        stream.write_all(resp_text.as_bytes()).map_err(|e| e.to_string())?;
        return Ok(());
    }

    write_http(stream, 404, "text/plain", "not found")
}

fn scoped_tools(grant: &RemoteGrant) -> serde_json::Value {
    let mut tools = vec![
        serde_json::json!({ "name": "pc.fs.read", "description": "按行读取文本文件", "inputSchema": { "type": "object", "properties": { "path": {"type":"string"}, "offset": {"type":"integer"}, "limit": {"type":"integer"} } } }),
        serde_json::json!({ "name": "pc.fs.list", "description": "列出目录", "inputSchema": { "type": "object", "properties": { "path": {"type":"string"} } } }),
        serde_json::json!({ "name": "pc.fs.search", "description": "文本搜索", "inputSchema": { "type": "object", "properties": { "path": {"type":"string"}, "query": {"type":"string"} } } }),
    ];
    if grant.fs_write {
        tools.push(serde_json::json!({ "name": "pc.fs.write", "description": "写入文本文件（原子替换）", "inputSchema": { "type": "object", "properties": { "path": {"type":"string"}, "content": {"type":"string"} }, "required": ["path","content"] } }));
        tools.push(serde_json::json!({ "name": "pc.fs.mkdir", "description": "创建目录", "inputSchema": { "type": "object", "properties": { "path": {"type":"string"} } } }));
    }
    if grant.exec_allowed {
        tools.push(serde_json::json!({ "name": "pc.process.exec", "description": "执行命令（需同时开启全局允许执行）", "inputSchema": { "type": "object", "properties": { "command": {"type":"string"}, "timeout": {"type":"integer"} }, "required": ["command"] } }));
    }
    if grant.context_enabled {
        for name in ["context_get", "context_get_version", "context_update", "context_history", "context_search"] {
            tools.push(serde_json::json!({ "name": name, "description": "共享上下文工具（绑定授权上下文）" }));
        }
    }
    serde_json::Value::Array(tools)
}

use std::io::{Read as _, Write as _};

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn write_http(mut stream: std::net::TcpStream, status: u16, ctype: &str, body: &str) -> Result<(), String> {
    let text = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
        if status < 400 { "OK" } else { "Error" },
        body.len()
    );
    stream.write_all(text.as_bytes()).map_err(|e| e.to_string())
}
