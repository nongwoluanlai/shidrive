//! All Tauri commands exposed to the frontend.

use serde_json::Value;
use tauri::State;

use crate::db::Db;
use crate::engine::{self as workflow_time, Engine};
use crate::manager::AgentManager;
use crate::models::*;
use crate::fsops;

type DbState<'a> = State<'a, std::sync::Arc<Db>>;
type AgentsState<'a> = State<'a, std::sync::Arc<AgentManager>>;
type EngineState<'a> = State<'a, std::sync::Arc<Engine>>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---------- projects ----------

#[tauri::command]
pub async fn projects_list(db: DbState<'_>) -> Result<Vec<Project>, String> {
    db.list_projects().map_err(err)
}

#[tauri::command]
pub async fn projects_create(db: DbState<'_>, name: String, root_path: String, description: String) -> Result<Project, String> {
    if name.trim().is_empty() {
        return Err("项目名称不能为空".into());
    }
    if !root_path.trim().is_empty() && !std::path::Path::new(root_path.trim()).exists() {
        return Err(format!("目录不存在：{root_path}"));
    }
    db.create_project(name.trim(), &root_path, &description).map_err(err)
}

#[tauri::command]
pub async fn projects_update(db: DbState<'_>, id: String, name: String, root_path: String, description: String) -> Result<(), String> {
    db.update_project(&id, &name, &root_path, &description).map_err(err)
}

#[tauri::command]
pub async fn projects_delete(db: DbState<'_>, id: String) -> Result<(), String> {
    if id == crate::models::NO_PROJECT_ID {
        return Err("「无项目」为内置项目，不可删除".into());
    }
    db.delete_project(&id).map_err(err)
}

// ---------- contexts ----------

#[tauri::command]
pub async fn contexts_list(db: DbState<'_>, project_id: String) -> Result<Vec<Context>, String> {
    db.list_contexts(&project_id).map_err(err)
}

#[tauri::command]
pub async fn contexts_create(db: DbState<'_>, project_id: String, name: String) -> Result<Context, String> {
    if name.trim().is_empty() {
        return Err("上下文名称不能为空".into());
    }
    db.create_context(&project_id, name.trim()).map_err(err)
}

#[tauri::command]
pub async fn contexts_update(db: DbState<'_>, id: String, name: String, overview: String, constraints: String) -> Result<(), String> {
    db.update_context(&id, &name, &overview, &constraints).map_err(err)
}

#[tauri::command]
pub async fn contexts_delete(db: DbState<'_>, id: String) -> Result<(), String> {
    db.delete_context(&id).map_err(err)
}

// ---------- context entries ----------

#[tauri::command]
pub async fn context_entries_list(db: DbState<'_>, context_id: String) -> Result<Vec<ContextEntry>, String> {
    db.list_entries(&context_id).map_err(err)
}

#[tauri::command]
pub async fn context_entry_add(db: DbState<'_>, context_id: String, kind: String, content: String) -> Result<ContextEntry, String> {
    if !["todo", "progress", "decision", "note"].contains(&kind.as_str()) {
        return Err(format!("未知的条目类型：{kind}"));
    }
    db.add_entry(&context_id, &kind, &content).map_err(err)
}

#[tauri::command]
pub async fn context_entry_update(db: DbState<'_>, id: String, content: String, status: String) -> Result<(), String> {
    db.update_entry(&id, &content, &status).map_err(err)
}

#[tauri::command]
pub async fn context_entry_delete(db: DbState<'_>, id: String) -> Result<(), String> {
    db.delete_entry(&id).map_err(err)
}

/// 共享上下文提交历史（版本列表，含快照与涉及文件）。
#[tauri::command]
pub async fn context_commits_list(db: DbState<'_>, context_id: String, limit: Option<i64>) -> Result<Vec<ScCommit>, String> {
    db.sc_list_commits(&context_id, limit.unwrap_or(50)).map_err(err)
}

// ---------- bindings & chat ----------

#[tauri::command]
pub async fn binding_get(db: DbState<'_>, context_id: String, agent_type: String) -> Result<Option<AgentBinding>, String> {
    db.get_binding(&context_id, &agent_type).map_err(err)
}

#[tauri::command]
pub async fn binding_unbind(db: DbState<'_>, context_id: String, agent_type: String) -> Result<(), String> {
    db.unbind(&context_id, &agent_type).map_err(err)
}

