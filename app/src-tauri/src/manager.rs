//! AgentManager: owns one ACP connection per agent type, resolves launch configs,
//! and exposes high-level session operations used by commands and the workflow engine.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use agent_client_protocol::schema::v1::{McpServer, McpServerHttp, ContentBlock, TextContent, ImageContent};
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

fn retry_new_without_mcp(agent_type: &str, with_mcp: &[McpServer]) -> bool {
    // DeepSeek declares HTTP MCP support. A -32603 persistence failure is NOT
    // an MCP rejection: retrying it on the same bridge hides the first error
    // behind "the ACP bridge has been disposed" and may create duplicate sessions.
    agent_type != "deepseek" && !with_mcp.is_empty()
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
        let with_mcp = vec![McpServer::Http(McpServerHttp::new("shidrive", "http://127.0.0.1/mcp"))];
        assert!(!retry_new_without_mcp("deepseek", &with_mcp));
        assert!(retry_new_without_mcp("codex", &with_mcp));
        let original = "Internal error (code -32603)（{\"details\":\"ACP session persistence flush failed\"}）";
        let shown = deepseek_new_error(original);
        assert!(shown.contains("ACP session persistence flush failed"));
        assert!(shown.contains("修复 DeepSeek 适配器"));
        let disposed = deepseek_new_error("Internal error: the ACP bridge has been disposed (code -32603)");
        assert!(disposed.contains("bridge has been disposed"));
    }

    /// v0.3.9 把 cap_str 也套在 tools 行上：JSON 数组字符串被从中间掐断后前端
    /// JSON.parse 失败 → 整个工具块从聊天里消失。工具行必须始终是合法 JSON，
    /// 体量靠逐条瘦身（acp::compact_tool_update）而不是掐断字符串来控制。
    #[test]
    fn replay_tool_rows_stay_valid_json_and_bounded() {
        let big = "y".repeat(crate::acp::TOOL_OUTPUT_MAX_BYTES * 4);
        let mut updates = vec![json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "跑一下测试" } })];
        for i in 0..6 {
            updates.push(json!({ "sessionUpdate": "tool_call", "toolCallId": format!("c{i}"), "title": "cargo test", "kind": "execute", "status": "pending",
                                 "rawInput": { "command": ["cargo", "test"] } }));
            updates.push(json!({ "sessionUpdate": "tool_call_update", "toolCallId": format!("c{i}"), "status": "completed",
                                 "content": [{ "type": "content", "content": { "type": "text", "text": big } }], "rawOutput": big }));
        }
        updates.push(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "全部通过" } }));
        let rows = replay_to_messages(&updates);
        assert_eq!(rows.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>(), vec!["user", "tools", "assistant"]);
        let (_, tools_json) = &rows[1];
        let tools: Vec<Value> = serde_json::from_str(tools_json).expect("tools row must be valid JSON");
        assert_eq!(tools.len(), 6, "tool_call + tool_call_update merged by toolCallId, nothing dropped");
        assert!(tools_json.len() < 6 * (2 * crate::acp::TOOL_OUTPUT_MAX_BYTES + 512), "tools row bounded: {}", tools_json.len());
        for t in &tools {
            assert_eq!(t["title"], "cargo test");
            assert_eq!(t["status"], "completed");
            assert_eq!(t["rawInput"]["command"][0], "cargo");
            assert!(t.get("content").is_some(), "bounded standard tool content is retained");
            assert!(t["rawOutput"].as_str().unwrap().len() <= crate::acp::TOOL_OUTPUT_MAX_BYTES + 64);
        }
    }

    /// 文本行仍按头尾截断，且不会切在多字节字符中间（不依赖 1.91 才稳定的 floor_char_boundary）。
    #[test]
    fn text_rows_are_capped_on_char_boundaries() {
        let s: String = std::iter::repeat("多字节字符串。").take(20_000).collect();
        assert!(s.len() > 64 * 1024);
        let capped = cap_str(s.clone());
        assert!(capped.len() < 66 * 1024);
        assert!(capped.contains("已截断"));
        assert!(capped.starts_with("多字节") && capped.ends_with("字符串。"));
        assert_eq!(cap_str("short".into()), "short");
    }
}

