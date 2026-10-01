//! ShiDrive host integration around the official ACP Rust SDK (stable protocol v1).
//!
//! The SDK owns JSON-RPC framing/dispatch, IDs and pending replies. This module
//! owns the process, session routing, UI decisions and lifecycle policies.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agent_client_protocol::{
    schema::{v1 as protocol, ProtocolVersion},
    Agent, Client, ConnectionTo, Error as SdkError, JsonRpcRequest, Lines, UntypedMessage,
};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

#[path = "acp_compat.rs"]
mod compat;
#[path = "acp_file_tasks.rs"]
mod file_tasks;
use tauri::{AppHandle, Emitter};

use crate::models::AgentLaunch;

pub const EVT_UPDATE: &str = "acp://update";
pub const EVT_STATUS: &str = "acp://status";
pub const EVT_PERMISSION: &str = "acp://permission";
/// ACP unstable elicitation：agent 请求用户输入（选项/自由文本），ACP 规范尚未转正，
/// 适配器侧以 MCP elicitation 形状桥接（message + requestedSchema → {action, content}）
pub const EVT_ELICIT: &str = "acp://elicitation";
pub const EVT_REQUESTS_CANCELLED: &str = "acp://requests-cancelled";

/// Requests retain their session identity so cancellation cannot affect another turn.
struct UserRequest {
    session_id: String,
    tx: tokio::sync::oneshot::Sender<Value>,
}
/// One lock orders gate changes, insertion/removal and UI events. A cancelled
/// SID stays blocked after acknowledgement until a new owned turn is accepted.
#[derive(Default)]
struct UserRequests {
    permissions: HashMap<String, UserRequest>,
    elicitations: HashMap<String, UserRequest>,
    cancelled_sessions: HashSet<String>,
}

/// Match adapter and preflight environment, including PATH prefix semantics.
pub(crate) fn apply_launch_env(
    cmd: &mut tokio::process::Command,
    env: &std::collections::BTreeMap<String, String>,
) {
    cmd.env("PYTHONUNBUFFERED", "1");
    for (key, value) in env {
        if key.eq_ignore_ascii_case("PATH") {
            let mut paths: Vec<_> = std::env::split_paths(value).collect();
            if let Some(current) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&current));
            }
            cmd.env(
                "PATH",
                std::env::join_paths(paths).unwrap_or_else(|_| value.into()),
            );
        } else {
            cmd.env(key, value);
        }
    }
}

/// A pending *UI decision*, not a JSON-RPC pending response. A dropped SDK
/// callback must also remove its dialog, even if the transport is still alive.
struct UserDecisionGuard {
    connection: Weak<AcpConnection>,
    request_id: String,
}
impl Drop for UserDecisionGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.connection.upgrade() {
            conn.cancel_user_request(&self.request_id);
        }
    }
}

/// Accumulated transcript of the current turn for one session (persisted on turn end).
#[derive(Default)]
struct TurnBuffer {
    context_id: Option<String>,
    turn_id: String,
    source: String,
    text: String,
    thought: String,
    tools: Vec<Value>,
}

pub struct AcpConnection {
    pub agent_type: String,
    /// Distinguishes session ids reused after a process restart.
    pub connection_id: String,
    sdk: Mutex<Option<ConnectionTo<Agent>>>,
    closed: tokio::sync::watch::Sender<bool>,
    child: Mutex<Option<tokio::process::Child>>,
    io_tasks: Mutex<Vec<tokio::task::AbortHandle>>,
    requests: Mutex<UserRequests>,
    file_tasks: Arc<file_tasks::FileTasks>,
    buffers: Arc<Mutex<HashMap<String, TurnBuffer>>>,
    /// sessions currently being loaded (replay updates are discarded)
    loading: Arc<Mutex<HashSet<String>>>,
    /// sessions successfully loaded/created on this connection (skip redundant session/load)
    loaded: Arc<Mutex<HashSet<String>>>,
    /// serialize session/load operations (adapters dislike concurrent loads)
    load_lock: Arc<tokio::sync::Mutex<()>>,
    /// cached capability fragments (models/modes/configOptions) from session responses
    caps_cache: Mutex<HashMap<String, Value>>,
    /// sessions whose updates are being captured (history replay on bind)
    capturing: Arc<Mutex<HashSet<String>>>,
    captures: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    pub agent_info: Mutex<Value>,
    alive: AtomicBool,
    lifecycle: Mutex<()>,
    app: AppHandle,
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        self.on_closed();
    }
}

