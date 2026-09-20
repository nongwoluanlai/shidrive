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

type PendingMap = Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<Value, String>>>>>;
/// permission_key ("{agent_type}:{rpc_id}") -> resolver delivering the chosen option id
type PermissionMap = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Value>>>>;

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
    next_id: AtomicI64,
    pending: PendingMap,
    permissions: PermissionMap,
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
            .stderr(std::process::Stdio::piped());
        for (k, v) in &launch.env {
            if k.eq_ignore_ascii_case("path") {
                let merged = match std::env::var("PATH") {
                    Ok(cur) => format!("{v};{cur}"),
                    Err(_) => v.clone(),
                };
                cmd.env("PATH", merged);
            } else {
                cmd.env(k, v);
            }
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = cmd.spawn().map_err(|e| format!("启动适配器失败 ({}): {e}", launch.command))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
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
        tauri::async_runtime::spawn(async move {
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
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            permissions: Arc::new(Mutex::new(HashMap::new())),
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
        Ok(conn)
    }

    fn spawn_reader(self: &Arc<Self>, stdout: tokio::process::ChildStdout) {
        let conn = self.clone();
        tauri::async_runtime::spawn(async move {
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
    }

    fn on_closed(self: &Arc<Self>) {
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
        let id = msg.get("id").and_then(|v| {
            v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)).or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });

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

    async fn handle_server_request(self: &Arc<Self>, id: i64, method: &str, params: Value) {
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
                let key = format!("{}:{id}", self.agent_type);
                let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
                self.permissions.lock().unwrap().insert(key.clone(), tx);
                let _ = self.app.emit(
                    EVT_PERMISSION,
                    json!({
                        "requestId": key,
                        "agentType": self.agent_type,
                        "sessionId": session_id,
                        "params": params,
                    }),
                );
                let outcome = rx.await.unwrap_or(json!({ "optionId": "__rejected__" }));
                if outcome.get("optionId").and_then(|o| o.as_str()) == Some("__connection_closed__") {
                    self.respond(id, Err(json!({ "code": -32000, "message": "连接已关闭" })));
                } else {
                    self.respond(id, Ok(outcome));
                }
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

    fn respond(&self, id: i64, result: Result<Value, Value>) {
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

        // accumulate transcript
        let mut flush_ctx: Option<String> = None;
        {
            let mut buffers = self.buffers.lock().unwrap();
            let buf = buffers.entry(session_id.clone()).or_default();
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
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
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
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(2500)).await;
                loading.lock().unwrap().remove(&sid);
                loaded.lock().unwrap().insert(sid);
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

    /// Serialized, replay-suppressed session/load with one retry.
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
    pub async fn session_list(&self) -> Result<Vec<crate::models::SessionInfo>, String> {
        let res = self
            .request("session/list", json!({}), Some(Duration::from_secs(20)))
            .await?;
        let arr = if let Some(a) = res.get("sessions").and_then(|s| s.as_array()) {
            a.clone()
        } else if let Some(a) = res.as_array() {
            a.clone()
        } else {
            Vec::new()
        };
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
            let _ = tx.send(json!({ "optionId": option_id }));
            Ok(())
        } else {
            // waiter gone (e.g. restart): answer directly on the wire
            let rpc_id: i64 = request_id
                .rsplit(':')
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| "无效的权限请求 ID".to_string())?;
            self.respond(rpc_id, Ok(json!({ "optionId": option_id })));
            Err("权限请求已失效，已直接拒绝应答".into())
        }
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn shutdown(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.notify("session/close", json!({}));
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.start_kill();
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
        Some(l) => (start + l as usize).min(lines.len()),
        None => lines.len(),
    };
    if start < lines.len() {
        out.extend_from_slice(&lines[start..end]);
    }
    Ok(out.concat())
}
