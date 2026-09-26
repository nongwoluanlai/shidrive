//! Minimal ACP (Agent Client Protocol) client over stdio JSON-RPC.
//!
//! Speaks newline-delimited JSON-RPC 2.0 with an adapter process
//! (e.g. `@agentclientprotocol/codex-acp`, `zcode-acp-server`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::models::AgentLaunch;

pub const EVT_UPDATE: &str = "acp://update";
pub const EVT_STATUS: &str = "acp://status";
pub const EVT_PERMISSION: &str = "acp://permission";
/// ACP unstable elicitation：agent 请求用户输入（选项/自由文本），ACP 规范尚未转正，
/// 适配器侧以 MCP elicitation 形状桥接（message + requestedSchema → {action, content}）
pub const EVT_ELICIT: &str = "acp://elicitation";

type PendingMap = Arc<Mutex<HashMap<Value, tokio::sync::oneshot::Sender<Result<Value, String>>>>>;
/// permission_key ("{agent_type}:{rpc_id}") -> resolver delivering the chosen option id
type PermissionMap = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Value>>>>;
/// elicitation_key -> resolver delivering {action: accept|decline|cancel, content?}

/// Match adapter and preflight environment, including PATH prefix semantics.
pub(crate) fn apply_launch_env(cmd: &mut tokio::process::Command, env: &std::collections::BTreeMap<String, String>) {
    cmd.env("PYTHONUNBUFFERED", "1");
    for (key, value) in env {
        if key.eq_ignore_ascii_case("PATH") {
            let mut paths: Vec<_> = std::env::split_paths(value).collect();
            if let Some(current) = std::env::var_os("PATH") { paths.extend(std::env::split_paths(&current)); }
            cmd.env("PATH", std::env::join_paths(paths).unwrap_or_else(|_| value.into()));
        } else {
            cmd.env(key, value);
        }
    }
}

struct PendingRequest { pending: PendingMap, id: Value }

/// JSON-RPC ids may be numbers or strings; adapters choose either. Never coerce.
fn parse_rpc_id(v: Option<&Value>) -> Option<Value> {
    let v = v?;
    match v {
        Value::Number(_) | Value::String(_) => Some(v.clone()),
        _ => None,
    }
}
impl Drop for PendingRequest {
    fn drop(&mut self) { self.pending.lock().unwrap().remove(&self.id); }
}

/// Accumulated transcript of the current turn for one session (persisted on turn end).
#[derive(Default)]
struct TurnBuffer {
    context_id: Option<String>,
    text: String,
    thought: String,
    tools: Vec<Value>,
}

pub struct AcpConnection {
    pub agent_type: String,
    out_tx: tokio::sync::mpsc::UnboundedSender<String>,
    child: Mutex<Option<tokio::process::Child>>,
    io_tasks: Mutex<Vec<tokio::task::AbortHandle>>,
    next_id: AtomicI64,
    pending: PendingMap,
    permissions: PermissionMap,
    elicitations: PermissionMap,
    buffers: Arc<Mutex<HashMap<String, TurnBuffer>>>,
    /// sessions currently being loaded (replay updates are discarded)
    loading: Arc<Mutex<HashSet<String>>>,
    /// sessions successfully loaded/created on this connection (skip redundant session/load)
    loaded: Arc<Mutex<HashSet<String>>>,
    /// serialize session/load operations (adapters dislike concurrent loads)
    load_lock: Arc<tokio::sync::Mutex<()>>,
    /// cached capability fragments (models/modes/configOptions) from session responses
    caps_cache: Mutex<Value>,
    /// sessions whose updates are being captured (history replay on bind)
    capturing: Arc<Mutex<HashSet<String>>>,
    captures: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    pub agent_info: Mutex<Value>,
    alive: AtomicBool,
    app: AppHandle,
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.start_kill();
        }
    }
}

impl AcpConnection {
    /// Spawn the adapter process and perform the ACP handshake.
    pub async fn spawn(app: AppHandle, agent_type: &str, launch: &AgentLaunch) -> Result<Arc<Self>, String> {
        let mut cmd = tokio::process::Command::new(&launch.command);
        cmd.args(&launch.args)
            .env("PYTHONUNBUFFERED", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        apply_launch_env(&mut cmd, &launch.env);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = cmd.spawn().map_err(|e| format!("启动适配器失败 ({}): {e}", launch.command))?;
        crate::child_job::attach(&child); // 随宿主退出回收（含其派生的 codex 等孙进程）
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let writer_task = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut writer = stdin;
            while let Some(line) = out_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
        });

