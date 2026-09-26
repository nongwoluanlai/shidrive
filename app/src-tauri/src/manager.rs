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

fn configured_env_path(env: &std::collections::BTreeMap<String, String>, key: &str) -> Option<std::path::PathBuf> {
    env.get(key).cloned().or_else(|| std::env::var(key).ok())
        .filter(|v| !v.trim().is_empty()).map(std::path::PathBuf::from)
}

fn retry_new_without_mcp(agent_type: &str, with_mcp: &Value) -> bool {
    // DeepSeek declares HTTP MCP support. A -32603 persistence failure is NOT
    // an MCP rejection: retrying it on the same bridge hides the first error
    // behind "the ACP bridge has been disposed" and may create duplicate sessions.
    agent_type != "deepseek" && with_mcp.as_array().is_some_and(|a| !a.is_empty())
}

fn deepseek_new_error(error: &str) -> String {
    if error.contains("ACP session persistence flush failed") {
        format!("DeepSeek 会话持久化失败。请在「Agent 管理」修复 DeepSeek 适配器（需要可选原生依赖），并检查 DSH_HOME/用户数据目录的写权限与磁盘空间。原始错误：{error}")
    } else if error.contains("the ACP bridge has been disposed") {
        format!("DeepSeek ACP 桥已关闭；请在「Agent 管理」修复适配器或检查数据目录权限。原始错误：{error}")
    } else {
        format!("DeepSeek 创建会话失败：{error}")
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn env_only_override_resolves_without_autodiscovery() {
        let env = std::collections::BTreeMap::from([
            ("ZCODE_BIN".into(), "custom/zcode.cjs".into()),
            ("ZCODE_NODE".into(), "custom/node.exe".into()),
        ]);
        assert_eq!(configured_env_path(&env, "ZCODE_BIN"), Some("custom/zcode.cjs".into()));
        assert_eq!(configured_env_path(&env, "ZCODE_NODE"), Some("custom/node.exe".into()));
    }

    #[test]
    fn deepseek_first_session_error_is_not_overwritten_by_mcp_retry() {
        let with_mcp = json!([{ "type": "http", "name": "shidrive", "url": "http://127.0.0.1/mcp" }]);
        assert!(!retry_new_without_mcp("deepseek", &with_mcp));
        assert!(retry_new_without_mcp("codex", &with_mcp));
        let original = "Internal error (code -32603)（{\"details\":\"ACP session persistence flush failed\"}）";
        let shown = deepseek_new_error(original);
        assert!(shown.contains("ACP session persistence flush failed"));
        assert!(shown.contains("修复 DeepSeek 适配器"));
        let disposed = deepseek_new_error("Internal error: the ACP bridge has been disposed (code -32603)");
        assert!(disposed.contains("bridge has been disposed"));
    }
}

/// Cancelling the future must cancel the session it actually prompted (including temp sessions).
struct ActiveTurn<'a> {
    manager: &'a AgentManager,
    conn: Arc<AcpConnection>,
    session_id: String,
    context_id: String,
    agent_type: String,
    completed: bool,
}