// ---------- ACP ----------

#[tauri::command]
pub async fn acp_status(agents: AgentsState<'_>, agent_type: String) -> Result<String, String> {
    Ok(agents.status(&agent_type).await)
}

#[tauri::command]
pub async fn acp_connect(agents: AgentsState<'_>, agent_type: String) -> Result<Value, String> {
    let conn = agents.ensure_connected(&agent_type).await?;
    let info = conn.agent_info.lock().unwrap().clone();
    Ok(info)
}

#[tauri::command]
pub async fn acp_disconnect(agents: AgentsState<'_>, agent_type: String) -> Result<(), String> {
    agents.disconnect(&agent_type).await;
    Ok(())
}

#[tauri::command]
pub async fn acp_session_new(agents: AgentsState<'_>, context: Context, agent_type: String) -> Result<String, String> {
    // ensure_session reuses the bound session (resuming it when needed);
    // for a brand-new session the frontend unbinds first.
    let (conn, sid) = agents.ensure_session(&context, &agent_type).await?;
    let _ = conn;
    Ok(sid)
}

#[tauri::command]
pub async fn acp_prompt(
    agents: AgentsState<'_>,
    context: Context,
    agent_type: String,
    text: String,
    images: Option<Vec<PromptImage>>,
) -> Result<Value, String> {
    if text.trim().is_empty() && images.as_ref().map_or(true, |i| i.is_empty()) {
        return Err("消息不能为空".into());
    }
    agents.prompt(&context, &agent_type, &text, &images.unwrap_or_default()).await
}

#[tauri::command]
pub async fn acp_cancel(agents: AgentsState<'_>, context_id: String, agent_type: String) -> Result<(), String> {
    agents.cancel(&context_id, &agent_type).await
}

#[tauri::command]
pub async fn acp_set_mode(agents: AgentsState<'_>, context_id: String, agent_type: String, mode_id: String) -> Result<(), String> {
    agents.set_mode(&context_id, &agent_type, &mode_id).await
}

#[tauri::command]
pub async fn acp_set_config_option(
    agents: AgentsState<'_>,
    context_id: String,
    agent_type: String,
    option_id: String,
    value: Value,
) -> Result<(), String> {
    agents.set_config_option(&context_id, &agent_type, &option_id, value).await
}

#[tauri::command]
pub async fn acp_respond_permission(agents: AgentsState<'_>, request_id: String, option_id: String) -> Result<(), String> {
    agents.respond_permission(&request_id, option_id).await
}

/// Historical sessions known to the adapter (for the 绑定历史会话 dialog).
#[tauri::command]
pub async fn acp_sessions_list(agents: AgentsState<'_>, agent_type: String) -> Result<Vec<SessionInfo>, String> {
    agents.list_sessions(&agent_type).await
}

/// Bind an existing adapter session to a context and return the replayed transcript.
/// `silent` marks an automatic background refresh (no history-loaded toast flow).
#[tauri::command]
pub async fn acp_session_bind(
    agents: AgentsState<'_>,
    context: Context,
    agent_type: String,
    session_id: String,
    title: Option<String>,
    silent: Option<bool>,
) -> Result<Vec<ReplayRow>, String> {
    agents
        .bind_session(&context, &agent_type, &session_id, title.as_deref(), silent.unwrap_or(false))
        .await
}

/// All agent bindings across projects (for the 会话管理 panel).
#[tauri::command]
pub async fn bindings_all(db: DbState<'_>) -> Result<Vec<BindingInfo>, String> {
    db.bindings_list_all().map_err(err)
}

#[tauri::command]
pub async fn binding_set_title(agents: AgentsState<'_>, context_id: String, agent_type: String, title: String) -> Result<(), String> {
    agents.set_binding_title(&context_id, &agent_type, &title).await
}

#[tauri::command]
pub async fn binding_set_model(db: DbState<'_>, context_id: String, agent_type: String, model: String) -> Result<(), String> {
    db.set_binding_model(&context_id, &agent_type, &model).map_err(err)
}

#[tauri::command]
pub async fn workflow_move(db: DbState<'_>, id: String, dir: i64) -> Result<(), String> {
    db.workflow_move(&id, dir).map_err(err)
}