        // Log adapter stderr (never blocking the protocol).
        let stderr_type = agent_type.to_string();
        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    log::debug!("[{stderr_type} stderr] {line}");
                }
            }
        });

        let conn = Arc::new(Self {
            agent_type: agent_type.to_string(),
            out_tx,
            child: Mutex::new(Some(child)),
            io_tasks: Mutex::new(vec![writer_task.abort_handle(), stderr_task.abort_handle()]),
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            permissions: Arc::new(Mutex::new(HashMap::new())),
            elicitations: Arc::new(Mutex::new(HashMap::new())),
            buffers: Arc::new(Mutex::new(HashMap::new())),
            loading: Arc::new(Mutex::new(HashSet::new())),
            loaded: Arc::new(Mutex::new(HashSet::new())),
            load_lock: Arc::new(tokio::sync::Mutex::new(())),
            caps_cache: Mutex::new(Value::Null),
            capturing: Arc::new(Mutex::new(HashSet::new())),
            captures: Arc::new(Mutex::new(HashMap::new())),
            agent_info: Mutex::new(Value::Null),
            alive: AtomicBool::new(true),
            app,
        });

        conn.spawn_reader(stdout);
        conn.emit_status("connecting", None);
        // Also closes the process when the initialize future is cancelled.
        struct InitGuard(Option<Arc<AcpConnection>>);
        impl Drop for InitGuard {
            fn drop(&mut self) { if let Some(conn) = self.0.take() { conn.shutdown(); } }
        }
        let mut guard = InitGuard(Some(conn.clone()));

        let init = conn
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": true, "writeTextFile": true },
                        "terminal": false
                    }
                }),
                Some(Duration::from_secs(30)),
            )
            .await?;
        conn.emit_status("connected", Some(&init));
        *conn.agent_info.lock().unwrap() = init;
        guard.0 = None;
        Ok(conn)
    }

    fn spawn_reader(self: &Arc<Self>, stdout: tokio::process::ChildStdout) {
        let conn = self.clone();
        let task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut reader = stdout;
            let mut buf = Vec::with_capacity(8192);
            let mut chunk = [0u8; 8192];
            'outer: loop {
                match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let s = String::from_utf8_lossy(&line);
                    let s = s.trim();
                    if s.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(s) {
                        Ok(msg) => {
                            if conn.handle_message(msg).await.is_err() {
                                break 'outer;
                            }
                        }
                        Err(e) => log::debug!("[{}] non-json line: {e}", conn.agent_type),
                    }
                }
                if buf.len() > 32 * 1024 * 1024 {
                    break;
                }
            }
            conn.on_closed();
        });
        self.io_tasks.lock().unwrap().push(task.abort_handle());
    }

    fn on_closed(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        // fail all pending requests
        let mut pending = self.pending.lock().unwrap();
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err("适配器连接已断开".into()));
        }
        drop(pending);
        // cancel pending permissions with "reject"
        let mut perms = self.permissions.lock().unwrap();
        for (_, tx) in perms.drain() {
            let _ = tx.send(json!({ "optionId": "__connection_closed__" }));
        }
        drop(perms);
        // Windows：先按 PID 终止整棵进程树（适配器派生的 codex/node 孙进程
        // 不随直接子进程死亡）；start_kill 作为兜底
        #[cfg(windows)]
        if let Some(pid) = self.child.lock().unwrap().as_ref().and_then(|c| c.id()) {
            let _ = crate::setup::hide_console(&mut std::process::Command::new("taskkill"))
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output();
        }
        if let Some(child) = self.child.lock().unwrap().as_mut() { let _ = child.start_kill(); }
        for task in self.io_tasks.lock().unwrap().drain(..) { task.abort(); }
        self.emit_status("disconnected", None);
    }

    fn emit_status(&self, state: &str, info: Option<&Value>) {
        let _ = self.app.emit(
            EVT_STATUS,
            json!({ "agentType": self.agent_type, "state": state, "info": info.cloned().unwrap_or(Value::Null) }),
        );
    }

    /// Handle one incoming JSON-RPC message. Returns Err when the stream should close.
    async fn handle_message(self: &Arc<Self>, msg: Value) -> Result<(), ()> {
        let method = msg.get("method").and_then(|m| m.as_str()).map(|s| s.to_string());
        let id = parse_rpc_id(msg.get("id"));

        match (method.as_deref(), id) {
            // response to our request
            (None, Some(id)) => {
                if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                    if let Some(err) = msg.get("error") {
                        let _ = tx.send(Err(format_acp_error(err)));
                    } else {
                        let _ = tx.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                    }
                }
                Ok(())
            }
            // server -> client request
            (Some(m), Some(id)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                self.handle_server_request(id, m, params).await;
                Ok(())
            }
            // notification
            (Some(m), None) => {
                if m == "session/update" {
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    self.handle_session_update(params);
                }
                Ok(())
            }
            (None, None) => Ok(()),
        }
    }

    async fn handle_server_request(self: &Arc<Self>, id: Value, method: &str, params: Value) {
        match method {
            "fs/read_text_file" => {
                let path = params.get("path").and_then(|p| p.as_str()).unwrap_or_default();
                let line = params.get("line").and_then(|v| v.as_u64());
                let limit = params.get("limit").and_then(|v| v.as_u64());
                match read_text_file_range(path, line, limit) {
                    Ok(content) => self.respond(id, Ok(json!({ "content": content }))),
                    Err(e) => self.respond(id, Err(json!({ "code": -32000, "message": e }))),
                }
            }
            "fs/write_text_file" => {
                let path = params.get("path").and_then(|p| p.as_str()).unwrap_or_default();
                let contents = params.get("contents").and_then(|c| c.as_str()).unwrap_or_default();
                match std::fs::write(path, contents) {
                    Ok(_) => {
                        let _ = self.app.emit(EVT_UPDATE, json!({ "agentType": self.agent_type, "fileChanged": path }));
                        self.respond(id, Ok(Value::Null))
                    }
                    Err(e) => self.respond(id, Err(json!({ "code": -32000, "message": e.to_string() }))),
                }
            }
            "session/request_permission" => {
                let session_id = params.get("sessionId").and_then(|s| s.as_str()).unwrap_or_default().to_string();
                let key = format!("{}:{}:{id}", self.agent_type, uuid::Uuid::new_v4());
                let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
                {
                    let mut permissions = self.permissions.lock().unwrap();
                    if !self.is_alive() { return; }
                    permissions.insert(key.clone(), tx);
                }
                let _ = self.app.emit(
                    EVT_PERMISSION,
                    json!({
                        "requestId": key,
                        "agentType": self.agent_type,
                        "sessionId": session_id,
                        "params": params,
                    }),
                );
                // Do not block the protocol reader on a user decision: it must still
                // process prompt cancellation, responses and EOF.
                let conn = self.clone();
                tokio::spawn(async move {
                    let outcome = rx.await.unwrap_or_else(|_| json!({ "outcome": "cancelled" }));
                    if conn.is_alive() {
                        conn.respond(id, Ok(json!({ "outcome": outcome })));
                    }
                });
            }
            // ACP unstable elicitation（含常见别名）：agent 向用户请求结构化输入。
            // 与 request_permission 同构：事件到 UI，oneshot 等待用户作答；
            // 应答整体为 { action: "accept"|"decline"|"cancel", content?: {...} }，原样回给适配器。
            "elicitation/create" | "session/elicitation/create" | "elicitation/request" => {
                let session_id = params.get("sessionId").and_then(|s| s.as_str()).unwrap_or_default().to_string();
                let key = format!("{}:{}:{id}", self.agent_type, uuid::Uuid::new_v4());
                let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
                {
                    let mut m = self.elicitations.lock().unwrap();
                    if !self.is_alive() { return; }
                    m.insert(key.clone(), tx);
                }
                let _ = self.app.emit(
                    EVT_ELICIT,
                    json!({
                        "requestId": key,
                        "agentType": self.agent_type,
                        "sessionId": session_id,
                        "params": params,
                    }),
                );
                let conn = self.clone();
                tokio::spawn(async move {
                    let outcome = rx.await.unwrap_or_else(|_| json!({ "action": "cancel" }));
                    if conn.is_alive() {
                        conn.respond(id, Ok(outcome));
                    }
                });
            }
            "terminal/create" | "terminal/output" | "terminal/wait_for_exit" | "terminal/release" | "terminal/kill" => {
                self.respond(id, Err(json!({ "code": -32601, "message": "terminal not supported" })));
            }
            other => {
                log::debug!("[{}] unhandled server request: {other}", self.agent_type);
                self.respond(id, Err(json!({ "code": -32601, "message": "method not found" })));
            }
        }
    }

    fn respond(&self, id: Value, result: Result<Value, Value>) {
        let msg = match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
        };
        let _ = self.out_tx.send(msg.to_string() + "\n");
    }

    fn handle_session_update(&self, params: Value) {
        let session_id = params.get("sessionId").and_then(|s| s.as_str()).unwrap_or_default().to_string();
        // history capture (bind flow): buffer replay updates instead of emitting
        if self.capturing.lock().unwrap().contains(&session_id) {
            self.captures
                .lock()
                .unwrap()
                .entry(session_id)
                .or_default()
                .push(params.get("update").cloned().unwrap_or(Value::Null));
            return;
        }
        // discard replay during session/load
        if self.loading.lock().unwrap().contains(&session_id) {
            return;
        }
        let update = params.get("update").cloned().unwrap_or(Value::Null);
        let utype = update.get("sessionUpdate").and_then(|t| t.as_str()).unwrap_or_default();

        // accumulate transcript —— 只对有进行中回合（begin_turn 登记过）的会话。
        // 没有回合却收到正文/工具块的，只能是 session/load 重放在捕获窗口
        // (end_capture) / 静默窗口 (finish_load) 之后才姗姗来迟的尾巴，或 cancel 之后
        // 的残余 chunk。以前这里 entry().or_default() 兜住后照样 emit 给前端：WebView
        // 把整段历史当成实时流再追加一遍（永不结束的 streaming 条目、逐 chunk 重渲染），
        // 同时 Rust 侧 TurnBuffer 无人 end_turn 也一直涨——这是绑定长历史时 WebView
        // 内存持续上涨的一条路径。现在直接丢弃，非正文类通知（模式/命令/配置更新）照常透传。
        let is_transcript = matches!(
            utype,
            "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk" | "tool_call" | "tool_call_update" | "plan"
        );
        let mut flush_ctx: Option<String> = None;
        {
            let mut buffers = self.buffers.lock().unwrap();
            let Some(buf) = buffers.get_mut(&session_id) else {
                if is_transcript {
                    log::debug!("[{}] drop stray {utype} for session {session_id} (no active turn)", self.agent_type);
                    return;
                }
                drop(buffers);
                let _ = self.app.emit(
                    EVT_UPDATE,
                    json!({ "agentType": self.agent_type, "sessionId": session_id, "contextId": Value::Null, "update": update }),
                );
                return;
            };
            match utype {
                "agent_message_chunk" => {
                    if let Some(t) = chunk_text(&update) {
                        buf.text.push_str(&t);
                    }
                }
                "agent_thought_chunk" => {
                    if let Some(t) = chunk_text(&update) {
                        buf.thought.push_str(&t);
                    }
                }
                "tool_call" => {
                    buf.tools.push(update.clone());
                }
                "tool_call_update" => {
                    let tid = update.get("toolCallId").and_then(|t| t.as_str());
                    if let Some(existing) = buf.tools.iter_mut().find(|t| {
                        t.get("toolCallId").and_then(|x| x.as_str()) == tid
                            || t.get("update").and_then(|u| u.get("toolCallId")).and_then(|x| x.as_str()) == tid
                    }) {
                        // merge fields so the original title/kind survive updates
                        if let (Some(obj), Some(newobj)) = (existing.as_object_mut(), update.as_object()) {
                            for (k, v) in newobj {
                                obj.insert(k.clone(), v.clone());
                            }
                        }
                    } else {
                        buf.tools.push(update.clone());
                    }
                }
                _ => {}
            }
            if !matches!(utype, "user_message_chunk") {
                flush_ctx = buf.context_id.clone();
            }
        }

        // 工具调用负载只发前端真正渲染的部分（见 compact_tool_update）——实时回合里的
        // 命令输出 / diff / MCP 结果与绑定重放一样会在 WebView 里成倍放大并把本地快照撑爆
        let update = if matches!(utype, "tool_call" | "tool_call_update") { compact_tool_update(&update) } else { update };
        let _ = self.app.emit(
            EVT_UPDATE,
            json!({
                "agentType": self.agent_type,
                "sessionId": session_id,
                "contextId": flush_ctx,
                "update": update,
            }),
        );
    }

    // ---------- outgoing ----------

    pub async fn request(&self, method: &str, params: Value, timeout: Option<Duration>) -> Result<Value, String> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(format!("{method} 失败：适配器未连接"));
        }
        let id = Value::Number(self.next_id.fetch_add(1, Ordering::SeqCst).into());
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            // Recheck under the same lock used to drain on disconnect.
            if !self.is_alive() { return Err(format!("{method} 失败：适配器未连接")); }
            pending.insert(id.clone(), tx);
        }
        let _pending = PendingRequest { pending: self.pending.clone(), id: id.clone() };
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        // PendingRequest Drop 负责在断开时清理 pending 表
        self.out_tx
            .send(msg.to_string() + "\n")
            .map_err(|_| format!("{method} 失败：无法写入适配器"))?;

        let wait = async {
            rx.await.map_err(|_| format!("{method} 失败：适配器已断开"))?
        };
        match timeout {
            Some(d) => match tokio::time::timeout(d, wait).await {
                Ok(res) => res,
                Err(_) => {
                    // drop the stale sender so it doesn't leak until disconnect
                    self.pending.lock().unwrap().remove(&id);
                    Err(format!("{method} 超时（{}s）", d.as_secs()))
                }
            },
            None => wait.await,
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.out_tx.send(msg.to_string() + "\n");
    }

    /// Mark a session as loading (replayed updates are dropped).
    pub fn begin_load(&self, session_id: &str) {
        self.loading.lock().unwrap().insert(session_id.to_string());
    }
    /// Finish a load attempt. On success the session stays suppressed for a short
    /// settle window (history replay notifications arrive after the response) and
    /// is then marked loaded. On failure nothing is cached so binds can retry.
    pub fn finish_load(&self, session_id: &str, ok: bool) {
        let loading = self.loading.clone();
        let loaded = self.loaded.clone();
        let sid = session_id.to_string();
        if ok {
            loaded.lock().unwrap().insert(sid.clone());
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(2500)).await;
                loading.lock().unwrap().remove(&sid);
            });
        } else {
            loading.lock().unwrap().remove(&sid);
        }
    }

    /// Cache capability fragments from session/new or session/load responses.
    pub fn cache_caps(&self, res: &Value) {
        let mut cur = self.caps_cache.lock().unwrap();
        let mut obj = match cur.take() {
            Value::Object(o) => o,
            _ => serde_json::Map::new(),
        };
        for key in ["models", "modes", "configOptions"] {
            if let Some(v) = res.get(key) {
                if !v.is_null() {
                    obj.insert(key.to_string(), v.clone());
                }
            }
        }
        *cur = Value::Object(obj);
    }

    pub fn cached_caps(&self) -> Value {
        self.caps_cache.lock().unwrap().clone()
    }

    /// set_config_option 成功后同步能力缓存：适配器若在响应里给出新的 configOptions
    ///（codex-acp 会）就整体采纳，否则只把该项的 currentValue（以及 model 对应的
    /// currentModelId）改成刚设置的值。session-ready 事件原样携带 cached_caps，
    /// 不同步的话前端刚改的选项会被旧值刷回。
    pub fn note_config_option(&self, option_id: &str, value: &Value, res: &Value) {
        if res.get("configOptions").is_some_and(|v| v.is_array()) {
            self.cache_caps(res);
            return;
        }
        let mut cur = self.caps_cache.lock().unwrap();
        if let Some(opts) = cur.get_mut("configOptions").and_then(|v| v.as_array_mut()) {
            for o in opts.iter_mut() {
                if o.get("id").and_then(|i| i.as_str()) == Some(option_id) {
                    if let Some(obj) = o.as_object_mut() {
                        obj.insert("currentValue".into(), value.clone());
                    }
                }
            }
        }
        if option_id == "model" {
            if let Some(m) = cur.get_mut("models").and_then(|v| v.as_object_mut()) {
                m.insert("currentModelId".into(), value.clone());
            }
        }
    }

    /// set_mode 成功后同步缓存里的当前模式（理由同上）。
    pub fn note_mode(&self, mode_id: &str) {
        let mut cur = self.caps_cache.lock().unwrap();
        if let Some(m) = cur.get_mut("modes").and_then(|v| v.as_object_mut()) {
            m.insert("currentModeId".into(), Value::String(mode_id.to_string()));
        }
    }

    /// Start capturing session updates for a session (history replay on bind).
    pub fn begin_capture(&self, session_id: &str) {
        self.capturing.lock().unwrap().insert(session_id.to_string());
        self.captures.lock().unwrap().remove(session_id);
    }

    /// Stop capturing and return the buffered updates, waiting for the replay to settle.
    pub async fn end_capture(&self, session_id: &str) -> Vec<Value> {
        // 静默判定：先等 300ms 让重放启动，之后每 150ms 轮询，连续两次长度不变即认为平稳
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut last = -1i64;
        let mut stable = 0;
        for _ in 0..30 {
            let len = self.captures.lock().unwrap().get(session_id).map(|v| v.len() as i64).unwrap_or(0);
            if len == last {
                stable += 1;
                if stable >= 2 {
                    break;
                }
            } else {
                stable = 0;
            }
            last = len;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        self.capturing.lock().unwrap().remove(session_id);
        self.captures.lock().unwrap().remove(session_id).unwrap_or_default()
    }

    /// Serialized, replay-suppressed session/load with one retry. Adapters without
    /// session/load (e.g. DeepSeek Harness) fall back to session/resume.
    pub async fn load_session(&self, session_id: &str, cwd: &str, mcp_servers: Value) -> Result<Value, String> {
        let _g = self.load_lock.lock().await;
        let params = json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": mcp_servers });
        self.begin_load(session_id);
        let mut res = self.request("session/load", params.clone(), Some(Duration::from_secs(60))).await;
        if res.is_err() {
            // one retry after a short pause
            tokio::time::sleep(Duration::from_millis(600)).await;
            self.begin_load(session_id);
            res = self.request("session/load", params, Some(Duration::from_secs(60))).await;
        }
        // session/load 不支持时（-32601 / 明确不支持文案）退回 session/resume
        if let Some(err_text) = res.as_ref().err() {
            let unsupported = err_text.contains("-32601") || err_text.contains("not supported")
                || err_text.contains("不支持") || err_text.contains("Unsupported");
            if unsupported {
                let resume_params = json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": mcp_servers });
                self.begin_load(session_id);
                res = self
                    .request("session/resume", resume_params, Some(Duration::from_secs(60)))
                    .await
                    .map_err(|e| format!("session/resume 亦失败（适配器不支持 session/load）：{e}"));
            }
        }
        let ok = res.is_ok();
        let err = res.as_ref().err().cloned();
        self.finish_load(session_id, ok);
        res.map_err(|e| {
            let _ = err;
            e
        })
    }
    pub fn is_loaded(&self, session_id: &str) -> bool {
        self.loaded.lock().unwrap().contains(session_id)
    }
    /// Whether another session is currently loaded on this connection.
    pub fn has_other_loaded(&self, session_id: &str) -> bool {
        self.loaded.lock().unwrap().iter().any(|s| s != session_id)
    }
    pub fn mark_loaded(&self, session_id: &str) {
        self.loaded.lock().unwrap().insert(session_id.to_string());
    }

    /// Remember that the adapter rejected our injected MCP server (avoid retrying every turn).
    pub fn mark_mcp_degraded(&self) {
        let mut info = self.agent_info.lock().unwrap();
        if let Some(obj) = info.as_object_mut() {
            obj.insert("_shidriveMcpDegraded".into(), Value::Bool(true));
        }
    }

    pub fn mcp_degraded(&self) -> bool {
        self.agent_info
            .lock()
            .unwrap()
            .get("_shidriveMcpDegraded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// List sessions known to the adapter (session/list). Returns normalized entries.
    /// 支持游标分页：循环拉取直到没有 nextCursor（上限 50 页），避免会话列表不完整。
    pub async fn session_list(&self) -> Result<Vec<crate::models::SessionInfo>, String> {
        let mut arr: Vec<serde_json::Value> = Vec::new();
        let mut cursor: Option<String> = None;
        for _page in 0..50 {
            let mut params = json!({});
            if let Some(c) = &cursor {
                params["cursor"] = json!(c);
            }
            let res = self
                .request("session/list", params, Some(Duration::from_secs(30)))
                .await?;
            if let Some(a) = res.get("sessions").and_then(|s| s.as_array()) {
                arr.extend(a.iter().cloned());
            } else if let Some(a) = res.as_array() {
                arr.extend(a.iter().cloned());
            }
            cursor = res
                .get("nextCursor")
                .or_else(|| res.get("next_cursor"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if cursor.is_none() {
                break;
            }
        }
        let mut out = Vec::new();
        for item in arr {
            let sid = item
                .get("sessionId")
                .or_else(|| item.get("session_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let Some(session_id) = sid else { continue };
            out.push(crate::models::SessionInfo {
                session_id,
                title: item.get("title").and_then(|v| v.as_str()).map(|s| s.to_string()),
                updated_at: item
                    .get("updatedAt")
                    .or_else(|| item.get("updated_at"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                cwd: item
                    .get("cwd")
                    .or_else(|| item.get("workingDirectory"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            });
        }
        Ok(out)
    }

    /// Begin tracking a turn: returns previous buffer content (if any) and resets it.
    pub fn begin_turn(&self, session_id: &str, context_id: &str) {
        // A live prompt starts immediately after load. Its updates must not be
        // discarded by the replay settle window.
        self.loading.lock().unwrap().remove(session_id);
        let mut buffers = self.buffers.lock().unwrap();
        buffers.insert(
            session_id.to_string(),
            TurnBuffer { context_id: Some(context_id.to_string()), ..Default::default() },
        );
    }

    /// End the turn: persist accumulated transcript rows and return (assistant_text, thought, tools_json).
    pub fn end_turn(&self, session_id: &str) -> (String, String, String) {
        let buf = self.buffers.lock().unwrap().remove(session_id).unwrap_or_default();
        (
            buf.text.trim().to_string(),
            buf.thought.trim().to_string(),
            serde_json::to_string(&buf.tools).unwrap_or_else(|_| "[]".into()),
        )
    }

    /// Deliver the user's permission decision to the pending server request.
    pub fn resolve_permission(&self, request_id: &str, option_id: &str) -> Result<(), String> {
        if let Some(tx) = self.permissions.lock().unwrap().remove(request_id) {
            tx.send(json!({ "outcome": "selected", "optionId": option_id }))
                .map_err(|_| "权限请求已失效".to_string())
        } else {
            Err("权限请求已失效".into())
        }
    }

    /// Deliver the user's elicitation answer to the pending server request.
    pub fn resolve_elicitation(&self, request_id: &str, response: Value) -> Result<(), String> {
        if let Some(tx) = self.elicitations.lock().unwrap().remove(request_id) {
            tx.send(response).map_err(|_| "输入请求已失效".to_string())
        } else {
            Err("输入请求已失效".into())
        }
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn shutdown(&self) {
        self.on_closed();
    }
}

/// 发往 WebView 的工具输出（rawOutput）上限（字节）：MessageItem 展开详情时最多显示
/// 4000 字符输出，再长的部分前端本来就截断不显示。
pub const TOOL_OUTPUT_MAX_BYTES: usize = 16 * 1024;
/// 工具输入（rawInput）上限（字节）：详情里完整展示，放宽到 64 KB（整文件写入之类
/// 的超大入参之外都不受影响）。
pub const TOOL_INPUT_MAX_BYTES: usize = 64 * 1024;
/// 每个工具调用最多带几个定位（前端只显示前 2 个）。
const TOOL_LOCATIONS_MAX: usize = 8;

/// 把 tool_call / tool_call_update 压成前端渲染所需的最小负载。
///
/// 长历史一次 session/load 重放里，命令输出、整文件 diff、MCP 结果会以 content /
/// rawOutput / rawInput 三份原样进入 WebView：先是 IPC 字符串、再 JSON.parse、再进
/// `$state` 代理（Svelte 5 每读一个属性就生成一个 signal 常驻）、随后整份 JSON.stringify
/// 落本地快照——几十 MB 的历史在 WebView 里放大好几倍，且随聊天一直驻留。
/// 前端从不读取 `content`（MessageItem 只用 toolCallId/title/kind/status/locations/
/// rawInput/rawOutput），rawInput/rawOutput 超过上限的部分也永远不会展示。
/// 输出始终是合法 JSON 对象——这一点是 v0.3.9 用 cap_str 掐断工具行字符串时丢失的。
pub fn compact_tool_update(update: &Value) -> Value {
    let Some(obj) = update.as_object() else { return update.clone() };
    let mut out = serde_json::Map::with_capacity(obj.len());
    for (k, v) in obj {
        match k.as_str() {
            "content" => continue,
            "rawInput" => {
                out.insert(k.clone(), cap_json_value(v, TOOL_INPUT_MAX_BYTES));
            }
            "rawOutput" => {
                out.insert(k.clone(), cap_json_value(v, TOOL_OUTPUT_MAX_BYTES));
            }
            "locations" => {
                let v = match v.as_array() {
                    Some(a) if a.len() > TOOL_LOCATIONS_MAX => Value::Array(a[..TOOL_LOCATIONS_MAX].to_vec()),
                    _ => v.clone(),
                };
                out.insert(k.clone(), v);
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// 字符串超限截断；结构化值按紧凑 JSON 长度判断，超限时降级为截断后的字符串
/// （前端 toolDetail 对字符串/对象两种形态都能显示）。
fn cap_json_value(v: &Value, max: usize) -> Value {
    fn truncate(s: &str, max: usize) -> String {
        let mut end = max.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n…(截断，共 {} 字节)", &s[..end], s.len())
    }
    match v {
        Value::String(s) if s.len() <= max => v.clone(),
        Value::String(s) => Value::String(truncate(s, max)),
        Value::Null | Value::Bool(_) | Value::Number(_) => v.clone(),
        other => {
            let s = other.to_string();
            if s.len() <= max { v.clone() } else { Value::String(truncate(&s, max)) }
        }
    }
}

fn chunk_text(update: &Value) -> Option<String> {
    let content = update.get("content")?;
    match content {
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
        ),
        b => b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()).or(Some(b.to_string())),
    }
}

fn format_acp_error(err: &Value) -> String {
    let details = err
        .get("data")
        .map(|d| {
            if d.is_null() {
                String::new()
            } else {
                format!("（{d}）")
            }
        })
        .unwrap_or_default();
    match (err.get("message").and_then(|m| m.as_str()), err.get("code").and_then(|c| c.as_i64())) {
        (Some(m), Some(code)) => format!("{m} (code {code}){details}"),
        (Some(m), None) => format!("{m}{details}"),
        _ => format!("{}{details}", err),
    }
}

fn read_text_file_range(path: &str, line: Option<u64>, limit: Option<u64>) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&data);
    let mut out: Vec<&str> = Vec::new();
    if line.is_none() && limit.is_none() {
        return Ok(text.to_string());
    }
    let start = line.unwrap_or(1).max(1) as usize - 1;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let end = match limit {
        Some(l) => start.saturating_add(usize::try_from(l).unwrap_or(usize::MAX)).min(lines.len()),
        None => lines.len(),
    };
    if start < lines.len() {
        out.extend_from_slice(&lines[start..end]);
    }
    Ok(out.concat())
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn compact_tool_update_drops_content_and_caps_raw_fields() {
        let big = "x".repeat(TOOL_INPUT_MAX_BYTES * 2);
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t1",
            "title": "cat big.log",
            "kind": "read",
            "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": big } }],
            "rawInput": { "command": ["cat", "big.log"] },
            "rawOutput": big,
            "locations": (0..20).map(|i| json!({ "path": format!("f{i}.rs") })).collect::<Vec<_>>(),
        });
        let c = compact_tool_update(&update);
        assert!(c.get("content").is_none(), "content is never rendered by the webview");
        for k in ["sessionUpdate", "toolCallId", "title", "kind", "status"] {
            assert_eq!(c.get(k), update.get(k), "{k} must survive untouched");
        }
        assert_eq!(c["rawInput"], update["rawInput"], "small structured input is kept as-is");
        let out = c["rawOutput"].as_str().unwrap();
        assert!(out.len() < TOOL_OUTPUT_MAX_BYTES + 64, "rawOutput capped: {}", out.len());
        assert!(out.ends_with("字节)"), "truncation marker appended");
        // rawInput gets the larger budget but is still bounded
        let c2 = compact_tool_update(&json!({ "toolCallId": "w", "rawInput": { "path": "a.txt", "content": big } }));
        let inp = c2["rawInput"].as_str().expect("oversized input degraded to string");
        assert!(inp.len() > TOOL_OUTPUT_MAX_BYTES && inp.len() < TOOL_INPUT_MAX_BYTES + 64, "rawInput capped at 64K: {}", inp.len());
        assert_eq!(c["locations"].as_array().unwrap().len(), TOOL_LOCATIONS_MAX);
    }

    #[test]
    fn compact_tool_update_caps_structured_output_and_respects_utf8() {
        // structured rawOutput above the cap degrades to a truncated string,
        // never splitting a multi-byte char
        let rows: Vec<Value> = (0..4000).map(|i| json!({ "i": i, "s": "多字节字符串" })).collect();
        let update = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t2", "rawOutput": rows });
        let c = compact_tool_update(&update);
        let out = c["rawOutput"].as_str().expect("degraded to string");
        assert!(out.len() < TOOL_OUTPUT_MAX_BYTES + 64);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        // untouched when the payload is small, non-object updates pass through
        let small = json!({ "sessionUpdate": "tool_call", "toolCallId": "t3", "rawOutput": { "ok": true } });
        assert_eq!(compact_tool_update(&small), small);
        assert_eq!(compact_tool_update(&Value::Null), Value::Null);
    }

    #[test]
    fn dropped_request_removes_pending_sender() {
        let pending: PendingMap = Default::default();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        pending.lock().unwrap().insert(Value::from(7), tx);
        { let _request = PendingRequest { pending: pending.clone(), id: Value::from(7) }; }
        assert!(pending.lock().unwrap().is_empty());
        assert!(matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed)));
    }

    #[test]
    fn launch_env_preserves_override_and_prepends_path() {
        let mut cmd = tokio::process::Command::new("unused");
        let prefix = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixture-bin");
        let env = std::collections::BTreeMap::from([
            ("PATH".into(), prefix.to_string_lossy().to_string()),
            ("ZCODE_NODE".into(), "custom-node".into()),
            ("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE".into(), "custom-provider".into()),
        ]);
        apply_launch_env(&mut cmd, &env);
        let vars: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
        assert_eq!(vars[std::ffi::OsStr::new("ZCODE_NODE")], Some(std::ffi::OsStr::new("custom-node")));
        assert_eq!(vars[std::ffi::OsStr::new("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE")], Some(std::ffi::OsStr::new("custom-provider")));
        assert_eq!(std::env::split_paths(vars[std::ffi::OsStr::new("PATH")].unwrap()).next(), Some(prefix));
    }

    #[test]
    fn file_range_accepts_unbounded_limit_without_overflow() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/acp.rs");
        let full = read_text_file_range(path, None, None).unwrap();
        let ranged = read_text_file_range(path, Some(2), Some(u64::MAX)).unwrap();
        assert_eq!(ranged, full.split_inclusive('\n').skip(1).collect::<String>());
    }
}