impl AcpConnection {
    /// Spawn the adapter process and perform the ACP handshake.
    pub async fn spawn(
        app: AppHandle,
        agent_type: &str,
        launch: &AgentLaunch,
    ) -> Result<Arc<Self>, String> {
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

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动适配器失败 ({}): {e}", launch.command))?;
        crate::child_job::attach(&child); // 随宿主退出回收（含其派生的 codex 等孙进程）
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

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

        let (closed, _) = tokio::sync::watch::channel(false);
        let conn = Arc::new(Self {
            agent_type: agent_type.to_string(),
            connection_id: uuid::Uuid::new_v4().to_string(),
            sdk: Mutex::new(None),
            closed,
            child: Mutex::new(Some(child)),
            io_tasks: Mutex::new(vec![stderr_task.abort_handle()]),
            requests: Mutex::new(UserRequests::default()),
            file_tasks: file_tasks::FileTasks::new(),
            buffers: Arc::new(Mutex::new(HashMap::new())),
            loading: Arc::new(Mutex::new(HashSet::new())),
            loaded: Arc::new(Mutex::new(HashSet::new())),
            load_lock: Arc::new(tokio::sync::Mutex::new(())),
            caps_cache: Mutex::new(HashMap::new()),
            capturing: Arc::new(Mutex::new(HashSet::new())),
            captures: Arc::new(Mutex::new(HashMap::new())),
            agent_info: Mutex::new(Value::Null),
            alive: AtomicBool::new(true),
            lifecycle: Mutex::new(()),
            app,
        });

        conn.emit_status("connecting", None);
        // Also closes the process when the initialize future is cancelled.
        struct InitGuard(Option<Arc<AcpConnection>>);
        impl Drop for InitGuard {
            fn drop(&mut self) {
                if let Some(conn) = self.0.take() {
                    conn.shutdown();
                }
            }
        }
        let mut guard = InitGuard(Some(conn.clone()));

        let ready = conn.start_sdk(stdin, stdout);
        ready
            .await
            .map_err(|_| "ACP SDK 连接任务未启动".to_string())?;
        let request = protocol::InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(
                protocol::ClientCapabilities::new()
                    .fs(protocol::FileSystemCapabilities::new()
                        .read_text_file(true)
                        .write_text_file(true))
                    .terminal(false),
            )
            .client_info(protocol::Implementation::new(
                "shidrive",
                env!("CARGO_PKG_VERSION"),
            ));
        let init = conn
            .sdk_request(compat::Preserve(request), Some(Duration::from_secs(30)))
            .await?;
        if init.typed.protocol_version != ProtocolVersion::V1 {
            return Err(format!(
                "适配器返回了不支持的 ACP 协议版本：{}（当前使用 v1）",
                init.typed.protocol_version
            ));
        }
        {
            // Serialize connected/disconnected events with driver shutdown.
            let _lifecycle = conn.lifecycle.lock().unwrap();
            if !conn.is_alive() {
                return Err("适配器在 initialize 完成前断开了连接".into());
            }
            conn.emit_status("connected", Some(&init.raw));
            *conn.agent_info.lock().unwrap() = init.raw;
        }
        guard.0 = None;
        Ok(conn)
    }