/// All contexts of all projects with project names (for pickers).
#[tauri::command]
pub async fn contexts_all(db: DbState<'_>) -> Result<Vec<(Context, String)>, String> {
    db.all_contexts().map_err(err)
}

/// Currently running workflow runs (for the 运行中任务 panel).
#[tauri::command]
pub async fn active_runs(engine: EngineState<'_>) -> Result<Vec<(String, String, String, String)>, String> {
    Ok(engine.active_runs())
}

// ---------- files ----------

#[tauri::command]
pub async fn fs_list(path: String) -> Result<Vec<fsops::DirEntryInfo>, String> {
    fsops::list_dir(&path)
}

#[tauri::command]
pub async fn fs_read(path: String) -> Result<(String, bool), String> {
    fsops::read_file(&path)
}

#[tauri::command]
pub async fn fs_write(path: String, content: String) -> Result<(), String> {
    fsops::write_file(&path, &content)
}

#[tauri::command]
pub async fn fs_create_file(path: String) -> Result<(), String> {
    fsops::create_file(&path)
}

#[tauri::command]
pub async fn fs_create_dir(path: String) -> Result<(), String> {
    fsops::create_dir(&path)
}

#[tauri::command]
pub async fn fs_rename(from: String, to: String) -> Result<(), String> {
    fsops::rename_path(&from, &to)
}

#[tauri::command]
pub async fn fs_delete(path: String) -> Result<(), String> {
    fsops::delete_path(&path)
}

#[tauri::command]
pub async fn fs_open_explorer(path: String) -> Result<(), String> {
    fsops::open_in_explorer(&path)
}

#[tauri::command]
pub async fn fs_open_terminal(path: String) -> Result<(), String> {
    fsops::open_in_terminal(&path)
}

#[tauri::command]
pub async fn fs_open_cmd(path: String) -> Result<(), String> {
    fsops::open_in_cmd(&path)
}

#[tauri::command]
pub async fn fs_open_default(path: String) -> Result<(), String> {
    fsops::open_default(&path)
}

#[tauri::command]
/// 读取会话的本地聊天记录 JSON（无记录返回空串）。
pub async fn chat_store_get(db: DbState<'_>, key: String) -> Result<String, String> {
    Ok(db.inner().chat_store_get(&key)?.unwrap_or_default())
}

/// 覆盖写入会话的本地聊天记录 JSON。
#[tauri::command]
pub async fn chat_store_set(db: DbState<'_>, key: String, itemsJson: String) -> Result<(), String> {
    db.inner().chat_store_set(&key, &itemsJson)
}

/// 删除会话的本地聊天记录。
#[tauri::command]
pub async fn chat_store_delete(db: DbState<'_>, key: String) -> Result<(), String> {
    db.inner().chat_store_delete(&key)
}

#[tauri::command]
pub async fn fs_desktop_dir() -> Result<String, String> {
    fsops::desktop_dir()
}

#[tauri::command]
pub async fn fs_copy_to_clipboard(paths: Vec<String>) -> Result<(), String> {
    fsops::copy_to_clipboard(&paths)
}

#[tauri::command]
pub async fn fs_paste_from_clipboard(dest_dir: String) -> Result<(), String> {
    fsops::paste_from_clipboard(&dest_dir)
}

// ---------- workflows ----------

#[tauri::command]
pub async fn workflows_list(db: DbState<'_>, project_id: String) -> Result<Vec<Workflow>, String> {
    db.list_workflows(&project_id).map_err(err)
}

#[tauri::command]
pub async fn workflow_create(
    db: DbState<'_>,
    project_id: String,
    name: String,
    description: String,
    enabled: bool,
    trigger_type: String,
    schedule: Option<ScheduleConfig>,
    steps: Vec<WorkflowStep>,
    env: Option<std::collections::BTreeMap<String, String>>,
    edges: Option<Vec<Edge>>,
) -> Result<Workflow, String> {
    if name.trim().is_empty() {
        return Err("工作流名称不能为空".into());
    }
    if trigger_type == "schedule" && schedule.is_none() {
        return Err("请配置定时规则".into());
    }
    db.create_workflow(
        &project_id,
        name.trim(),
        &description,
        enabled,
        &trigger_type,
        schedule.as_ref(),
        &steps,
        &env.unwrap_or_default(),
        &edges.unwrap_or_default(),
    )
    .map_err(err)
}

