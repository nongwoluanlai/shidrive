//! AgentManager: owns one ACP connection per agent type, resolves launch configs,
//! and exposes high-level session operations used by commands and the workflow engine.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::RwLock;

use crate::acp::AcpConnection;
use crate::db::Db;
use crate::models::*;
use crate::setup::Tools;

pub fn enabled_agents(db: &Arc<Db>) -> Vec<String> {
    crate::agents::enabled_agents(db)
}

/// Target session for a prompt: the context binding, an explicit session id, or a fresh temp session.
#[derive(Clone, Debug)]
pub enum SessionTarget {
    Binding,
    Explicit(String),
    Temp,
}

pub struct AgentManager {
    pub app: AppHandle,
    pub db: Arc<Db>,
    pub tools: Tools,
    conns: RwLock<HashMap<String, Arc<AcpConnection>>>,
    overrides: RwLock<HashMap<String, AgentLaunch>>,
    /// serializes prompts per (context_id, agent_type) so a UI turn and a
    /// workflow turn on the same binding never interleave
    prompt_locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl AgentManager {
    pub fn new(app: AppHandle, db: Arc<Db>) -> Self {
        let tools = Tools::discover(&db);
        let mut overrides = HashMap::new();
        for sp in crate::agents::agent_specs() {
            let key = format!("agent.{}", sp.id);
            if let Ok(Some(raw)) = db.get_setting(&key) {
                if let Ok(launch) = serde_json::from_str::<AgentLaunch>(&raw) {
                    overrides.insert(sp.id.clone(), launch);
                }
            }
        }
        Self { app, db, tools, conns: RwLock::new(HashMap::new()), overrides: RwLock::new(overrides), prompt_locks: std::sync::Mutex::new(HashMap::new()) }
    }

    fn prompt_lock(&self, context_id: &str, agent_type: &str) -> Arc<tokio::sync::Mutex<()>> {
        let key = format!("{context_id}:{agent_type}");
        let mut locks = self.prompt_locks.lock().unwrap();
        locks.entry(key).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
    }

    pub fn mcp_url(&self) -> String {
        let port = crate::mcp::port_from_settings(&self.db);
        format!("http://127.0.0.1:{port}/mcp")
    }

    pub async fn set_override(&self, agent_type: &str, launch: Option<AgentLaunch>) {
        let key = format!("agent.{agent_type}");
        let mut overrides = self.overrides.write().await;
        match launch {
            Some(l) => {
                let _ = self.db.set_setting(&key, &serde_json::to_string(&l).unwrap_or_default());
                overrides.insert(agent_type.to_string(), l);
            }
            None => {
                let _ = self.db.set_setting(&key, "");
                overrides.remove(agent_type);
            }
        }
        self.disconnect(agent_type).await;
    }

    /// Effective launch config: user override or auto-discovered default (registry-driven).
    pub async fn launch_for(&self, agent_type: &str) -> Result<AgentLaunch, String> {
        if let Some(l) = self.overrides.read().await.get(agent_type) {
            if !l.command.trim().is_empty() {
                return Ok(l.clone());
            }
        }
        let sp = crate::agents::spec(agent_type)
            .ok_or_else(|| format!("未知的 Agent 类型：{agent_type}（可在「设置 → Agent 管理」启用更多工具）"))?;
        let node = self.tools.node_exe();
        let node_str = node.to_string_lossy().to_string();
        let mut env: HashMap<String, String> = HashMap::new();
        let mut args: Vec<String> = Vec::new();
        match agent_type {
            "codex" => {
                let adapter = self.tools.codex_adapter();
                if !adapter.exists() {
                    return Err(format!("未找到 codex 适配器：请在「设置 → Agent 管理」展开 Codex 后点「安装适配器」。"));
                }
                args = vec!["--disable-warning=ExperimentalWarning".into(), adapter.to_string_lossy().to_string()];
                if let Some(codex) = self.tools.codex_exe() {
                    // 适配器优先读 CODEX_PATH（完整路径）；不设它时适配器在 PATH 上
                    // 找不到裸 "codex" 会回退到内置 CLI，而内置 CLI 因 --omit=optional
                    // 缺平台二进制而报 "Missing optional dependency"。
                    env.insert("CODEX_PATH".to_string(), codex.to_string_lossy().to_string());
                    if let Some(dir) = codex.parent() {
                        env.insert("PATH".to_string(), dir.to_string_lossy().to_string());
                    }
                }
            }
            "zcode" => {
                let adapter = self.tools.zcode_adapter();
                if !adapter.exists() {
                    return Err(format!("未找到 zcode 适配器：请在「设置 → Agent 管理」展开 ZCode 后点「安装适配器」。"));
                }
                args = vec!["--disable-warning=ExperimentalWarning".into(), adapter.to_string_lossy().to_string()];
                if let Some(cli) = self.tools.zcode_cli() {
                    env.insert("ZCODE_BIN".to_string(), cli.to_string_lossy().to_string());
                }
                if self.tools.node_exe().exists() {
                    env.insert("ZCODE_NODE".to_string(), self.tools.node_exe().to_string_lossy().to_string());
                }
            }
            _ => {
                let pkg = sp
                    .npm
                    .as_ref()
                    .ok_or_else(|| format!("{} 使用二进制发行：请在「设置 → Agent 管理」中手动配置命令路径。", sp.name))?;
                let script = crate::agents::adapter_script(&self.tools, pkg)
                    .ok_or_else(|| format!("未安装 {pkg} 适配器：请在「设置 → Agent 管理」展开 {} 后点「安装适配器」。", sp.name))?;
                args = vec!["--disable-warning=ExperimentalWarning".into(), script.to_string_lossy().to_string()];
                args.extend(sp.args.iter().cloned());
            }
        }
        if let Some(l) = self.overrides.read().await.get(agent_type) {
            for (k, v) in &l.env {
                env.insert(k.clone(), v.clone());
            }
        }
        Ok(AgentLaunch { command: node_str, args, env: env.into_iter().collect() })
    }

    pub async fn status(&self, agent_type: &str) -> String {
        match self.conns.read().await.get(agent_type) {
            Some(c) if c.is_alive() => "connected".into(),
            _ => "disconnected".into(),
        }
    }

    pub async fn disconnect(&self, agent_type: &str) {
        if let Some(c) = self.conns.write().await.remove(agent_type) {
            c.shutdown();
        }
        let _ = self.app.emit("acp://status", json!({ "agentType": agent_type, "state": "disconnected", "info": Value::Null }));
    }

    pub async fn ensure_connected(&self, agent_type: &str) -> Result<Arc<AcpConnection>, String> {
        {
            let conns = self.conns.read().await;
            if let Some(c) = conns.get(agent_type) {
                if c.is_alive() {
                    return Ok(c.clone());
                }
            }
        }
        let launch = self.launch_for(agent_type).await?;
        let conn = AcpConnection::spawn(self.app.clone(), agent_type, &launch).await?;
        self.conns.write().await.insert(agent_type.to_string(), conn.clone());
        Ok(conn)
    }

    fn project_root_for(&self, context: &Context) -> String {
        self.db
            .list_projects()
            .ok()
            .and_then(|ps| ps.into_iter().find(|p| p.id == context.project_id))
            .map(|p| p.root_path)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().to_string())
    }