    fn start_sdk(
        self: &Arc<Self>,
        stdin: tokio::process::ChildStdin,
        stdout: tokio::process::ChildStdout,
    ) -> tokio::sync::oneshot::Receiver<()> {
        // Standard Tokio codec supplies a bounded line stream. The SDK's Lines
        // transport, not ShiDrive code, parses and writes JSON-RPC frames.
        const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
        let transport = Lines::new(
            SinkExt::<String>::sink_map_err(
                FramedWrite::new(stdin, LinesCodec::new()),
                std::io::Error::other,
            ),
            FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_LINE_BYTES))
                .map(|line| line.map_err(std::io::Error::other)),
        );
        let notify_conn = Arc::downgrade(self);
        let permission_conn = Arc::downgrade(self);
        let elicitation_conn = Arc::downgrade(self);
        let write_conn = Arc::downgrade(self);
        let read_files = self.file_tasks.clone();
        let write_files = self.file_tasks.clone();
        let close_conn = Arc::downgrade(self);
        let ready_conn = Arc::downgrade(self);
        let exit_conn = Arc::downgrade(self);
        let mut stopped = self.closed.subscribe();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let builder = Client.builder().name("shidrive-acp-v1")
            .on_receive_notification(async move |notification: compat::SessionUpdate, _cx| {
                if let Some(conn) = notify_conn.upgrade() { conn.handle_session_update(notification); }
                Ok(())
            }, agent_client_protocol::on_receive_notification!())
            .on_receive_request(async move |request: protocol::ReadTextFileRequest, responder, cx| {
                let job = read_files.reserve(&request.session_id.to_string(), request.path.as_os_str().len())?;
                let cancellation = responder.cancellation();
                cx.spawn(async move {
                    let result = job.run(cancellation, move || {
                        read_text_file_range(&request.path.to_string_lossy(), request.line.map(u64::from), request.limit.map(u64::from))
                    }).await.map(protocol::ReadTextFileResponse::new);
                    let _ = responder.respond_with_result(result);
                    Ok(())
                })
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async move |request: compat::WriteTextFile, responder, cx| {
                let request = request.0;
                let job = write_files.reserve(&request.session_id.to_string(), request.content.len().saturating_add(request.path.as_os_str().len()))?;
                let cancellation = responder.cancellation();
                let weak = write_conn.clone();
                cx.spawn(async move {
                    let path = request.path.clone();
                    let result = job.run(cancellation, move || {
                        std::fs::write(request.path, request.content).map_err(|e| e.to_string())
                    }).await;
                    if result.is_ok() {
                        if let Some(conn) = weak.upgrade().filter(|conn| conn.is_alive()) {
                            let _ = conn.app.emit(EVT_UPDATE, json!({ "agentType": conn.agent_type, "fileChanged": path }));
                        }
                    }
                    let _ = responder.respond_with_result(result.map(|_| protocol::WriteTextFileResponse::new()));
                    Ok(())
                })
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async move |request: protocol::RequestPermissionRequest, responder, cx| {
                let conn = permission_conn.upgrade().ok_or_else(SdkError::internal_error)?;
                let Some((request_id, rx)) = conn.enqueue_decision(&request.session_id.to_string(), serde_json::to_value(request)?, false)? else {
                    return responder.respond(protocol::RequestPermissionResponse::new(protocol::RequestPermissionOutcome::Cancelled));
                };
                let guard = UserDecisionGuard { connection: permission_conn.clone(), request_id };
                let cancellation = responder.cancellation();
                drop(conn);
                cx.spawn(async move {
                    let _guard = guard;
                    let outcome = tokio::select! {
                        value = rx => serde_json::from_value::<protocol::RequestPermissionResponse>(json!({
                            "outcome": value.unwrap_or_else(|_| json!({"outcome":"cancelled"}))
                        })).map_err(SdkError::from),
                        _ = cancellation.cancelled() => Err(SdkError::request_cancelled()),
                    };
                    let _ = responder.respond_with_result(outcome);
                    Ok(())
                })
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async move |request: compat::Elicitation, responder, cx| {
                let conn = elicitation_conn.upgrade().ok_or_else(SdkError::internal_error)?;
                let sid = request.params.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_string();
                let Some((request_id, rx)) = conn.enqueue_decision(&sid, request.params, true)? else {
                    return responder.respond(json!({"action":"cancel"}));
                };
                let guard = UserDecisionGuard { connection: elicitation_conn.clone(), request_id };
                let cancellation = responder.cancellation();
                drop(conn);
                cx.spawn(async move {
                    let _guard = guard;
                    let outcome = tokio::select! {
                        value = rx => Ok(value.unwrap_or_else(|_| json!({"action":"cancel"}))),
                        _ = cancellation.cancelled() => Err(SdkError::request_cancelled()),
                    };
                    let _ = responder.respond_with_result(outcome);
                    Ok(())
                })
            }, agent_client_protocol::on_receive_request!())
            .on_close(async move |_cx| {
                if let Some(conn) = close_conn.upgrade() { conn.on_closed(); }
                Ok(())
            });
        let driver = builder.connect_with(transport, async move |cx| {
            if let Some(conn) = ready_conn.upgrade() {
                *conn.sdk.lock().unwrap() = Some(cx.clone());
            }
            let _ = ready_tx.send(());
            cx.incoming_closed().await;
            Ok(())
        });
        let task = tokio::spawn(async move {
            // EOF, parse/IO failure, explicit disconnect AND a dropped driver
            // all clear the host-owned permission/elicitation queues.
            struct DriverExit(Weak<AcpConnection>);
            impl Drop for DriverExit {
                fn drop(&mut self) {
                    if let Some(conn) = self.0.upgrade() {
                        conn.on_closed();
                    }
                }
            }
            let _exit = DriverExit(exit_conn);
            tokio::select! {
                result = driver => if let Err(error) = result { log::debug!("ACP SDK connection ended: {error}"); },
                _ = stopped.changed() => {},
            }
        });
        self.io_tasks.lock().unwrap().push(task.abort_handle());
        ready_rx
    }

    fn on_closed(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        // JSON-RPC pending replies belong to the SDK. Wake application callers
        // as well, including ones waiting during an explicit local shutdown.
        self.file_tasks.close();
        self.closed.send_replace(true);
        self.sdk.lock().unwrap().take();
        // Close both UI queues, including unscoped legacy elicitations.
        self.cancel_user_requests(None);
        // Windows：先按 PID 终止整棵进程树（适配器派生的 codex/node 孙进程
        // 不随直接子进程死亡）；start_kill 作为兜底
        #[cfg(windows)]
        if let Some(pid) = self.child.lock().unwrap().as_ref().and_then(|c| c.id()) {
            let _ = crate::setup::hide_console(&mut std::process::Command::new("taskkill"))
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output();
        }
        // Drop the handle after requesting termination: Tokio then reaps the
        // child even while the manager still caches this closed connection.
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
        for task in self.io_tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        self.emit_status("disconnected", None);
    }

    fn emit_status(&self, state: &str, info: Option<&Value>) {
        let _ = self.app.emit(
            EVT_STATUS,
            json!({ "agentType": self.agent_type, "state": state, "info": info.cloned().unwrap_or(Value::Null) }),
        );
    }

    fn enqueue_decision(
        &self,
        session_id: &str,
        params: Value,
        elicitation: bool,
    ) -> Result<Option<(String, tokio::sync::oneshot::Receiver<Value>)>, SdkError> {
        let mut requests = self.requests.lock().unwrap();
        if !self.is_alive() {
            return Err(SdkError::internal_error().data("适配器连接已断开"));
        }
        // Empty SID is an explicitly connection-scoped legacy extension. Do
        // not guess a current turn or cancel it when an unrelated SID stops.
        if !session_id.is_empty() && requests.cancelled_sessions.contains(session_id) {
            return Ok(None);
        }
        let key = format!("{}:{}", self.agent_type, uuid::Uuid::new_v4());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (map, event) = if elicitation {
            (&mut requests.elicitations, EVT_ELICIT)
        } else {
            (&mut requests.permissions, EVT_PERMISSION)
        };
        map.insert(
            key.clone(),
            UserRequest {
                session_id: session_id.into(),
                tx,
            },
        );
        // Same lock for the gate, insertion, cancellation and emission: no
        // expired dialog can be published after its cancellation event.
        let _ = self.app.emit(event, json!({
            "requestId": key, "agentType": self.agent_type, "sessionId": session_id, "params": params
        }));
        Ok(Some((key, rx)))
    }

    fn cancel_user_request(&self, request_id: &str) {
        let mut guard = self.requests.lock().unwrap();
        let requests = &mut *guard;
        for (map, outcome) in [
            (&mut requests.permissions, json!({"outcome":"cancelled"})),
            (&mut requests.elicitations, json!({"action":"cancel"})),
        ] {
            if let Some(request) = map.remove(request_id) {
                let _ = request.tx.send(outcome);
                let _ = self.app.emit(EVT_REQUESTS_CANCELLED, json!({
                    "agentType": self.agent_type, "connectionId": self.connection_id, "requestIds": [request_id]
                }));
            }
        }
    }

    fn handle_session_update(&self, notification: compat::SessionUpdate) {
        let session_id = notification.typed.session_id.to_string();
        let raw = notification
            .raw
            .get("update")
            .cloned()
            .unwrap_or(Value::Null);
        let utype = raw
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let update = if matches!(utype.as_str(), "tool_call" | "tool_call_update") {
            compact_tool_update(&raw)
        } else {
            raw
        };
        if self.capturing.lock().unwrap().contains(&session_id) {
            self.captures
                .lock()
                .unwrap()
                .entry(session_id)
                .or_default()
                .push(update);
            return;
        }
        if self.loading.lock().unwrap().contains(&session_id) {
            return;
        }
        if utype == "config_option_update" {
            self.cache_caps(&session_id, &update);
        }
        if utype == "current_mode_update" {
            if let Some(mode) = update.get("currentModeId").and_then(Value::as_str) {
                self.note_mode(&session_id, mode);
            }
        }
        let is_transcript = matches!(
            utype.as_str(),
            "agent_message_chunk"
                | "agent_thought_chunk"
                | "user_message_chunk"
                | "tool_call"
                | "tool_call_update"
                | "plan"
        );
        let route = {
            let mut buffers = self.buffers.lock().unwrap();
            if let Some(buf) = buffers.get_mut(&session_id) {
                match utype.as_str() {
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
                    "tool_call" | "tool_call_update" => {
                        let tid = update.get("toolCallId");
                        if let Some(existing) =
                            buf.tools.iter_mut().find(|t| t.get("toolCallId") == tid)
                        {
                            if let (Some(obj), Some(fields)) =
                                (existing.as_object_mut(), update.as_object())
                            {
                                for (k, v) in fields {
                                    if !v.is_null() {
                                        obj.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                        } else {
                            buf.tools.push(update.clone());
                        }
                    }
                    _ => {}
                }
                Some((
                    buf.context_id.clone(),
                    buf.turn_id.clone(),
                    buf.source.clone(),
                ))
            } else {
                None
            }
        };
        if is_transcript && route.is_none() {
            log::debug!(
                "[{}] drop stray {utype} for session {session_id} (no active turn)",
                self.agent_type
            );
            return;
        }
        let (context_id, turn_id, source) = route.unwrap_or((None, String::new(), "chat".into()));
        // Workflow output belongs to the run, never to a context's unrelated binding.
        let event = if source == "workflow" {
            "acp://workflow-update"
        } else {
            EVT_UPDATE
        };
        let _ = self.app.emit(
            event,
            json!({
                "agentType": self.agent_type, "connectionId": self.connection_id,
                "sessionId": session_id, "contextId": context_id, "turnId": turn_id,
                "source": source, "update": update,
            }),
        );
    }

    // ---------- outgoing ----------

    /// Typed SDK requests only; request IDs/pending responders are SDK-owned.
    /// This future is polled by manager tasks, never by the SDK dispatch handler.
    async fn sdk_request<R: JsonRpcRequest>(
        &self,
        request: R,
        timeout: Option<Duration>,
    ) -> Result<R::Response, String> {
        let sdk = self.sdk.lock().unwrap().clone().ok_or("适配器连接已断开")?;
        let method = request.method().to_string();
        let mut closed = self.closed.subscribe();
        if !self.is_alive() {
            return Err("适配器连接已断开".into());
        }
        let wait = async {
            tokio::select! {
                // A response already delivered by the SDK wins a simultaneous
                // EOF; otherwise a valid final prompt result could be lost.
                biased;
                result = sdk.send_request(request).block_task() => result.map_err(format_acp_error),
                _ = closed.changed() => Err("适配器连接已断开".into()),
            }
        };
        match timeout {
            Some(duration) => tokio::time::timeout(duration, wait)
                .await
                .map_err(|_| format!("{method} 超时（{}s）", duration.as_secs()))?,
            None => wait.await,
        }
    }

    async fn preserving<R: JsonRpcRequest>(
        &self,
        request: R,
        timeout: Option<Duration>,
    ) -> Result<Value, String> {
        self.sdk_request(compat::Preserve(request), timeout)
            .await
            .map(|r| r.raw)
    }

    pub async fn new_session(
        &self,
        cwd: &str,
        mcp_servers: Vec<protocol::McpServer>,
    ) -> Result<Value, String> {
        self.preserving(
            protocol::NewSessionRequest::new(cwd).mcp_servers(mcp_servers),
            Some(Duration::from_secs(90)),
        )
        .await
    }

    pub async fn close_session(&self, sid: &str) -> Result<Value, String> {
        self.preserving(
            protocol::CloseSessionRequest::new(sid.to_string()),
            Some(Duration::from_secs(30)),
        )
        .await
    }

    pub async fn prompt_session(
        &self,
        sid: &str,
        blocks: Vec<protocol::ContentBlock>,
    ) -> Result<Value, String> {
        self.preserving(protocol::PromptRequest::new(sid.to_string(), blocks), None)
            .await
    }

    /// Atomically stop admissions for the SID before sending cancellation.
    pub fn cancel_session(&self, sid: &str) {
        self.cancel_matching_turn(sid, None);
    }

    /// Owned background turns cannot cancel a newer turn reusing the same SID.
    pub fn cancel_turn(&self, sid: &str, turn_id: &str) {
        self.cancel_matching_turn(sid, Some(turn_id));
    }

    fn cancel_matching_turn(&self, sid: &str, expected: Option<&str>) {
        // Lock order: buffers -> requests -> file scope / SDK. Holding the
        // buffers lock through notify also orders cancel against begin_turn.
        let buffers = self.buffers.lock().unwrap();
        if expected.is_some_and(|id| !buffers.get(sid).is_some_and(|b| b.turn_id == id)) {
            return;
        }
        let mut requests = self.requests.lock().unwrap();
        requests.cancelled_sessions.insert(sid.into());
        self.file_tasks.cancel_session(sid);
        self.drain_user_requests(&mut requests, Some(sid));
        if let Some(sdk) = self.sdk.lock().unwrap().as_ref() {
            if let Err(e) =
                sdk.send_notification(protocol::CancelNotification::new(sid.to_string()))
            {
                log::debug!("ACP cancel could not be queued: {e}");
            }
        }
    }

    pub async fn set_session_mode(&self, sid: &str, mode: &str) -> Result<Value, String> {
        self.preserving(
            protocol::SetSessionModeRequest::new(sid.to_string(), mode.to_string()),
            Some(Duration::from_secs(15)),
        )
        .await
    }

    pub async fn set_session_config(
        &self,
        sid: &str,
        id: &str,
        value: &Value,
    ) -> Result<Value, String> {
        let value = match value {
            Value::String(s) => protocol::SessionConfigOptionValue::value_id(s.clone()),
            Value::Bool(b) => protocol::SessionConfigOptionValue::boolean(*b),
            _ => return Err("ACP 配置值必须是选项 ID 或布尔值".into()),
        };
        self.preserving(
            protocol::SetSessionConfigOptionRequest::new(sid.to_string(), id.to_string(), value),
            Some(Duration::from_secs(15)),
        )
        .await
    }

    /// Deliberate extension/test escape hatch. Standard ACP operations above
    /// never use stringly typed requests. Framing and correlation remain SDK-owned.
    pub async fn extension_request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, String> {
        if !method.starts_with('_') {
            return Err("扩展 ACP 方法名必须以下划线开头".into());
        }
        let request = UntypedMessage::new(method, params).map_err(format_acp_error)?;
        self.sdk_request(request, timeout).await
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
    pub fn cache_caps(&self, session_id: &str, res: &Value) {
        let mut cache = self.caps_cache.lock().unwrap();
        let cur = cache.entry(session_id.to_string()).or_insert(Value::Null);
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

    pub fn cached_caps(&self, session_id: &str) -> Value {
        self.caps_cache.lock().unwrap().get(session_id).cloned().unwrap_or(Value::Null)
    }

    /// set_config_option 成功后同步能力缓存：适配器若在响应里给出新的 configOptions
    ///（codex-acp 会）就整体采纳，否则只把该项的 currentValue（以及 model 对应的
    /// currentModelId）改成刚设置的值。session-ready 事件原样携带 cached_caps，
    /// 不同步的话前端刚改的选项会被旧值刷回。
    pub fn note_config_option(&self, session_id: &str, option_id: &str, value: &Value, res: &Value) {
        if res.get("configOptions").is_some_and(|v| v.is_array()) {
            self.cache_caps(session_id, res);
            return;
        }
        let mut cache = self.caps_cache.lock().unwrap();
        let cur = cache.entry(session_id.to_string()).or_insert_with(|| json!({}));
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
    pub fn note_mode(&self, session_id: &str, mode_id: &str) {
        let mut cache = self.caps_cache.lock().unwrap();
        let cur = cache.entry(session_id.to_string()).or_insert_with(|| json!({}));
        if let Some(m) = cur.get_mut("modes").and_then(|v| v.as_object_mut()) {
            m.insert("currentModeId".into(), Value::String(mode_id.to_string()));
        }
    }

    /// Start capturing session updates for a session (history replay on bind).
    pub fn begin_capture(&self, session_id: &str) {
        self.capturing.lock().unwrap().insert(session_id.to_string());
        self.captures.lock().unwrap().remove(session_id);
    }

    pub fn abort_capture(&self, session_id: &str) {
        self.capturing.lock().unwrap().remove(session_id);
        self.captures.lock().unwrap().remove(session_id);
        self.loading.lock().unwrap().remove(session_id);
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
    pub async fn load_session(&self, session_id: &str, cwd: &str, mcp_servers: Vec<protocol::McpServer>) -> Result<Value, String> {
        let _g = self.load_lock.lock().await;
        struct LoadGuard<'a>(&'a AcpConnection, &'a str, bool);
        impl Drop for LoadGuard<'_> {
            fn drop(&mut self) {
                if !self.2 { self.0.loading.lock().unwrap().remove(self.1); }
            }
        }
        let mut guard = LoadGuard(self, session_id, false);
        // 按 initialize 声明的能力选择：未声明 loadSession 但声明了 sessionCapabilities.resume
        // （官方 dsh）时直接 resume，省掉必然失败的 load + 重试往返。
        let resume_only = {
            let info = self.agent_info.lock().unwrap();
            let caps = &info["agentCapabilities"];
            caps["loadSession"].as_bool() != Some(true) && caps["sessionCapabilities"].get("resume").is_some()
        };
        if resume_only {
            let resume = protocol::ResumeSessionRequest::new(session_id.to_string(), cwd).mcp_servers(mcp_servers);
            self.begin_load(session_id);
            let res = self.preserving(resume, Some(Duration::from_secs(60))).await
                .map_err(|e| format!("session/resume 失败：{e}"));
            if let Ok(response) = &res { self.cache_caps(session_id, response); }
            let ok = res.is_ok();
            self.finish_load(session_id, ok);
            guard.2 = true;
            return res;
        }
        let request = protocol::LoadSessionRequest::new(session_id.to_string(), cwd).mcp_servers(mcp_servers.clone());
        self.begin_load(session_id);
        let mut res = self.preserving(request.clone(), Some(Duration::from_secs(60))).await;
        if res.is_err() {
            // one retry after a short pause
            tokio::time::sleep(Duration::from_millis(600)).await;
            self.begin_load(session_id);
            res = self.preserving(request, Some(Duration::from_secs(60))).await;
        }
        // session/load 不支持时（-32601 / 明确不支持文案）退回 session/resume
        if let Some(err_text) = res.as_ref().err() {
            let unsupported = err_text.contains("-32601") || err_text.contains("not supported")
                || err_text.contains("不支持") || err_text.contains("Unsupported");
            if unsupported {
                let resume = protocol::ResumeSessionRequest::new(session_id.to_string(), cwd).mcp_servers(mcp_servers);
                self.begin_load(session_id);
                res = self
                    .preserving(resume, Some(Duration::from_secs(60)))
                    .await
                    .map_err(|e| format!("session/resume 亦失败（适配器不支持 session/load）：{e}"));
            }
        }
        if let Ok(response) = &res { self.cache_caps(session_id, response); }
        let ok = res.is_ok();
        self.finish_load(session_id, ok);
        guard.2 = true;
        res
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
            let request = protocol::ListSessionsRequest::new().cursor(cursor.clone());
            let res = self.preserving(request, Some(Duration::from_secs(30))).await?;
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

    pub fn has_active_turn(&self, session_id: &str) -> bool {
        self.buffers.lock().unwrap().contains_key(session_id)
    }

    /// The manager holds the (connection, session) lock. Refuse overwrite even if a
    /// future caller forgets that lock; a rejected turn must never erase its owner.
    pub fn begin_turn(
        &self,
        session_id: &str,
        context_id: &str,
        turn_id: &str,
        source: &str,
    ) -> Result<(), String> {
        let mut buffers = self.buffers.lock().unwrap();
        if buffers.contains_key(session_id) {
            return Err("该会话已有进行中的回合".into());
        }
        if !self.is_alive() {
            return Err("适配器连接已断开".into());
        }
        let mut requests = self.requests.lock().unwrap();
        requests.cancelled_sessions.remove(session_id);
        self.file_tasks.begin_session(session_id);
        self.loading.lock().unwrap().remove(session_id);
        buffers.insert(
            session_id.to_string(),
            TurnBuffer {
                context_id: Some(context_id.to_string()),
                turn_id: turn_id.to_string(),
                source: source.to_string(),
                ..Default::default()
            },
        );
        Ok(())
    }

    pub fn end_turn(&self, session_id: &str, turn_id: &str) -> (String, String, String) {
        let buf = {
            let mut buffers = self.buffers.lock().unwrap();
            if !buffers
                .get(session_id)
                .is_some_and(|b| b.turn_id == turn_id)
            {
                return (String::new(), String::new(), "[]".into());
            }
            let mut requests = self.requests.lock().unwrap();
            let buf = buffers.remove(session_id).unwrap();
            self.drain_user_requests(&mut requests, Some(session_id));
            // Keep a cancelled SID's gate closed through the ended state.
            // Only an accepted begin_turn may open a new epoch.
            buf
        };
        (
            buf.text.trim().to_string(),
            buf.thought.trim().to_string(),
            serde_json::to_string(&buf.tools).unwrap_or_else(|_| "[]".into()),
        )
    }

    /// Drain currently queued decisions. Session cancellation additionally
    /// closes the admission gate in cancel_matching_turn before this step.
    pub fn cancel_user_requests(&self, session_id: Option<&str>) {
        self.drain_user_requests(&mut self.requests.lock().unwrap(), session_id);
    }

    fn drain_user_requests(&self, requests: &mut UserRequests, session_id: Option<&str>) {
        let mut cancelled = Vec::new();
        for (map, outcome) in [
            (&mut requests.permissions, json!({ "outcome": "cancelled" })),
            (&mut requests.elicitations, json!({ "action": "cancel" })),
        ] {
            let ids: Vec<_> = map
                .iter()
                .filter(|(_, r)| session_id.is_none_or(|sid| r.session_id == sid))
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                if let Some(r) = map.remove(&id) {
                    let _ = r.tx.send(outcome.clone());
                }
                cancelled.push(id);
            }
        }
        if !cancelled.is_empty() {
            let _ = self.app.emit(EVT_REQUESTS_CANCELLED, json!({
                "agentType": self.agent_type, "connectionId": self.connection_id, "requestIds": cancelled
            }));
        }
    }

    /// Deliver the user's permission decision to the pending server request.
    pub fn resolve_permission(&self, request_id: &str, option_id: &str) -> Result<(), String> {
        if let Some(tx) = self.requests.lock().unwrap().permissions.remove(request_id) {
            tx.tx
                .send(json!({ "outcome": "selected", "optionId": option_id }))
                .map_err(|_| "权限请求已失效".to_string())
        } else {
            Err("权限请求已失效".into())
        }
    }

    /// Deliver the user's elicitation answer to the pending server request.
    pub fn resolve_elicitation(&self, request_id: &str, response: Value) -> Result<(), String> {
        if let Some(tx) = self
            .requests
            .lock()
            .unwrap()
            .elicitations
            .remove(request_id)
        {
            tx.tx
                .send(response)
                .map_err(|_| "输入请求已失效".to_string())
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
/// 标准 content 可能是唯一的工具结果：保留并限长，而不是删除；MessageItem 可展示
/// 文本、diff 摘要和超限时的字符串回退。rawInput/rawOutput 同样有独立预算。
/// 输出始终是合法 JSON 对象——这一点是 v0.3.9 用 cap_str 掐断工具行字符串时丢失的。
pub fn compact_tool_update(update: &Value) -> Value {
    let Some(obj) = update.as_object() else { return update.clone() };
    let mut out = serde_json::Map::with_capacity(obj.len());
    for (k, v) in obj {
        match k.as_str() {
            "content" => {
                out.insert(k.clone(), cap_json_value(v, TOOL_OUTPUT_MAX_BYTES));
            }
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

fn format_acp_error(err: SdkError) -> String {
    let details = err.data.as_ref().filter(|d| !d.is_null()).map(|d| format!("（{d}）")).unwrap_or_default();
    format!("{} (code {}){details}", err.message, i32::from(err.code))
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
    fn compact_tool_update_preserves_content_and_caps_raw_fields() {
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
        assert!(c["content"].as_str().unwrap().len() < TOOL_OUTPUT_MAX_BYTES + 64, "content-only results are retained with a bound");
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
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file!());
        let path = path.to_str().unwrap();
        let full = read_text_file_range(path, None, None).unwrap();
        let ranged = read_text_file_range(path, Some(2), Some(u64::MAX)).unwrap();
        assert_eq!(ranged, full.split_inclusive('\n').skip(1).collect::<String>());
    }
}