#[tauri::command]
pub async fn workflow_update(engine: EngineState<'_>, workflow: Workflow) -> Result<(), String> {
    let next = if workflow.enabled && workflow.trigger_type == "schedule" {
        workflow
            .schedule
            .as_ref()
            .map(|s| workflow_time::fmt_time(workflow_time::compute_next(s, chrono::Local::now())))
    } else {
        None
    };
    let mut w = workflow;
    w.next_run_at = next;
    engine.db.update_workflow(&w).map_err(err)
}

#[tauri::command]
pub async fn workflow_delete(db: DbState<'_>, id: String) -> Result<(), String> {
    db.delete_workflow(&id).map_err(err)
}

#[tauri::command]
pub async fn workflow_run(engine: EngineState<'_>, id: String) -> Result<(), String> {
    engine.run_now(&id)
}

#[tauri::command]
pub async fn workflow_stop(engine: EngineState<'_>, run_id: String) -> Result<(), String> {
    engine.stop_run(&run_id);
    Ok(())
}

#[tauri::command]
pub async fn runs_list(db: DbState<'_>, workflow_id: String, limit: Option<i64>) -> Result<Vec<WorkflowRun>, String> {
    db.list_runs(&workflow_id, limit.unwrap_or(30)).map_err(err)
}

// ---------- settings ----------

#[tauri::command]
pub async fn settings_get(db: DbState<'_>, key: String) -> Result<Option<String>, String> {
    db.get_setting(&key).map_err(err)
}

#[tauri::command]
pub async fn settings_set(db: DbState<'_>, key: String, value: String) -> Result<(), String> {
    db.set_setting(&key, &value).map_err(err)
}

/// Agent 注册表与环境状态（设置页）。
#[tauri::command]
pub async fn agents_registry(agents: AgentsState<'_>) -> Result<Vec<crate::agents::AgentEnvStatus>, String> {
    Ok(crate::agents::registry_status(&agents.db, &agents.tools))
}

/// 启用顺序（会话页 tab 顺序）。
#[tauri::command]
pub async fn agents_enabled_get(agents: AgentsState<'_>) -> Result<Vec<String>, String> {
    Ok(crate::agents::enabled_agents(&agents.db))
}

#[tauri::command]
pub async fn agents_enabled_set(agents: AgentsState<'_>, ids: Vec<String>) -> Result<(), String> {
    for id in &ids {
        // 立即断开被停用的 agent
    }
    let before = crate::agents::enabled_agents(&agents.db);
    for id in &before {
        if !ids.contains(id) {
            agents.disconnect(id).await;
        }
    }
    crate::agents::set_enabled_agents(&agents.db, &ids)
}

/// 安装 npm 适配器（托管目录 / .tools/acp）。
#[tauri::command]
pub async fn agents_bootstrap(agents: AgentsState<'_>, db: DbState<'_>, id: String) -> Result<String, String> {
    let sp = crate::agents::spec(&id).ok_or_else(|| format!("未知的 Agent：{id}"))?;
    let pkg = sp.npm.as_ref().ok_or_else(|| format!("{} 无需安装适配器（二进制发行，请手动配置命令）。", sp.name))?;
    // 代理仅作用于这次 npm 安装子进程（不影响工作流 / ACP / MCP）
    let proxy = db
        .get_setting("network.proxy")
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    crate::agents::bootstrap_adapter(&agents.tools, pkg, proxy.as_deref())
}

#[tauri::command]
pub async fn setup_status(agents: AgentsState<'_>) -> Result<SetupStatus, String> {
    Ok(agents.setup_status())
}