    /// mcpServers for session/new: inject the ShiDrive MCP endpoint when the
    /// adapter supports HTTP MCP (or hasn't declared a capability). On failure
    /// the caller retries without it.
    fn mcp_servers_param(&self, conn: &AcpConnection) -> Value {
        if conn.mcp_degraded() {
            return json!([]);
        }
        let caps = &conn.agent_info.lock().unwrap();
        let declared_http = caps
            .get("agentCapabilities")
            .and_then(|c| c.get("mcpCapabilities"))
            .and_then(|m| m.get("http"))
            .map(|v| !v.is_null())
            .unwrap_or(false);
        let no_mcp_caps = caps.get("agentCapabilities").and_then(|c| c.get("mcpCapabilities")).is_none();
        if declared_http || no_mcp_caps {
            json!([{ "type": "http", "url": self.mcp_url(), "name": "shidrive" }])
        } else {
            json!([])
        }
    }

    async fn session_new_impl(&self, conn: &AcpConnection, context: &Context) -> Result<(String, Value), String> {
        let cwd = self.project_root_for(context);
        let with_mcp = self.mcp_servers_param(conn);
        let res = conn
            .request("session/new", json!({ "cwd": cwd, "mcpServers": with_mcp }), Some(Duration::from_secs(90)))
            .await;
        match res {
            Ok(r) => Ok((r.get("sessionId").and_then(|s| s.as_str()).unwrap_or_default().to_string(), r)),
            Err(e) => {
                if !with_mcp.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    // retry without the injected MCP server (adapter may not support http type)
                    let res = conn
                        .request("session/new", json!({ "cwd": cwd, "mcpServers": [] }), Some(Duration::from_secs(90)))
                        .await?;
                    conn.mark_mcp_degraded();
                    return Ok((
                        res.get("sessionId").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
                        res,
                    ));
                }
                Err(e)
            }
        }
    }

    /// Ensure a live session for (context, agent): resume via session/load when bound.
    pub async fn ensure_session(&self, context: &Context, agent_type: &str) -> Result<(Arc<AcpConnection>, String), String> {
        let cwd = self.project_root_for(context);
        let conn = self.ensure_connected(agent_type).await?;
        let binding = self.db.get_binding(&context.id, agent_type)?;

        if let Some(b) = binding {
            if let Some(sid) = b.session_id.clone() {
                if !conn.is_loaded(&sid) {
                    if let Err(e) = conn.load_session(&sid, &cwd, self.mcp_servers_param(&conn)).await {
                        log::warn!("[{agent_type}] session/load {sid} failed: {e}; unbinding");
                        let _ = self.db.set_binding_session(&context.id, agent_type, None, Some("会话已失效，待重新创建"), None);
                        let _ = self.app.emit("acp://update", json!({ "agentType": agent_type, "sessionExpired": true, "contextId": context.id, "detail": e }));
                    } else {
                        let caps = conn.cached_caps();
                        let _ = self.app.emit(
                            "acp://session-ready",
                            json!({ "agentType": agent_type, "contextId": context.id, "sessionId": sid, "title": self.db.get_binding(&context.id, agent_type).ok().flatten().and_then(|b| b.title), "response": caps }),
                        );
                        return Ok((conn, sid));
                    }
                } else {
                    let caps = conn.cached_caps();
                    let _ = self.app.emit(
                        "acp://session-ready",
                        json!({ "agentType": agent_type, "contextId": context.id, "sessionId": sid, "title": self.db.get_binding(&context.id, agent_type).ok().flatten().and_then(|b| b.title), "response": caps }),
                    );
                    return Ok((conn, sid));
                }
            }
        }

        // create a fresh session
        let (sid, res) = self.session_new_impl(&conn, context).await?;
        if sid.is_empty() {
            return Err("适配器未返回 sessionId".into());
        }
        conn.mark_loaded(&sid);
        let title = res.get("title").and_then(|t| t.as_str()).map(|s| s.to_string());
        conn.cache_caps(&res);
        self.db.set_binding_session(&context.id, agent_type, Some(&sid), title.as_deref(), Some(&cwd))?;
        let _ = self.app.emit(
            "acp://session-ready",
            json!({ "agentType": agent_type, "contextId": context.id, "sessionId": sid, "title": title, "response": res }),
        );
        Ok((conn, sid))
    }

    /// session/load with replay capture. Returns the transcript rows on success.
    async fn load_with_capture(
        &self,
        conn: &Arc<AcpConnection>,
        agent_type: &str,
        session_id: &str,
        cwd: &str,
    ) -> Result<Vec<(String, String)>, String> {
        conn.begin_capture(session_id);
        let res = conn.load_session(session_id, cwd, self.mcp_servers_param(conn)).await;
        let updates = conn.end_capture(session_id).await;
        match &res {
            Ok(resp) => {
                conn.cache_caps(resp);
                Ok(replay_to_messages(&updates))
            }
            Err(e) => Err(e.clone()),
        }
    }

    /// Bind an existing (historical) adapter session to a context and return the
    /// replayed transcript. Chat history is NOT persisted locally — the ACP
    /// session is the source of truth; the UI renders the returned rows.
    /// `silent` marks an automatic background refresh.
    pub async fn bind_session(
        &self,
        context: &Context,
        agent_type: &str,
        session_id: &str,
        title: Option<&str>,
        silent: bool,
    ) -> Result<Vec<ReplayRow>, String> {
        let _ = silent;
        let cwd = self.project_root_for(context);
        let mut conn = self.ensure_connected(agent_type).await?;
        log::info!(
            "[{agent_type}] bind_session ctx={} sid={session_id} loaded={} other_loaded={}",
            context.id,
            conn.is_loaded(session_id),
            conn.has_other_loaded(session_id)
        );
        // 始终尝试重放取转录（多个上下文可能绑定同一个适配器会话，跳过重放
        // 会让新打开的那个上下文拿到空历史）。重放失败时重启适配器走冷启动
        // 路径再试一次——冷启动重放是可靠的。
        let mut loaded = match self.load_with_capture(&conn, agent_type, session_id, &cwd).await {
            Ok(rows) => Some(rows),
            Err(e) => {
                log::warn!("[{agent_type}] session/load {session_id} failed: {e}");
                None
            }
        };
        if loaded.is_none()
            || (loaded.as_ref().unwrap_or(&Vec::new()).is_empty() && conn.has_other_loaded(session_id))
        {
            log::warn!("[{agent_type}] restarting adapter to re-replay session {session_id}");
            self.disconnect(agent_type).await;
            conn = self.ensure_connected(agent_type).await?;
            loaded = Some(self.load_with_capture(&conn, agent_type, session_id, &cwd).await?);
        }
        let rows: Vec<ReplayRow> = loaded
            .unwrap_or_default()
            .into_iter()
            .map(|(role, content)| ReplayRow { role, content })
            .collect();
        log::info!("[{agent_type}] bind_session replay rows={}", rows.len());
        // 绑定关系本身仍然落库（会话元数据，不是聊天记录）
        self.db.set_binding_session(&context.id, agent_type, Some(session_id), title, Some(&cwd))?;
        // 绑定了带历史的会话视为一次已完成的对话状态
        let _ = self.db.set_binding_status(&context.id, agent_type, "completed");
        let caps = conn.cached_caps();
        let mut response = caps.as_object().cloned().unwrap_or_default();
        response.insert("bound".to_string(), Value::Bool(true));
        let _ = self.app.emit(
            "acp://session-ready",
            json!({ "agentType": agent_type, "contextId": context.id, "sessionId": session_id, "response": response }),
        );
        Ok(rows)
    }

    pub async fn list_sessions(&self, agent_type: &str) -> Result<Vec<SessionInfo>, String> {
        let conn = self.ensure_connected(agent_type).await?;
        conn.session_list().await
    }

    /// Send a prompt to the session chosen by `target` and wait for turn completion.
    /// `images` are optional base64 attachments sent as ACP image content blocks
    /// (supported by codex-acp and zcode-acp-server).
    pub async fn prompt_with(
        &self,
        context: &Context,
        agent_type: &str,
        target: SessionTarget,
        text: &str,
        images: &[crate::models::PromptImage],
    ) -> Result<Value, String> {
        // one turn at a time per (context, agent)
        let turn_lock = self.prompt_lock(&context.id, agent_type);
        let _turn = turn_lock.lock().await;
        let conn = self.ensure_connected(agent_type).await?;
        let cwd = self.project_root_for(context);
        let sid: String = match target {
            SessionTarget::Binding => {
                let (conn2, sid) = self.ensure_session(context, agent_type).await?;
                drop(conn2);
                sid
            }
            SessionTarget::Explicit(sid) => {
                if !conn.is_loaded(&sid) {
                    conn.load_session(&sid, &cwd, self.mcp_servers_param(&conn)).await?;
                }
                sid
            }
            SessionTarget::Temp => {
                let (sid, _res) = self.session_new_impl(&conn, context).await?;
                if sid.is_empty() {
                    return Err("适配器未返回 sessionId".into());
                }
                conn.mark_loaded(&sid);
                sid
            }
        };

        conn.begin_turn(&sid, &context.id);
        let _ = self.db.set_binding_status(&context.id, agent_type, "running");
        let _ = self.app.emit("acp://binding-status", json!({ "contextId": context.id, "agentType": agent_type, "status": "running" }));
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(json!({ "type": "text", "text": text }));
        }
        for img in images {
            blocks.push(json!({ "type": "image", "data": img.data, "mimeType": img.mime }));
        }
        if blocks.is_empty() {
            blocks.push(json!({ "type": "text", "text": text }));
        }
        let result = conn
            .request("session/prompt", json!({ "sessionId": sid, "prompt": blocks }), None)
            .await;
        let interrupted = matches!(&result, Ok(v) if v.get("stopReason").and_then(|s| s.as_str()) == Some("cancelled"));
        let final_status = match &result {
            Err(_) => "interrupted",
            Ok(_) if interrupted => "interrupted",
            _ => "completed",
        };
        let _ = self.db.set_binding_status(&context.id, agent_type, final_status);
        let _ = self.app.emit("acp://binding-status", json!({ "contextId": context.id, "agentType": agent_type, "status": final_status }));
        // 回合内容经 acp://update 实时流给前端；历史不再落本地库
        let _ = conn.end_turn(&sid);
        result
    }

    /// Convenience wrapper used by the chat UI.
    pub async fn prompt(
        &self,
        context: &Context,
        agent_type: &str,
        text: &str,
        images: &[crate::models::PromptImage],
    ) -> Result<Value, String> {
        self.prompt_with(context, agent_type, SessionTarget::Binding, text, images).await
    }

    pub async fn cancel(&self, context_id: &str, agent_type: &str) -> Result<(), String> {
        let binding = self.db.get_binding(context_id, agent_type)?;
        if let Some(sid) = binding.and_then(|b| b.session_id) {
            let conn = self.ensure_connected(agent_type).await?;
            conn.notify("session/cancel", json!({ "sessionId": sid }));
        }
        Ok(())
    }

    pub async fn set_mode(&self, context_id: &str, agent_type: &str, mode_id: &str) -> Result<(), String> {
        let binding = self.db.get_binding(context_id, agent_type)?;
        if let Some(sid) = binding.and_then(|b| b.session_id) {
            let conn = self.ensure_connected(agent_type).await?;
            conn.request("session/set_mode", json!({ "sessionId": sid, "modeId": mode_id }), Some(Duration::from_secs(15)))
                .await?;
        }
        Ok(())
    }

    pub async fn set_config_option(&self, context_id: &str, agent_type: &str, option_id: &str, value: Value) -> Result<(), String> {
        let binding = self.db.get_binding(context_id, agent_type)?;
        if let Some(sid) = binding.and_then(|b| b.session_id) {
            let conn = self.ensure_connected(agent_type).await?;
            conn.request(
                "session/set_config_option",
                // codex-acp expects `configId`; zed-style adapters use `configOptionId` — send both
                json!({ "sessionId": sid, "configOptionId": option_id, "configId": option_id, "value": value }),
                Some(Duration::from_secs(15)),
            )
            .await?;
            // 记住模型选择（会话恢复时也能看到）
            if option_id == "model" {
                if let Some(m) = value.as_str() {
                    let _ = self.db.set_binding_model(context_id, agent_type, m);
                }
            }
        }
        Ok(())
    }

    pub async fn set_binding_title(&self, context_id: &str, agent_type: &str, title: &str) -> Result<(), String> {
        self.db.set_binding_title(context_id, agent_type, title)
    }

    pub async fn respond_permission(&self, request_id: &str, option_id: String) -> Result<(), String> {
        let agent_type = request_id.split(':').next().unwrap_or_default().to_string();
        let conns = self.conns.read().await;
        if let Some(conn) = conns.get(&agent_type) {
            conn.resolve_permission(request_id, &option_id)
        } else {
            Err("适配器未连接，无法应答权限请求".into())
        }
    }

    /// Capabilities payload for the UI after a resume. Load responses carry
    /// models/modes but we may skip the load when the session is already live —
    /// in that case reuse whatever the UI already has (frontend merges).
    fn session_caps(&self, conn: &Arc<AcpConnection>, _sid: &str) -> Value {
        let info = conn.agent_info.lock().unwrap().clone();
        json!({ "fromResume": true, "agentCapabilities": info.get("agentCapabilities").cloned().unwrap_or(Value::Null) })
    }

    pub fn setup_status(&self) -> SetupStatus {
        SetupStatus {
            node_path: self.tools.node_exe().to_string_lossy().to_string(),
            codex_path: self.tools.codex_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            zcode_cli_path: self.tools.zcode_cli().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            codex_adapter_path: self.tools.codex_adapter().to_string_lossy().to_string(),
            zcode_adapter_path: self.tools.zcode_adapter().to_string_lossy().to_string(),
            python_path: self.tools.python_exe().to_string_lossy().to_string(),
            tools_dir: self.tools.tools_dir.to_string_lossy().to_string(),
            data_dir: self.app.path().app_data_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            mcp_port: crate::mcp::port_from_settings(&self.db),
        }
    }
}