impl Drop for ActiveTurn<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.conn.notify("session/cancel", json!({ "sessionId": self.session_id }));
            let _ = self.manager.db.set_binding_status(&self.context_id, &self.agent_type, "interrupted");
            let _ = self.manager.app.emit("acp://binding-status", json!({
                "contextId": self.context_id, "agentType": self.agent_type, "status": "interrupted"
            }));
        }
        let _ = self.conn.end_turn(&self.session_id);
    }
}

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
    /// Serialize connect, disconnect and config replacement for each adapter.
    connection_locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    bind_locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
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
        Self { app, db, tools, conns: RwLock::new(HashMap::new()), overrides: RwLock::new(overrides), connection_locks: std::sync::Mutex::new(HashMap::new()), bind_locks: std::sync::Mutex::new(HashMap::new()), prompt_locks: std::sync::Mutex::new(HashMap::new()) }
    }

    fn prompt_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.prompt_locks.lock().unwrap();
        locks.entry(key.to_string()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
    }

    pub fn mcp_url(&self) -> String {
        let port = crate::mcp::port_from_settings(&self.db);
        format!("http://127.0.0.1:{port}/mcp")
    }

    fn connection_lock(&self, agent_type: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.connection_locks.lock().unwrap().entry(agent_type.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
    }

    /// 绑定互斥（与 connection_lock 分离：bind_session 内部还会经
    /// ensure_connected/disconnect 获取 connection_lock，同一把锁会自死锁）
    fn bind_lock(&self, agent_type: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.bind_locks.lock().unwrap().entry(agent_type.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
    }

    /// Manual settings only: auto-discovery belongs in the read-only status UI.
    pub async fn override_for(&self, agent_type: &str) -> AgentLaunch {
        self.overrides.read().await.get(agent_type).cloned().unwrap_or_else(|| AgentLaunch {
            command: String::new(), args: Vec::new(), env: Default::default(),
        })
    }

    pub async fn set_override(&self, agent_type: &str, launch: Option<AgentLaunch>) {
        let lock = self.connection_lock(agent_type);
        let _guard = lock.lock().await;
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
        drop(overrides);
        self.disconnect_locked(agent_type).await;
    }

    /// Probe the same CLI, Node and environment that the adapter will inherit.
    async fn zcode_preflight(&self, launch: &AgentLaunch) -> Result<(), String> {
        let zc = launch.env.get("ZCODE_BIN").ok_or("未配置 ZCODE_BIN")?;
        let node = launch.env.get("ZCODE_NODE").unwrap_or(&launch.command);
        let is_script = std::path::Path::new(zc).extension().and_then(|s| s.to_str())
            .map(|ext| matches!(ext, "cjs" | "mjs" | "js")).unwrap_or(false);
        let mut cmd = tokio::process::Command::new(if is_script { node } else { zc });
        if is_script { cmd.arg(zc); }
        // Probe server mode, never the interactive TUI entry point.
        cmd.args(["app-server", "--stdio"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::acp::apply_launch_env(&mut cmd, &launch.env);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("zcode 后端预检：无法用 {node} 启动 {zc}: {e}"))?;
        let stderr = child.stderr.take().ok_or("zcode 预检缺少 stderr")?;
        // Keep stderr draining, but retain only a bounded diagnostic tail. No detached reader.
        let diagnostic = async move {
            use tokio::io::AsyncReadExt;
            let mut stderr = stderr;
            let mut tail = Vec::new();
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut chunk).await {
                if n == 0 { break; }
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > 16 * 1024 { tail.drain(..tail.len() - 16 * 1024); }
            }
            String::from_utf8_lossy(&tail).to_string()
        };
        tokio::pin!(diagnostic);
        let mut output = None;
        let status = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                tokio::select! {
                    status = child.wait() => break status,
                    text = &mut diagnostic, if output.is_none() => output = Some(text),
                }
            }
        }).await;
        match status {
            Err(_) => {
                child.kill().await.map_err(|e| format!("zcode 预检进程清理失败: {e}"))?;
                Ok(())
            }
            Ok(Ok(st)) if st.success() => {
                // 干净退出（stdin EOF 后 app-server 主动收尾）也算健康：
                // 预检只关心 CLI 能否带着这份环境启动。
                Ok(())
            }
            Ok(Ok(st)) => {
                let out = match output {
                    Some(text) => text,
                    None => tokio::time::timeout(Duration::from_millis(250), &mut diagnostic).await.unwrap_or_default(),
                };
                let tail = out.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
                Err(format!("zcode 后端启动失败（{st}）：{}", if tail.is_empty() { "无错误输出" } else { &tail }))
            }
            Ok(Err(e)) => Err(format!("zcode 后端预检失败: {e}")),
        }
    }

    /// Verify the freshly installed DeepSeek binary, including lazy native dependencies.
    /// A successful initialize alone is insufficient: missing Koffi/flock fails at session/new.
    /// DSH_HOME is disposable so this never deletes or modifies the user's ~/.dsh sessions.
    pub async fn probe_deepseek_adapter(&self, script: &std::path::Path) -> Result<(), String> {
        struct ProbeHome(std::path::PathBuf);
        impl Drop for ProbeHome {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }

        let node = crate::node_rt::require_deepseek_node(&self.tools)?;
        let root = crate::agents::managed_deepseek_dir().ok_or("无法确定 DeepSeek 安装目录")?;
        let home = ProbeHome(root.join(format!(".probe-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&home.0).map_err(|e| format!("创建 DeepSeek 校验目录失败: {e}"))?;
        let launch = AgentLaunch {
            command: node.to_string_lossy().to_string(),
            args: vec![
                "--disable-warning=ExperimentalWarning".into(),
                script.to_string_lossy().to_string(),
                "--profile".into(), "acp".into(),
            ],
            env: std::collections::BTreeMap::from([("DSH_HOME".into(), home.0.to_string_lossy().to_string())]),
        };
        // 探针的状态事件使用独立 id，避免 initialize 成功时误把真实
        // DeepSeek 连接显示成“已连接”（它还没有通过 session/new）。
        let conn = AcpConnection::spawn(self.app.clone(), "deepseek-probe", &launch).await
            .map_err(|e| format!("DeepSeek 安装校验：ACP initialize 失败: {e}"))?;
        let result = async {
            let cwd = home.0.to_string_lossy().to_string();
            let res = conn.request("session/new", json!({ "cwd": cwd, "mcpServers": [] }), Some(Duration::from_secs(90))).await
                .map_err(|e| format!("DeepSeek 安装校验：session/new 失败: {e}"))?;
            let sid = res.get("sessionId").and_then(Value::as_str)
                .filter(|sid| !sid.is_empty()).ok_or("DeepSeek 安装校验：session/new 未返回 sessionId")?;
            conn.request("session/close", json!({ "sessionId": sid }), Some(Duration::from_secs(30))).await
                .map_err(|e| format!("DeepSeek 安装校验：session/close 失败: {e}"))?;
            Ok::<(), String>(())
        }.await;
        conn.shutdown();
        result
    }

    pub async fn launch_for(&self, agent_type: &str) -> Result<AgentLaunch, String> {
        let manual = self.override_for(agent_type).await;
        if !manual.command.trim().is_empty() {
            return Ok(manual);
        }
        if agent_type == "deepseek" {
            // 检查将真正执行 dsh 的 Node；不能依赖仅查 major 的通用状态。
            crate::node_rt::require_deepseek_node(&self.tools)?;
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
                let zc = configured_env_path(&manual.env, "ZCODE_BIN")
                    .or_else(|| self.tools.zcode_cli())
                    .ok_or("未找到 ZCode 桌面端的 zcode.cjs：请在环境变量里配置 ZCODE_BIN=<zcode.cjs 完整路径>")?;
                let backend_node = configured_env_path(&manual.env, "ZCODE_NODE").unwrap_or_else(|| node.clone());
                env.insert("ZCODE_BIN".into(), zc.to_string_lossy().to_string());
                env.insert("ZCODE_NODE".into(), backend_node.to_string_lossy().to_string());
                if let Some(provider) = manual.env.get("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE").map(std::path::PathBuf::from)
                    .or_else(|| self.tools.zcode_provider_config(&zc))
                    .or_else(|| configured_env_path(&manual.env, "ZCODE_BUILTIN_PROVIDER_CONFIG_FILE")) {
                    env.insert("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE".into(), provider.to_string_lossy().to_string());
                }
                if let Some(personal) = configured_env_path(&manual.env, "ZCODE_PERSONAL_PROVIDER_CONFIG_FILE")
                    .or_else(|| {
                        let home = configured_env_path(&manual.env, "ZCODE_HOME").or_else(|| {
                            configured_env_path(&manual.env, "HOME").or_else(|| configured_env_path(&manual.env, "USERPROFILE"))
                                .map(|home| home.join(".zcode"))
                        })?;
                        let path = home.join("v2").join("provider_config.json");
                        path.is_file().then_some(path)
                    }) {
                    env.insert("ZCODE_PERSONAL_PROVIDER_CONFIG_FILE".into(), personal.to_string_lossy().to_string());
                }
                let adapter = self.tools.zcode_adapter();
                if !adapter.exists() {
                    return Err(format!("未找到 zcode 适配器：请在「设置 → Agent 管理」展开 ZCode 后点「安装适配器」。"));
                }
                args = vec!["--disable-warning=ExperimentalWarning".into(), adapter.to_string_lossy().to_string()];
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
        env.extend(manual.env);
        Ok(AgentLaunch { command: node_str, args, env: env.into_iter().collect() })
    }

    pub async fn status(&self, agent_type: &str) -> String {
        match self.conns.read().await.get(agent_type) {
            Some(c) if c.is_alive() => "connected".into(),
            _ => "disconnected".into(),
        }
    }

    /// 当前已连接的 agent id 快照（退出清理用）。
    pub fn connection_ids(&self) -> Vec<String> {
        tauri::async_runtime::block_on(async { self.conns.read().await.keys().cloned().collect() })
    }

    pub async fn disconnect(&self, agent_type: &str) {
        let lock = self.connection_lock(agent_type);
        let _guard = lock.lock().await;
        self.disconnect_locked(agent_type).await;
    }

    async fn disconnect_locked(&self, agent_type: &str) {
        if let Some(c) = self.conns.write().await.remove(agent_type) {
            c.shutdown();
        }
        let _ = self.app.emit("acp://status", json!({ "agentType": agent_type, "state": "disconnected", "info": Value::Null }));
    }

    pub async fn ensure_connected(&self, agent_type: &str) -> Result<Arc<AcpConnection>, String> {
        let lock = self.connection_lock(agent_type);
        let _guard = lock.lock().await;
        {
            let conns = self.conns.read().await;
            if let Some(c) = conns.get(agent_type) {
                if c.is_alive() {
                    return Ok(c.clone());
                }
            }
        }
        if let Some(stale) = self.conns.write().await.remove(agent_type) {
            stale.shutdown();
        }
        let launch = self.launch_for(agent_type).await?;
        if agent_type == "zcode" && self.override_for(agent_type).await.command.trim().is_empty() {
            self.zcode_preflight(&launch).await?;
        }
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
    /// adapter supports HTTP MCP (or hasn't declared a capability). Other agents
    /// retain their old retry without MCP; DeepSeek preserves the first error.
    fn mcp_servers_param(&self, conn: &AcpConnection) -> Value {
        if conn.mcp_degraded() {
            return json!([]);
        }
        let caps = &conn.agent_info.lock().unwrap();
        let declared_http = caps
            .get("agentCapabilities")
            .and_then(|c| c.get("mcpCapabilities"))
            .and_then(|m| m.get("http"))
            .and_then(|v| v.as_bool())
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
                if conn.agent_type == "deepseek" {
                    // 首次错误通常含 persistence flush failed；不能在同一桥上重试
                    // 造成第二个 disposed 错误覆盖它。连接可能已经被 dsh 关闭。
                    log::warn!("[deepseek] first session/new failed: {e}");
                    if e.contains("the ACP bridge has been disposed")
                        || e.contains("ACP session persistence flush failed")
                    {
                        self.disconnect("deepseek").await;
                    }
                    return Err(deepseek_new_error(&e));
                }
                if retry_new_without_mcp(&conn.agent_type, &with_mcp) {
                    // 其他适配器保留原有的无 MCP 回退行为。
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
        self.ensure_session_inner(context, agent_type, true).await
    }

    /// `emit_ready=false`：装载/恢复会话但不广播 session-ready。
    /// 供 set_config_option/set_mode 使用——它们在每轮配置回放里都会调用本函数，
    /// 若此时广播 session-ready，前端「会话配置记忆」effect 会重套配置并再次
    /// 触发本函数，形成 IPC 风暴直至 Win32 消息队列耗尽（0x80070718）。
    pub async fn ensure_session_inner(&self, context: &Context, agent_type: &str, emit_ready: bool) -> Result<(Arc<AcpConnection>, String), String> {
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
                        if emit_ready {
                            let _ = self.app.emit(
                                "acp://session-ready",
                                json!({ "agentType": agent_type, "contextId": context.id, "sessionId": sid, "title": self.db.get_binding(&context.id, agent_type).ok().flatten().and_then(|b| b.title), "response": caps }),
                            );
                        }
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

        let sid = self.create_and_bind_session(&conn, context, agent_type, emit_ready).await?;
        Ok((conn, sid))
    }

    /// For an explicit DeepSeek New Session: keep the old binding until session/new
    /// succeeds. A disposed bridge must not erase the user's current session.
    pub async fn create_fresh_deepseek_session(&self, context: &Context) -> Result<String, String> {
        let conn = self.ensure_connected("deepseek").await?;
        self.create_and_bind_session(&conn, context, "deepseek", true).await
    }

    async fn create_and_bind_session(
        &self, conn: &AcpConnection, context: &Context, agent_type: &str, emit_ready: bool,
    ) -> Result<String, String> {
        let (sid, res) = self.session_new_impl(conn, context).await?;
        if sid.is_empty() {
            return Err("适配器未返回 sessionId".into());
        }
        let cwd = self.project_root_for(context);
        conn.mark_loaded(&sid);
        let title = res.get("title").and_then(Value::as_str).map(|s| s.to_string())
            .or_else(|| (agent_type == "deepseek").then(|| "新会话".to_string()));
        conn.cache_caps(&res);
        self.db.set_binding_session(&context.id, agent_type, Some(&sid), title.as_deref(), Some(&cwd))?;
        if emit_ready {
            let _ = self.app.emit(
                "acp://session-ready",
                json!({ "agentType": agent_type, "contextId": context.id, "sessionId": sid, "title": title, "response": res }),
            );
        }
        Ok(sid)
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
        // 串行化同 agent 的绑定（独立锁：connection_lock 在内部的
        // ensure_connected/disconnect 里还会获取，同锁重入会自死锁）
        let lock = self.bind_lock(agent_type);
        let _guard = lock.lock().await;
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
        // one turn at a time per session (workflow temp sessions never block the UI bound session)
        let lock_sid = match &target {
            SessionTarget::Explicit(sid) => format!("{agent_type}:{sid}"),
            _ => format!("{agent_type}:ctx-{}", context.id),
        };
        let turn_lock = self.prompt_lock(&lock_sid);
        let _turn = turn_lock.lock().await;
        let mut conn = self.ensure_connected(agent_type).await?;
        let cwd = self.project_root_for(context);
        let sid: String = match target {
            SessionTarget::Binding => {
                let (conn2, sid) = self.ensure_session(context, agent_type).await?;
                conn = conn2;
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
        let mut turn = ActiveTurn {
            manager: self, conn: conn.clone(), session_id: sid.clone(), context_id: context.id.clone(),
            agent_type: agent_type.to_string(), completed: false,
        };
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
        turn.completed = true;
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
            // 连接重建过的话先装载会话，避免 cancel 打到适配器不认识的 id 上
            if !conn.is_loaded(&sid) {
                if let Some(context) = self.db.get_context_row(context_id) {
                    let _ = self.ensure_session(&context, agent_type).await;
                }
            }
            let live = self
                .db
                .get_binding(context_id, agent_type)
                .ok()
                .flatten()
                .and_then(|b| b.session_id)
                .unwrap_or(sid);
            conn.notify("session/cancel", json!({ "sessionId": live }));
        }
        Ok(())
    }

    pub async fn set_mode(&self, context_id: &str, agent_type: &str, mode_id: &str) -> Result<(), String> {
        let Some(context) = self.db.get_context_row(context_id) else {
            return Ok(());
        };
        let (_conn, sid) = self.ensure_session_inner(&context, agent_type, false).await?;
        self.ensure_connected(agent_type)
            .await?
            .request("session/set_mode", json!({ "sessionId": sid, "modeId": mode_id }), Some(Duration::from_secs(15)))
            .await?;
        Ok(())
    }

    pub async fn set_config_option(&self, context_id: &str, agent_type: &str, option_id: &str, value: Value) -> Result<(), String> {
        let Some(context) = self.db.get_context_row(context_id) else {
            return Ok(());
        };
        // 走 ensure_session 而非裸绑定 id：适配器重启后内存中无旧会话，
        // 直接发请求会得到 "Session ... not found"（-32603）；装载/恢复失败时
        // ensure_session 会解绑并新建，再对返回的活跃 id 应用配置。
        // quiet（不发 session-ready）：否则前端「配置记忆」effect 重套配置再触发
        // 本函数，形成 IPC 风暴直至 Win32 消息队列耗尽
        let (conn, sid) = self.ensure_session_inner(&context, agent_type, false).await?;
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

    pub async fn respond_elicitation(&self, request_id: &str, response: Value) -> Result<(), String> {
        let agent_type = request_id.split(':').next().unwrap_or_default().to_string();
        let conns = self.conns.read().await;
        if let Some(conn) = conns.get(&agent_type) {
            conn.resolve_elicitation(request_id, response)
        } else {
            Err("适配器未连接，无法应答输入请求".into())
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
            vscode_path: self.tools.vscode_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            tools_dir: self.tools.tools_dir.to_string_lossy().to_string(),
            data_dir: self.app.path().app_data_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            mcp_port: crate::mcp::port_from_settings(&self.db),
        }
    }
}

/// Convert captured session/load replay updates into (role, content) chat rows.
/// 单条内容上限（字节）。超限保留头 48KB + 尾 16KB 并插入截断标记：
/// 巨型会话（含大量工具输出/图鉴类内容）整份转录灌进前端会造成
/// 数倍的内存放大（rows → items → 深代理 → persist stringify → IPC 副本），
/// 渲染只需摘要即可；完整数据仍在适配器会话里。
fn cap_str(s: String) -> String {
    const LIMIT: usize = 64 * 1024;
    if s.len() <= LIMIT {
        return s;
    }
    let head = &s[..s.floor_char_boundary(48 * 1024).min(s.len())];
    let tail_from = s.floor_char_boundary(s.len().saturating_sub(16 * 1024));
    let tail = &s[tail_from..];
    format!(
        "{head}\n\n…[已截断 {}KB：查看完整内容请参考原始会话]\n\n{tail}",
        (s.len() - head.len() - tail.len()) / 1024
    )
}

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
                $out.push(("user".to_string(), cap_str(std::mem::take(&mut $user))));
            }
            if !$thought.is_empty() {
                $out.push(("thought".to_string(), cap_str(std::mem::take(&mut $thought))));
            }
            if !$tools.is_empty() {
                $out.push(("tools".to_string(), cap_str(serde_json::to_string(&$tools).unwrap_or_else(|_| "[]".into()))));
                $tools.clear();
            }
            if !$assistant.is_empty() {
                $out.push(("assistant".to_string(), cap_str(std::mem::take(&mut $assistant))));
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