/// Cancelling the future must cancel the session it actually prompted (including temp sessions).
struct ActiveTurn {
    app: AppHandle,
    db: Arc<Db>,
    conn: Arc<AcpConnection>,
    session_id: String,
    context_id: String,
    agent_type: String,
    turn_id: String,
    source: &'static str,
    completed: bool,
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        if !self.completed {
            self.conn.cancel_turn(&self.session_id, &self.turn_id);
            self.status("interrupted");
        }
        let _ = self.conn.end_turn(&self.session_id, &self.turn_id);
    }
}

impl ActiveTurn {
    fn status(&self, status: &str) {
        // Workflow temp/explicit sessions do not own the context's UI binding.
        if self.source == "chat" {
            let _ = self.db.set_binding_status(&self.context_id, &self.agent_type, status);
        }
        let event = if self.source == "chat" { "acp://binding-status" } else { "acp://workflow-status" };
        let _ = self.app.emit(event, json!({
            "contextId": self.context_id, "agentType": self.agent_type, "sessionId": self.session_id,
            "connectionId": self.conn.connection_id, "turnId": self.turn_id, "source": self.source, "status": status
        }));
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
    /// Every entry point uses the same (connection generation, actual SID) lock.
    prompt_locks: std::sync::Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
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

    fn prompt_lock(&self, conn: &AcpConnection, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let key = format!("{}:{}:{session_id}", conn.connection_id, conn.agent_type);
        let mut locks = self.prompt_locks.lock().unwrap();
        if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) { return lock; }
        locks.retain(|_, weak| weak.strong_count() > 0);
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }

    pub fn mcp_url(&self) -> String {
        let port = crate::mcp::port_from_settings(&self.db);
        format!("http://127.0.0.1:{port}/mcp")
    }

    fn connection_lock(&self, agent_type: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.connection_locks.lock().unwrap().entry(agent_type.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
    }

    /// 按 (context, agent) 串行化绑定变更（与 connection_lock 分离：内部还会经
    /// ensure_connected/disconnect 获取 connection_lock，同一把锁会自死锁）
    fn bind_lock(&self, context_id: &str, agent_type: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.bind_locks.lock().unwrap().entry(format!("{context_id}:{agent_type}"))
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
            let res = conn.new_session(&cwd, vec![]).await
                .map_err(|e| format!("DeepSeek 安装校验：session/new 失败: {e}"))?;
            let sid = res.get("sessionId").and_then(Value::as_str)
                .filter(|sid| !sid.is_empty()).ok_or("DeepSeek 安装校验：session/new 未返回 sessionId")?;
            conn.close_session(sid).await
                .map_err(|e| format!("DeepSeek 安装校验：session/close 失败: {e}"))?;
            Ok::<(), String>(())
        }.await;
        conn.shutdown();
        result
    }