/// Convert captured session/load replay updates into (role, content) chat rows.
fn replay_to_messages(updates: &[Value]) -> Vec<(String, String)> {
    fn text_of(content: &Value) -> String {
        match content {
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
            b => b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()).unwrap_or_default(),
        }
    }

    let mut out: Vec<(String, String)> = Vec::new();
    let mut user = String::new();
    let mut thought = String::new();
    let mut assistant = String::new();
    let mut tools: Vec<Value> = Vec::new();

    macro_rules! flush {
        ($out:expr, $user:expr, $thought:expr, $assistant:expr, $tools:expr) => {
            if !$user.is_empty() {
                $out.push(("user".to_string(), std::mem::take(&mut $user)));
            }
            if !$thought.is_empty() {
                $out.push(("thought".to_string(), std::mem::take(&mut $thought)));
            }
            if !$tools.is_empty() {
                $out.push(("tools".to_string(), serde_json::to_string(&$tools).unwrap_or_else(|_| "[]".into())));
                $tools.clear();
            }
            if !$assistant.is_empty() {
                $out.push(("assistant".to_string(), std::mem::take(&mut $assistant)));
            }
        };
    }

    for u in updates {
        let t = u.get("sessionUpdate").and_then(|t| t.as_str()).unwrap_or("");
        match t {
            "user_message" | "user_message_chunk" => {
                if !assistant.is_empty() || !tools.is_empty() || !user.is_empty() {
                    // a new user turn starts: flush the previous one
                    if !assistant.is_empty() || !tools.is_empty() {
                        flush!(out, user, thought, assistant, tools);
                    }
                }
                let text = text_of(u.get("content").unwrap_or(&Value::Null));
                if let Some(m) = u.get("content") {
                    let _ = m;
                }
                user.push_str(&text);
                if user.is_empty() {
                    if let Some(txt) = u.get("text").and_then(|x| x.as_str()) {
                        user.push_str(txt);
                    }
                }
            }
            "agent_message_chunk" => {
                assistant.push_str(&text_of(u.get("content").unwrap_or(&Value::Null)));
            }
            "agent_thought_chunk" => {
                thought.push_str(&text_of(u.get("content").unwrap_or(&Value::Null)));
            }
            "tool_call" | "tool_call_update" => {
                let tid = u.get("toolCallId").cloned().unwrap_or(Value::Null);
                if let Some(existing) = tools.iter_mut().find(|x| x.get("toolCallId") == Some(&tid)) {
                    if let (Some(obj), Some(newobj)) = (existing.as_object_mut(), u.as_object()) {
                        for (k, v) in newobj {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                } else {
                    tools.push(u.clone());
                }
            }
            _ => {}
        }
    }
    flush!(out, user, thought, assistant, tools);
    out
}