/// Import a bounded skin package into the skin store.
#[tauri::command]
pub async fn skin_import(path: String) -> Result<serde_json::Value, String> {
    let root = crate::skins::root()?;
    tauri::async_runtime::spawn_blocking(move || crate::skins::import(&root, std::path::Path::new(&path)))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn skins_list() -> Result<Vec<serde_json::Value>, String> {
    let root = crate::skins::root()?;
    tauri::async_runtime::spawn_blocking(move || crate::skins::list(&root))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn skin_asset_data(skin_dir: String, file: String) -> Result<String, String> {
    let root = crate::skins::root()?;
    tauri::async_runtime::spawn_blocking(move || crate::skins::asset(&root, &skin_dir, &file))
        .await.map_err(|e| e.to_string())?
}

// ---------- 外部编程接入（MCP） ----------

type RemoteState<'a> = tauri::State<'a, crate::remote_mcp::RemoteManager>;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStartOpts {
    #[serde(default = "default_listen_host", alias = "listen_host")]
    listen_host: String,
    #[serde(default = "default_port")]
    port: i64,
    #[serde(default, alias = "quick_tunnel")]
    quick_tunnel: bool,
}
fn default_listen_host() -> String { "0.0.0.0".into() }
fn default_port() -> i64 { 51688 }

#[tauri::command]
pub async fn remote_mcp_start(
    app: tauri::AppHandle,
    remote: RemoteState<'_>,
    db: DbState<'_>,
    engine: EngineState<'_>,
    opts: Option<RemoteStartOpts>,
) -> Result<(), String> {
    let o = opts.unwrap_or(RemoteStartOpts {
        listen_host: default_listen_host(),
        port: default_port(),
        quick_tunnel: false,
    });
    let port = u16::try_from(o.port.clamp(1, 65535)).map_err(|_| "端口超出范围")?;
    remote.start(app, db.inner().clone(), engine.inner().clone(), &o.listen_host, port, o.quick_tunnel)
}

/// 运行中单独启动 Quick Tunnel（切换接入方式为 Cloudflare 时调用；幂等）。
#[tauri::command]
pub async fn remote_tunnel_start(
    app: tauri::AppHandle,
    remote: RemoteState<'_>,
    db: DbState<'_>,
) -> Result<(), String> {
    remote.tunnel_start(app, db.inner().clone())
}

/// 运行中单独停止 Quick Tunnel 并清空地址（切回自定义地址时调用）。
#[tauri::command]
pub async fn remote_tunnel_stop(remote: RemoteState<'_>) -> Result<(), String> {
    remote.tunnel_stop();
    Ok(())
}

#[tauri::command]
pub async fn remote_mcp_stop(remote: RemoteState<'_>) -> Result<(), String> {
    remote.stop();
    Ok(())
}

#[tauri::command]
pub async fn remote_mcp_status(remote: RemoteState<'_>, db: DbState<'_>) -> Result<crate::remote_mcp::RemoteStatus, String> {
    Ok(remote.status(db.inner()))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGrantInputPayload {
    pub project_id: String,
    pub project_name: String,
    pub project_root: String,
    pub context_id: Option<String>,
    pub context_name: Option<String>,
    #[serde(default)]
    pub context_enabled: bool,
    #[serde(default)]
    pub fs_write: bool,
    #[serde(default)]
    pub exec_allowed: bool,
}

#[tauri::command]
pub async fn remote_grants_list(db: DbState<'_>) -> Result<serde_json::Value, String> {
    let grants = crate::remote_mcp::grants_list(db.inner())?;
    serde_json::to_value(grants).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remote_grant_create(db: DbState<'_>, input: RemoteGrantInputPayload) -> Result<serde_json::Value, String> {
    let input = crate::remote_mcp::RemoteGrantInput {
        project_id: input.project_id,
        project_name: input.project_name,
        project_root: input.project_root,
        context_id: input.context_id,
        context_name: input.context_name.unwrap_or_default(),
        context_enabled: input.context_enabled,
        fs_write: input.fs_write,
        exec_allowed: input.exec_allowed,
    };
    let (grant, token) = crate::remote_mcp::grant_create(db.inner(), &input)?;
    serde_json::to_value(serde_json::json!({ "grant": grant, "token": token }))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remote_grant_revoke(db: DbState<'_>, id: String) -> Result<(), String> {
    crate::remote_mcp::grant_revoke(db.inner(), &id)
}

#[tauri::command]
pub async fn remote_grant_delete(db: DbState<'_>, id: String) -> Result<(), String> {
    crate::remote_mcp::grant_delete(db.inner(), &id)
}

/// 重新签发授权凭据（旧凭据立即失效）。保留给「手动重置单个授权凭据」场景；
/// 常规轮换已改为：服务启动时全部刷新 / 暂停→继续开放时刷新。
#[tauri::command]
pub async fn remote_grant_rotate_token(db: DbState<'_>, id: String) -> Result<serde_json::Value, String> {
    let token = crate::remote_mcp::grant_rotate_token(db.inner(), &id)?;
    Ok(serde_json::json!({ "token": token }))
}

/// 取授权当前有效凭据（不轮换）。供「复制提示词」随时取用。
#[tauri::command]
pub async fn remote_grant_token(db: DbState<'_>, id: String) -> Result<serde_json::Value, String> {
    let token = crate::remote_mcp::grant_token_get(db.inner(), &id)?;
    Ok(serde_json::json!({ "token": token }))
}

/// 暂停开放：暂停期间该授权的连接一律 403。
#[tauri::command]
pub async fn remote_grant_pause(db: DbState<'_>, id: String) -> Result<(), String> {
    crate::remote_mcp::grant_pause(db.inner(), &id)
}

/// 继续开放：清除暂停标记并刷新凭据。
#[tauri::command]
pub async fn remote_grant_resume(db: DbState<'_>, id: String) -> Result<(), String> {
    crate::remote_mcp::grant_resume(db.inner(), &id)
}

/// 检测 cloudflared 环境：解析路径 + 读取版本（供设置页展示）。
#[tauri::command]
pub async fn remote_cloudflared_status(db: DbState<'_>) -> Result<crate::remote_mcp::CloudflaredStatus, String> {
    Ok(crate::remote_mcp::cloudflared_status(db.inner()))
}

/// 保存手动指定的 cloudflared 路径（空串=清除）。
#[tauri::command]
pub async fn remote_cloudflared_set_path(db: DbState<'_>, path: String) -> Result<(), String> {
    crate::remote_mcp::cloudflared_set_path(db.inner(), &path)
}

/// 下载 cloudflared（可选出站代理，仅作用于本次下载）。隧道运行中拒绝（Windows 下
/// 正在运行的 exe 无法被覆盖，先装会失败到一半）。
#[tauri::command]
pub async fn remote_cloudflared_install(remote: RemoteState<'_>, db: DbState<'_>) -> Result<String, String> {
    {
        let st = remote.status(db.inner());
        if st.tunnel_running {
            return Err("隧道正在运行：请先停止服务再更新 cloudflared".into());
        }
    }
    let proxy = db
        .get_setting("network.proxy")
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    tauri::async_runtime::spawn_blocking(move || {
        let appdata = std::env::var("APPDATA").map_err(|_| "无法确定用户数据目录".to_string())?;
        let dir = std::path::PathBuf::from(&appdata)
            .join("com.shidrive.desktop")
            .join("tools")
            .join("cloudflared");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {e}"))?;
        let dest = dir.join("cloudflared.exe");
        let url = "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe";
        let mut builder = ureq::AgentBuilder::new();
        if let Some(p) = proxy.as_deref().filter(|p| !p.is_empty()) {
            match ureq::Proxy::new(p) {
                Ok(px) => builder = builder.proxy(px),
                Err(e) => return Err(format!("代理配置无效: {e}")),
            }
        }
        let agent = builder.build();
        let resp = agent
            .get(url)
            .call()
            .map_err(|e| format!("下载失败（可配置代理后重试）: {e}"))?;
        let mut reader = resp.into_reader();
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            let n = reader.read(&mut chunk).map_err(|e| format!("下载中断: {e}"))?;
            if n == 0 { break; }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > 256 * 1024 * 1024 { return Err("下载超过 256MB 上限".into()); }
        }
        let tmp = dest.with_extension("download");
        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| format!("写入失败: {e}"))?;
            std::io::Write::write_all(&mut f, &buf).map_err(|e| format!("写入失败: {e}"))?;
        }
        std::fs::rename(&tmp, &dest).map_err(|e| format!("安装失败: {e}"))?;
        Ok(format!("cloudflared 已安装：{}", dest.display()))
    })
    .await
    .map_err(|e| format!("任务失败: {e}"))?
}

/// 写文本到系统剪贴板。
#[tauri::command]
pub async fn clipboard_write_text(text: String) -> Result<(), String> {
    crate::fsops::copy_text_to_clipboard(&text)
}

/// 读系统剪贴板文本。
#[tauri::command]
pub async fn clipboard_read_text() -> Result<String, String> {
    crate::fsops::read_text_from_clipboard()
}

/// 导出项目/上下文/条目/工作流到桌面 JSON 备份，返回文件路径。
#[tauri::command]
pub async fn data_export(db: DbState<'_>) -> Result<String, String> {
    let data = db.export_backup()?;
    let desktop = crate::fsops::desktop_dir()?;
    let path = std::path::PathBuf::from(desktop)
        .join(format!("shidrive-backup-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    let json = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写入失败: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// 从备份 JSON 导入（INSERT OR REPLACE，按 id 覆盖），返回写入摘要。
#[tauri::command]
pub async fn data_import(db: DbState<'_>, path: String) -> Result<String, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("读取失败: {e}"))?;
    let data: serde_json::Value = serde_json::from_str(&raw).map_err(|e| format!("解析失败: {e}"))?;
    if data.get("app").and_then(|v| v.as_str()) != Some("shidrive") {
        return Err("不是使驾的备份文件（缺少 app 标识）".into());
    }
    let counts = db.import_backup(&data)?;
    let summary = counts
        .iter()
        .map(|(t, n)| format!("{t}×{n}"))
        .collect::<Vec<_>>()
        .join("，");
    Ok(format!("导入完成：{summary}"))
}

/// 前端气泡提示落日志文件（error/warn 记录详情，便于远程排查）。
#[tauri::command]
pub async fn ui_log(kind: String, text: String) -> Result<(), String> {
    match kind.as_str() {
        "error" => log::error!("[ui] {text}"),
        "warn" => log::warn!("[ui] {text}"),
        _ => log::info!("[ui] {text}"),
    }
    Ok(())
}

/// 在 VS Code 中打开目录（路径可在设置 → 环境与路径指定，留空自动检测）。
#[tauri::command]
pub async fn fs_open_vscode(agents: AgentsState<'_>, path: String) -> Result<(), String> {
    let code = agents
        .tools
        .vscode_exe()
        .ok_or_else(|| "未找到 VS Code：请在「设置 → 环境与路径」填写 code 路径".to_string())?;
    fsops::open_in_vscode(&code.to_string_lossy(), &path)
}

/// 卸载 npm 适配器（托管目录与 .tools/acp 中所有安装位置）。
#[tauri::command]
pub async fn agents_uninstall(agents: AgentsState<'_>, db: DbState<'_>, id: String) -> Result<String, String> {
    let sp = crate::agents::spec(&id).ok_or_else(|| format!("未知的 Agent：{id}"))?;
    let pkg = sp.npm.as_ref().ok_or_else(|| format!("{} 为二进制发行，无需卸载适配器。", sp.name))?;
    let proxy = db
        .get_setting("network.proxy")
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let tools = agents.tools.clone();
    let pkg2 = pkg.clone();
    tauri::async_runtime::spawn_blocking(move || crate::agents::uninstall_adapter(&tools, &pkg2, proxy.as_deref()))
        .await
        .map_err(|e| format!("卸载任务失败: {e}"))?
}

/// Node 运行时探测结果（版本 / 来源 / 是否满足 ≥22）。
#[tauri::command]
pub async fn node_status(agents: AgentsState<'_>) -> Result<crate::node_rt::NodeStatus, String> {
    let tools = agents.tools.clone();
    tauri::async_runtime::spawn_blocking(move || Ok(crate::node_rt::node_status(&tools)))
        .await
        .map_err(|e| format!("探测失败: {e}"))?
}

/// 下载 node22 便携版到用户数据目录（仅 Windows；代理只作用于本次下载）。
#[tauri::command]
pub async fn node_download(db: DbState<'_>) -> Result<String, String> {
    let proxy = db
        .get_setting("network.proxy")
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    tauri::async_runtime::spawn_blocking(move || crate::node_rt::download_node22(proxy.as_deref()))
        .await
        .map_err(|e| format!("下载任务失败: {e}"))?
}

#[tauri::command]
pub async fn agent_config_get(agents: AgentsState<'_>, agent_type: String) -> Result<AgentLaunch, String> {
    Ok(agents.override_for(&agent_type).await)
}

#[tauri::command]
pub async fn agent_config_set(agents: AgentsState<'_>, agent_type: String, launch: Option<AgentLaunch>) -> Result<(), String> {
    if crate::agents::spec(&agent_type).is_none() {
        return Err(format!("未知的 Agent 类型：{agent_type}"));
    }
    agents.set_override(&agent_type, launch).await;
    Ok(())
}