    pub async fn launch_for(&self, agent_type: &str) -> Result<AgentLaunch, String> {
        let mut manual = self.override_for(agent_type).await;
        if !manual.command.trim().is_empty() {
            // Empty args + an executable override means "use the registered launch
            // mode". Non-empty args are a complete, explicit command-line override.
            if manual.args.is_empty() {
                if let Some(sp) = crate::agents::spec(agent_type) { manual.args = sp.args.clone(); }
            }
            return Ok(manual);
        }
        if agent_type == "deepseek" {
            // 接入方式与设置页共用 agents::deepseek_source（acp / dsh / harness 互斥，不相互回退）。
            // 不改写 DSH_HOME（那是 dsh 的数据目录，指向安装目录会把会话写进 Program Files）。
            let mode = crate::agents::deepseek_mode_setting(&self.db);
            let src = crate::agents::deepseek_source(&manual.env, mode.as_deref());
            match src.kind.as_str() {
                "acp" => {
                    // 三方桥自带 dsh 运行时，用 ShiDrive 的 Node 直接运行其 bin.js，无需参数；
                    // 不使用全局 dsh plugin profile，不影响用户的 dsh 安装。
                    let node = crate::node_rt::require_deepseek_node(&self.tools)?;
                    let mut env = manual.env;
                    env.remove("DSH_CMD");
                    return Ok(AgentLaunch {
                        command: node.to_string_lossy().to_string(),
                        args: vec!["--disable-warning=ExperimentalWarning".into(), src.path],
                        env,
                    });
                }
                "manual" | "desktop" => {
                    let args = crate::agents::spec("deepseek").map(|sp| sp.args.clone()).unwrap_or_default();
                    let mut env = manual.env;
                    // 默认模型：用户未设时注入最新模型名（deepseek-flash 为 V4.1 快速版，
                    // deepseek-v4-pro 为旗舰版；deepseek-chat/reasoner 已于 2026-07 弃用）
                    env.entry("DEEPSEEK_MODEL".to_string())
                        .or_insert_with(|| "deepseek-flash".to_string());
                    return Ok(AgentLaunch { command: src.path, args, env });
                }
                "invalid" => {
                    return Err(format!(
                        "DSH_CMD 指向的路径无效（{}）。请在「设置 → Agent 管理 → DeepSeek」重新选择 dsh.cmd 路径，或清除后改用自动检测。",
                        src.path
                    ));
                }
                "npm" => {
                    // 检查将真正执行 dsh 的 Node；不能依赖仅查 major 的通用状态。
                    crate::node_rt::require_deepseek_node(&self.tools)?;
                }
                _ => {
                    return Err(match src.mode.as_str() {
                        "acp" => "DeepSeek 三方 ACP 未安装：请在「设置 → Agent 管理 → DeepSeek」选择「三方 ACP」并点「安装」。",
                        "harness" => "DeepSeek 官方完整包未安装：请在「设置 → Agent 管理 → DeepSeek」选择「官方完整包」并点「安装」。",
                        _ => "未找到 dsh.cmd：请在「设置 → Agent 管理 → DeepSeek」点「检测」或「选择路径…」。",
                    }.into());
                }
            }
        }
        let sp = crate::agents::spec(agent_type)
            .ok_or_else(|| format!("未知的 Agent 类型：{agent_type}（可在「设置 → Agent 管理」启用更多工具）"))?;
        let node = self.tools.node_exe();
        let node_str = node.to_string_lossy().to_string();
        let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        let mut args: Vec<String>;
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
            "pi" => {
                let script = crate::agents::adapter_script(&self.tools, crate::agents::PI_ACP_PKG)
                    .ok_or("未安装 pi-acp 适配器：请在「设置 → Agent 管理」展开 Pi 后点「安装适配器」。")?;
                args = vec!["--disable-warning=ExperimentalWarning".into(), script.to_string_lossy().to_string()];
                if !manual.env.contains_key("PI_ACP_PI_COMMAND") {
                    if let Some(cli) = crate::agents::pi_cli_command(&self.tools) {
                        // 托管 pi 的 npm shim 通过 PATH 找 node：前置实际使用的 Node，
                        // 确保不装系统 Node 也能跑，且与版本检查的是同一份 Node。
                        crate::node_rt::require_node_22_19(&self.tools, "pi")?;
                        env.insert("PI_ACP_PI_COMMAND".into(), cli.to_string_lossy().to_string());
                        let dirs = [node.parent(), cli.parent()].into_iter().flatten().map(|p| p.to_path_buf());
                        if let Ok(path) = std::env::join_paths(dirs) {
                            env.insert("PATH".into(), path.to_string_lossy().to_string());
                        }
                    } else if crate::agents::pi_global_command().is_none() {
                        return Err("未找到 pi CLI：请在「设置 → Agent 管理」展开 Pi 点「安装适配器」（会同时安装 pi），或自行 npm i -g @earendil-works/pi-coding-agent，或在环境变量里配置 PI_ACP_PI_COMMAND。".into());
                    }
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
        args.extend(manual.args); // automatic executable + additional user arguments
        env.extend(manual.env);
        Ok(AgentLaunch { command: node_str, args, env: env.into_iter().collect() })
    }

    /// 打开新控制台运行交互式 pi，供 /login 或配置模型提供商（等价于 ACP
    /// Terminal Auth 的 `pi-acp --terminal-login`，但直接用 node 运行 CLI 脚本，
    /// 避免 shell 模式下带空格的用户目录路径被拆开）。
    pub async fn open_pi_login(&self, cwd: Option<String>) -> Result<(), String> {
        let launch = self.launch_for("pi").await?;
        let mut cmd = match self.tools_pi_login_target(&launch) {
            Some((program, args)) => {
                let mut c = std::process::Command::new(program);
                c.args(args);
                c
            }
            None => return Err("未找到 pi CLI：请先安装适配器或配置 PI_ACP_PI_COMMAND。".into()),
        };
        for (key, value) in &launch.env {
            if key.eq_ignore_ascii_case("PATH") {
                let mut paths: Vec<_> = std::env::split_paths(value).collect();
                if let Some(current) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&current));
                }
                if let Ok(p) = std::env::join_paths(paths) { cmd.env("PATH", p); }
            } else {
                cmd.env(key, value);
            }
        }
        if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd.current_dir(dir);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE
        }
        cmd.spawn().map(|_| ()).map_err(|e| format!("启动 pi 终端失败: {e}"))
    }

    /// 登录终端要运行的程序：用户指定的 PI_ACP_PI_COMMAND > 托管 CLI 脚本（node 运行）> PATH 上的 pi。
    fn tools_pi_login_target(&self, launch: &AgentLaunch) -> Option<(std::path::PathBuf, Vec<String>)> {
        let configured = launch.env.iter().find(|(k, _)| k.as_str() == "PI_ACP_PI_COMMAND").map(|(_, v)| v.clone());
        let managed = crate::agents::pi_cli_command(&self.tools).map(|p| p.to_string_lossy().to_string());
        match configured {
            // 自动注入的托管 shim → 直接 node 跑脚本；用户自定义的命令原样执行。
            Some(cmd) if Some(&cmd) != managed.as_ref() => Some((cmd.into(), vec![])),
            _ => match crate::agents::pi_cli_script(&self.tools) {
                Some(script) => Some((self.tools.node_exe(), vec![script.to_string_lossy().to_string()])),
                None => crate::agents::pi_global_command().map(|p| (p, vec![])),
            },
        }
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
    fn mcp_servers_param(&self, conn: &AcpConnection) -> Vec<McpServer> {
        if conn.mcp_degraded() {
            return vec![];
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
            vec![McpServer::Http(McpServerHttp::new("shidrive", self.mcp_url()))]
        } else {
            vec![]
        }
    }

    async fn session_new_impl(&self, conn: &AcpConnection, context: &Context) -> Result<(String, Value), String> {
        let cwd = self.project_root_for(context);
        let with_mcp = self.mcp_servers_param(conn);
        let res = conn
            .new_session(&cwd, with_mcp.clone())
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
                        .new_session(&cwd, vec![])
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

    /// Serialize binding resolution/creation, but release the context lock while
    /// prompting so config changes and independent temporary runs remain usable.
    pub async fn ensure_session_inner(&self, context: &Context, agent_type: &str, emit_ready: bool) -> Result<(Arc<AcpConnection>, String), String> {
        let lock = self.bind_lock(&context.id, agent_type);
        let _guard = lock.lock().await;
        self.ensure_session_locked(context, agent_type, emit_ready).await
    }

    async fn ensure_session_locked(&self, context: &Context, agent_type: &str, emit_ready: bool) -> Result<(Arc<AcpConnection>, String), String> {
        let cwd = self.project_root_for(context);
        let conn = self.ensure_connected(agent_type).await?;
        if let Some(b) = self.db.get_binding(&context.id, agent_type)? {
            if let Some(sid) = b.session_id {
                if !conn.is_loaded(&sid) {
                    let lock = self.prompt_lock(&conn, &sid);
                    let _session = lock.lock().await;
                    if !conn.is_loaded(&sid) {
                        // Preserve the old binding and snapshot on transient load
                        // failures; never silently replace it with an empty session.
                        conn.load_session(&sid, &cwd, self.mcp_servers_param(&conn)).await?;
                    }
                }
                if emit_ready {
                    let _ = self.app.emit("acp://session-ready", json!({
                        "agentType": agent_type, "contextId": context.id, "sessionId": sid,
                        "connectionId": conn.connection_id, "title": b.title, "response": conn.cached_caps(&sid)
                    }));
                }
                return Ok((conn, sid));
            }
        }
        let sid = self.create_and_bind_session(&conn, context, agent_type).await?;
        Ok((conn, sid))
    }

    async fn check_binding_idle(&self, context_id: &str, agent_type: &str) -> Result<(), String> {
        if let Some(sid) = self.db.get_binding(context_id, agent_type)?.and_then(|b| b.session_id) {
            if self.conns.read().await.get(agent_type).is_some_and(|c| c.has_active_turn(&sid)) {
                return Err("会话正在进行中，请先停止后再更换或解绑".into());
            }
        }
        Ok(())
    }

    /// All adapters keep the old binding until creating the replacement succeeds.
    pub async fn create_fresh_session(&self, context: &Context, agent_type: &str) -> Result<String, String> {
        let lock = self.bind_lock(&context.id, agent_type);
        let _guard = lock.lock().await;
        self.check_binding_idle(&context.id, agent_type).await?;
        let conn = self.ensure_connected(agent_type).await?;
        self.create_and_bind_session(&conn, context, agent_type).await
    }

    pub async fn create_fresh_deepseek_session(&self, context: &Context) -> Result<String, String> {
        self.create_fresh_session(context, "deepseek").await
    }

    pub async fn unbind(&self, context_id: &str, agent_type: &str) -> Result<(), String> {
        let lock = self.bind_lock(context_id, agent_type);
        let _guard = lock.lock().await;
        self.check_binding_idle(context_id, agent_type).await?;
        self.db.unbind(context_id, agent_type)
    }

    async fn create_and_bind_session(
        &self, conn: &AcpConnection, context: &Context, agent_type: &str,
    ) -> Result<String, String> {
        let (sid, res) = self.session_new_impl(conn, context).await?;
        if sid.is_empty() {
            return Err("适配器未返回 sessionId".into());
        }
        let cwd = self.project_root_for(context);
        conn.mark_loaded(&sid);
        let title = res.get("title").and_then(Value::as_str).map(|s| s.to_string())
            .or_else(|| Some("新会话".to_string()));
        conn.cache_caps(&sid, &res);
        self.db.set_binding_session(&context.id, agent_type, Some(&sid), title.as_deref(), Some(&cwd))?;
        let _ = self.app.emit(
            "acp://session-ready",
            json!({ "agentType": agent_type, "contextId": context.id, "sessionId": sid, "connectionId": conn.connection_id, "title": title, "response": res }),
        );
        Ok(sid)
    }

    /// session/load with replay capture. Returns the transcript rows on success.
    async fn load_with_capture(
        &self,
        conn: &Arc<AcpConnection>,
        session_id: &str,
        cwd: &str,
    ) -> Result<Vec<(String, String)>, String> {
        struct CaptureGuard<'a>(&'a AcpConnection, &'a str);
        impl Drop for CaptureGuard<'_> { fn drop(&mut self) { self.0.abort_capture(self.1); } }
        conn.begin_capture(session_id);
        let _capture = CaptureGuard(conn, session_id);
        let res = conn.load_session(session_id, cwd, self.mcp_servers_param(conn)).await;
        let updates = conn.end_capture(session_id).await;
        match &res {
            Ok(resp) => {
                conn.cache_caps(session_id, resp);
                Ok(replay_to_messages(&updates))
            }
            Err(e) => Err(e.clone()),
        }
    }

    /// Bind an existing (historical) adapter session to a context and return the
    /// replayed transcript. The UI snapshots rows locally, tagged with this SID.
    /// `silent` marks an automatic background refresh.
    pub async fn bind_session(
        &self,
        context: &Context,
        agent_type: &str,
        session_id: &str,
        title: Option<&str>,
        silent: bool,
    ) -> Result<Vec<ReplayRow>, String> {
        // Same lock order as UI prompts: context binding -> actual session.
        let lock = self.bind_lock(&context.id, agent_type);
        let _guard = lock.lock().await;
        self.check_binding_idle(&context.id, agent_type).await?;
        if silent && self.db.get_binding(&context.id, agent_type)?.and_then(|b| b.session_id).as_deref() != Some(session_id) {
            return Err("会话绑定已变化，忽略过期的历史刷新".into());
        }
        let cwd = self.project_root_for(context);
        let conn = self.ensure_connected(agent_type).await?;
        let session_lock = self.prompt_lock(&conn, session_id);
        let _session = session_lock.lock().await;
        // Empty replay is a valid result. Never kill other sessions to manufacture
        // history, and never capture notifications from an active prompt.
        let loaded = self.load_with_capture(&conn, session_id, &cwd).await?;
        let rows: Vec<ReplayRow> = loaded
            .into_iter()
            .map(|(role, content)| ReplayRow { role, content })
            .collect();
        log::info!("[{agent_type}] bind_session replay rows={}", rows.len());
        // 绑定关系本身仍然落库（会话元数据，不是聊天记录）
        self.db.set_binding_session(&context.id, agent_type, Some(session_id), title, Some(&cwd))?;
        // 绑定了带历史的会话视为一次已完成的对话状态
        let _ = self.db.set_binding_status(&context.id, agent_type, "completed");
        let caps = conn.cached_caps(session_id);
        let mut response = caps.as_object().cloned().unwrap_or_default();
        response.insert("bound".to_string(), Value::Bool(true));
        let _ = self.app.emit(
            "acp://session-ready",
            json!({ "agentType": agent_type, "contextId": context.id, "sessionId": session_id, "connectionId": conn.connection_id, "response": response }),
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
        self.prompt_with_request_id(context, agent_type, target, text, images, None, "workflow").await
    }

    async fn prompt_with_request_id(
        &self, context: &Context, agent_type: &str, target: SessionTarget,
        text: &str, images: &[crate::models::PromptImage], request_id: Option<&str>, source: &'static str,
    ) -> Result<Value, String> {
        let binding_lock = self.bind_lock(&context.id, agent_type);
        let binding_guard = if matches!(&target, SessionTarget::Binding) { Some(binding_lock.lock().await) } else { None };
        let mut conn = self.ensure_connected(agent_type).await?;
        let cwd = self.project_root_for(context);
        let sid = match target {
            SessionTarget::Binding => {
                let (c, sid) = self.ensure_session_locked(context, agent_type, true).await?;
                conn = c;
                sid
            }
            SessionTarget::Explicit(sid) => sid,
            SessionTarget::Temp => {
                let (sid, caps) = self.session_new_impl(&conn, context).await?;
                if sid.is_empty() { return Err("适配器未返回 sessionId".into()); }
                conn.mark_loaded(&sid);
                conn.cache_caps(&sid, &caps);
                sid
            }
        };
        let lock = self.prompt_lock(&conn, &sid);
        let session_guard = lock.lock_owned().await;
        if !conn.is_alive() { return Err("适配器连接已断开，请重试".into()); }
        if !conn.is_loaded(&sid) { conn.load_session(&sid, &cwd, self.mcp_servers_param(&conn)).await?; }
        let turn_id = request_id.filter(|id| !id.is_empty()).map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        conn.begin_turn(&sid, &context.id, &turn_id, source)?;
        let mut turn = ActiveTurn {
            app: self.app.clone(), db: self.db.clone(), conn: conn.clone(),
            session_id: sid.clone(), context_id: context.id.clone(),
            agent_type: agent_type.to_string(), turn_id: turn_id.clone(), source, completed: false,
        };
        // Register before releasing the binding guard so rebind/unbind cannot
        // redirect the UI or cancel button while this session is running.
        drop(binding_guard);
        turn.status("running");
        let mut blocks = Vec::new();
        if !text.is_empty() { blocks.push(ContentBlock::Text(TextContent::new(text))); }
        for img in images { blocks.push(ContentBlock::Image(ImageContent::new(img.data.clone(), img.mime.clone()))); }
        if blocks.is_empty() { blocks.push(ContentBlock::Text(TextContent::new(text))); }
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        struct CancelOnDrop(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) { if let Some(tx) = self.0.take() { let _ = tx.send(()); } }
        }
        let mut cancel = CancelOnDrop(Some(cancel_tx));
        // Dropping a workflow future is not an ACP cancellation acknowledgement.
        // Keep the SID lock/buffer alive until the peer replies (or disconnects),
        // otherwise the next prompt could steal late chunks from the canceled one.
        let task = tokio::spawn(async move {
            let _session = session_guard;
            let request = conn.prompt_session(&sid, blocks);
            tokio::pin!(request);
            let mut was_cancelled = false;
            let mut result = tokio::select! {
                biased;
                result = &mut request => result,
                _ = cancel_rx => {
                    was_cancelled = true;
                    conn.cancel_turn(&sid, &turn_id);
                    request.await
                }
            };
            let interrupted = was_cancelled || matches!(&result, Ok(v) if v.get("stopReason").and_then(Value::as_str) == Some("cancelled"));
            turn.status(if result.is_err() || interrupted { "interrupted" } else { "completed" });
            let (text, _, _) = conn.end_turn(&sid, &turn_id);
            if source == "workflow" {
                if let Ok(Value::Object(obj)) = &mut result {
                    obj.insert("_shidriveOutput".into(), Value::String(cap_str(text)));
                }
            }
            turn.completed = true;
            drop(turn);
            result
        });
        let joined = task.await;
        cancel.0.take();
        joined.map_err(|e| format!("会话任务结束异常: {e}"))?
    }

    pub async fn prompt(&self, context: &Context, agent_type: &str, text: &str, images: &[crate::models::PromptImage]) -> Result<Value, String> {
        self.prompt_from_ui(context, agent_type, text, images, None).await
    }

    pub async fn prompt_from_ui(&self, context: &Context, agent_type: &str, text: &str, images: &[crate::models::PromptImage], request_id: Option<&str>) -> Result<Value, String> {
        self.prompt_with_request_id(context, agent_type, SessionTarget::Binding, text, images, request_id, "chat").await
    }

    pub async fn cancel(&self, context_id: &str, agent_type: &str) -> Result<(), String> {
        if let Some(sid) = self.db.get_binding(context_id, agent_type)?.and_then(|b| b.session_id) {
            if let Some(conn) = self.conns.read().await.get(agent_type) {
                if conn.has_active_turn(&sid) {
                    conn.cancel_session(&sid);
                }
            }
        }
        Ok(())
    }

    pub async fn set_mode(&self, context_id: &str, agent_type: &str, mode_id: &str) -> Result<(), String> {
        let Some(context) = self.db.get_context_row(context_id) else {
            return Ok(());
        };
        let lock = self.bind_lock(context_id, agent_type);
        let _guard = lock.lock().await;
        let (conn, sid) = self.ensure_session_locked(&context, agent_type, false).await?;
        conn.set_session_mode(&sid, mode_id)
            .await?;
        // 之后任何 session-ready 都会把 cached_caps 发给前端：同步缓存，避免刷回旧模式
        conn.note_mode(&sid, mode_id);
        Ok(())
    }

    pub async fn set_config_option(&self, context_id: &str, agent_type: &str, option_id: &str, value: Value, expected_session_id: Option<&str>) -> Result<(), String> {
        let Some(context) = self.db.get_context_row(context_id) else {
            return Ok(());
        };
        let lock = self.bind_lock(context_id, agent_type);
        let _guard = lock.lock().await;
        if let Some(expected) = expected_session_id {
            let current = self.db.get_binding(context_id, agent_type)?.and_then(|b| b.session_id);
            if current.as_deref() != Some(expected) { return Err("会话绑定已改变，忽略过期配置请求".into()); }
        }
        // Quiet: emitting session-ready here would trigger the UI preference
        // effect recursively. A failed load leaves the original binding intact.
        let (conn, sid) = self.ensure_session_locked(&context, agent_type, false).await?;
        let res = conn.set_session_config(&sid, option_id, &value).await?;
        // 同步能力缓存（适配器返回新的 configOptions 就整体采纳，否则只改这一项）：
        // 之后的 session-ready 携带 cached_caps，不同步会把前端刚改的值刷回旧值
        conn.note_config_option(&sid, option_id, &value, &res);
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
    // str::floor_char_boundary 在 Rust 1.91 才稳定，README 承诺 1.88+：手写等价逻辑
    fn floor_boundary(s: &str, mut i: usize) -> usize {
        i = i.min(s.len());
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    }
    let head = &s[..floor_boundary(&s, 48 * 1024)];
    let tail_from = floor_boundary(&s, s.len().saturating_sub(16 * 1024));
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
                // 工具行是 JSON 数组：不能用 cap_str 掐断（前端 JSON.parse 失败会把整个
                // 工具块丢掉），体量由 compact_tool_update 逐条控制
                $out.push(("tools".to_string(), serde_json::to_string(&$tools).unwrap_or_else(|_| "[]".into())));
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
                // 只保留前端会渲染的字段并限长（见 acp::compact_tool_update）：
                // 命令输出 / 整文件 diff / MCP 结果原样进 WebView 是绑定长历史时内存暴涨的大头
                let u = crate::acp::compact_tool_update(u);
                let tid = u.get("toolCallId").cloned().unwrap_or(Value::Null);
                if let Some(existing) = tools.iter_mut().find(|x| x.get("toolCallId") == Some(&tid)) {
                    if let (Some(obj), Some(newobj)) = (existing.as_object_mut(), u.as_object()) {
                        for (k, v) in newobj {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                } else {
                    tools.push(u);
                }
            }
            _ => {}
        }
    }
    flush!(out, user, thought, assistant, tools);
    out
}
