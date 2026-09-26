//! ShiDrive MCP server: streamable-HTTP JSON-RPC endpoint + skills document.
//!
//! GET  /skills[?sharedsession=<contextId>]  → markdown skill document for agents
//! POST /mcp                                 → MCP (JSON-RPC 2.0): tools/list, tools/call
//!
//! Shared context model is git-like: every agent update is a commit
//! (seq version + summary + files + full snapshot). Stale writes are rejected
//! with guidance so the agent can re-read and merge, like a conflict.

use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::db::Db;
use crate::models::{Edge, ScheduleConfig, ScEntry, ScSnapshot, Workflow, WorkflowStep};

const MAX_OVERVIEW: usize = 2000;
const MAX_CONSTRAINTS: usize = 2000;
const MAX_ENTRY: usize = 500;
const MAX_ENTRIES: usize = 60;
const MAX_SUMMARY: usize = 600;
const MAX_FILES: usize = 30;

pub struct McpState {
    pub db: Arc<Db>,
    /// engine for workflow tools (run/list live runs)
    pub engine: std::sync::OnceLock<Arc<crate::engine::Engine>>,
    /// serialize context_update flows (check → write-through → snapshot → commit)
    pub commit_lock: std::sync::Mutex<()>,
}

pub fn port_from_settings(db: &Db) -> i64 {
    db.get_setting("mcp.port")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(8345)
        .clamp(1024, 65535)
}

/// Spawn the MCP HTTP server. Returns after binding; errors are logged.
pub fn start(app: AppHandle, db: Arc<Db>, engine: Arc<crate::engine::Engine>) {
    let port = port_from_settings(&db) as u16;
    tauri::async_runtime::spawn(async move {
        if let Err(e) = serve(app, db, engine, port).await {
            log::error!("MCP server failed on port {port}: {e}");
        }
    });
}

async fn serve(app: AppHandle, db: Arc<Db>, engine: Arc<crate::engine::Engine>, port: u16) -> Result<(), String> {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            // surface the failure so the UI doesn't show a healthy service
            let _ = app.emit("mcp://status", json!({ "port": port, "running": false, "error": e.to_string() }));
            return Err(format!("bind: {e}"));
        }
    };
    log::info!("MCP server listening on http://127.0.0.1:{port}/mcp");
    let _ = app.emit("mcp://status", json!({ "port": port, "running": true }));
    let state = Arc::new(McpState { db, commit_lock: std::sync::Mutex::new(()), engine: std::sync::OnceLock::from(engine) });
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                log::error!("MCP accept: {e}");
                continue;
            }
        };
        let state = state.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = handle_conn(stream, state, app, port).await;
        });
    }
}

async fn handle_conn(mut stream: tokio::net::TcpStream, state: Arc<McpState>, app: AppHandle, port: u16) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    // read until end of headers
    let header_end = loop {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let (method, path) = {
        let mut it = request_line.split_whitespace();
        (
            it.next().unwrap_or_default().to_string(),
            it.next().unwrap_or_default().to_string(),
        )
    };
    let mut content_length = 0usize;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 8 * 1024 * 1024 {
            break;
        }
    }
    let body = buf[header_end.min(buf.len())..].to_vec();

    let (path_only, query) = match path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (path.clone(), String::new()),
    };

    let (status, content_type, payload) = match (method.as_str(), path_only.as_str()) {
        ("GET", "/skills") => (200, "text/markdown; charset=utf-8", skills_document(&query, port)),
        ("POST", "/mcp") => {
            let parsed: Result<Value, _> = serde_json::from_slice(&body);
            match parsed {
                Ok(msg) => (200, "application/json", handle_mcp(&state, &app, msg).to_string()),
                Err(e) => (
                    200,
                    "application/json",
                    json!({"jsonrpc":"2.0","id":Value::Null,"error":{"code":-32700,"message":format!("请求不是合法的 JSON：{e}。MCP 端点要求 POST JSON-RPC 2.0，例如 {{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}}。")}}).to_string(),
                ),
            }
        }
        ("GET", "/health") => (200, "application/json", json!({"ok":true}).to_string()),
        _ => (404, "text/plain; charset=utf-8", "not found".to_string()),
    };

    let resp = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if status == 200 { "OK" } else { "Not Found" },
        payload.len()
    );
    stream.write_all(resp.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.write_all(payload.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------- skills document ----------

fn skills_document(query: &str, port: u16) -> String {
    let shared = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("sharedsession="))
        .map(|s| s.to_string());
    let base = format!("http://127.0.0.1:{port}/mcp");
    let context_line = match &shared {
        Some(id) if !id.is_empty() && id != "1" => format!(
            "- **你的共享上下文 ID：`{id}`**。后续所有工具调用的 `context_id` 参数都使用它。"
        ),
        _ => "- 你还没有绑定共享上下文：请向用户询问 contextId（用户可在使驾界面点击「复制接入提示词」获得），再继续。".to_string(),
    };
    format!(
        r#"# 使驾 · 共享上下文技能（ShiDrive Shared Context Skill）

你好，编程 Agent。本技能让你接入使驾的**共享上下文**：多个 Agent（Codex / ZCode / …）围绕同一个项目上下文协作时，通过它共享 概述 / 待办 / 进展 / 注意 / 约束，并像 git 一样留下提交记录。用户可以在界面上查看每一次提交（版本 + 摘要 + 涉及文件 + 快照），所以**提交质量 = 你的工作交接质量**。

## 连接信息

- MCP 端点（POST，JSON-RPC 2.0）：`{base}`
- 若你的运行环境已由使驾注入名为 `shidrive` 的 MCP 服务，直接调用工具即可，无需手工发 HTTP。
{context_line}

## 可用工具

| 工具 | 用途 |
|---|---|
| `context_get` | 读取完整共享上下文（概述/待办/进展/注意/约束 + 当前版本号） |
| `context_get_version` | 只查当前版本号与最近提交摘要（开工前检查是否落后） |
| `context_update` | 提交更新：`base_version` + 本次摘要 + 涉及文件 + 变更的章节 |
| `context_history` | 浏览提交历史（谁在什么时候做了什么、改了哪些文件） |
| `context_search` | 按关键词搜索历史提交的摘要 / 涉及文件 / 内容 |
| `workflow_list` | 列出项目的工作流（默认「无项目」；可用 project_name 或 context_id 定位项目） |
| `workflow_get` / `workflow_create` / `workflow_update` / `workflow_delete` | 查看/创建/更新/删除项目工作流。把用户常跑的命令（打包、构建、git 提交等）沉淀为工作流，用户可在使驾界面一键运行 |
| `workflow_run` | 立即运行某个工作流 |

## 工作规范

1. **开工前**：先 `context_get_version` 检查版本；如果你记忆中的版本落后，先 `context_get` 阅读最新内容再动手。
2. **提交**：完成一轮工作后调用 `context_update`：
   - `base_version` = 你读到的版本号（不一致会被拒绝，像 git 冲突一样，请重新读取后合并再提交）；
   - `summary` = 本次提交摘要（做了什么、为什么）；
   - `files` = 本次改动的核心文件，**用相对于项目根目录的路径**（大量修改时只记关键文件）；
   - `updates` 里只放你实际改动的章节：`overview` / `todos`(含 status: open/done) / `progress` / `notes` / `constraints`。
3. **精简记录**：概述 ≤ {MAX_OVERVIEW} 字、约束 ≤ {MAX_CONSTRAINTS} 字、每条记录 ≤ {MAX_ENTRY} 字、每类 ≤ {MAX_ENTRIES} 条、摘要 ≤ {MAX_SUMMARY} 字。超限会被拒绝并提示你精简；更旧的细节依赖 `context_history` / `context_search`，不要把上下文写成流水账。

## 交接质量要求（提交前自检，最重要）

> 自检问题：**一个新的 Agent 只读这份共享上下文（不看聊天记录），能否不问用户就直接接手工作？** 不能就补齐再提交。

- `overview` 必须包含：
  - 项目一句话定位 + 当前阶段；
  - **关键文档路径**：README / 需求或计划文档 / 设计文档 / 关键配置的位置（绝对路径或项目相对路径）；
  - 构建、运行、测试命令；技术栈要点；重要端口与数据位置。
  - 若发现 overview 缺少以上任何一项，**主动补全**，不要原样维持。
- `progress` 每条 = 做了什么 + 为什么 + 结果（成功/失败/遗留）。保留最近 3~5 轮工作的完整轨迹；被精简掉的旧轨迹靠提交历史追溯。
- 用户的新诉求、纠正、偏好 → 记入 `notes` 或 `constraints`，避免下一个 Agent 重蹈覆辙。
- 完成用户明确交办的事项后，把对应 `todos` 标记 done 并移入 `progress`。

## 章节语义

- `overview`：项目定位 + 文档/构建入口 + 当前阶段归纳（精炼但足以交接）
- `todos`：未完成事项
- `progress`：已完成事项、成功/失败尝试（可交接的工作轨迹）
- `notes`：不明确、待确认、需要注意的事项
- `constraints`：用户明确要求的项目约束
"#,
        MAX_OVERVIEW = MAX_OVERVIEW,
        MAX_CONSTRAINTS = MAX_CONSTRAINTS,
        MAX_ENTRY = MAX_ENTRY,
        MAX_ENTRIES = MAX_ENTRIES,
        MAX_SUMMARY = MAX_SUMMARY,
    )
}

// ---------- MCP protocol ----------

fn handle_mcp(state: &McpState, app: &AppHandle, msg: Value) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or_default().to_string();
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    match method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "shidrive", "title": "使驾共享上下文", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
        "notifications/initialized" | "notification/initialized" => {
            json!({"jsonrpc":"2.0","id":id,"result":{}})
        }
        "ping" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
        "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools": tool_definitions()}}),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(state, app, &name, &args) {
                Ok(text) => json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"content":[{"type":"text","text":text}],"isError":false}
                }),
                Err(e) => json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"content":[{"type":"text","text":e}],"isError":true}
                }),
            }
        }
        other => json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32601,"message":format!("未知方法 {other}。可用方法：initialize、tools/list、tools/call（工具见 tools/list）。")}
        }),
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "context_get",
            "description": "读取共享上下文完整内容（概述/待办/进展/注意/约束 + 当前版本号 version）",
            "inputSchema": { "type":"object", "required":["context_id"], "properties":{
                "context_id": {"type":"string","description":"共享上下文 ID（由接入提示词或用户给出）"}
            }}
        },
        {
            "name": "context_get_version",
            "description": "轻量查询当前版本号与最近一次提交摘要，用于开工前判断本地认知是否落后",
            "inputSchema": { "type":"object", "required":["context_id"], "properties":{
                "context_id": {"type":"string"}
            }}
        },
        {
            "name": "context_update",
            "description": "提交一轮共享上下文更新（git 式）。base_version 不匹配当前版本会被拒绝并提示重新读取合并。",
            "inputSchema": { "type":"object", "required":["context_id","base_version","summary"], "properties":{
                "context_id": {"type":"string"},
                "base_version": {"type":"integer","description":"你读取内容时的版本号"},
                "summary": {"type":"string","description":"本次提交摘要：做了哪些工作"},
                "files": {"type":"array","items":{"type":"string"},"description":"本次改动的核心文件路径"},
                "updates": { "type":"object", "description":"要替换的章节（只传改动过的）", "properties":{
                    "overview": {"type":"string"},
                    "todos": {"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["open","done"]}},"required":["content"]}},
                    "progress": {"type":"array","items":{"type":"object","properties":{"content":{"type":"string"}},"required":["content"]}},
                    "notes": {"type":"array","items":{"type":"object","properties":{"content":{"type":"string"}},"required":["content"]}},
                    "constraints": {"type":"string"}
                }}
            }}
        },
        {
            "name": "context_history",
            "description": "浏览提交历史（最新在前），可按 limit 控制条数",
            "inputSchema": { "type":"object", "required":["context_id"], "properties":{
                "context_id": {"type":"string"},
                "limit": {"type":"integer","description":"默认 20"}
            }}
        },
        {
            "name": "context_search",
            "description": "按关键词搜索历史提交的摘要/涉及文件/内容，用于找回被精简掉的信息",
            "inputSchema": { "type":"object", "required":["context_id","query"], "properties":{
                "context_id": {"type":"string"},
                "query": {"type":"string"},
                "limit": {"type":"integer"}
            }}
        },
        {
            "name": "workflow_list",
            "description": "列出项目的工作流（默认「无项目」，可用 project_name 或 context_id 指定项目）",
            "inputSchema": { "type":"object", "properties":{
                "project_name": {"type":"string","description":"项目名称（默认：无项目）"},
                "context_id": {"type":"string","description":"共享上下文 ID，用于定位其所属项目"}
            }}
        },
        {
            "name": "workflow_get",
            "description": "读取工作流完整定义（步骤/连线/环境变量/调度）",
            "inputSchema": { "type":"object", "required":["name"], "properties":{
                "name": {"type":"string","description":"工作流名称或 id"},
                "project_name": {"type":"string"},
                "context_id": {"type":"string"}
            }}
        },
        {
            "name": "workflow_create",
            "description": "在项目中创建工作流。steps 为节点数组，按数组顺序自动连线（或用 edges 自由连线）。适合把常用命令（打包/构建/git 提交等）沉淀为可复用工作流。",
            "inputSchema": { "type":"object", "required":["name","steps"], "properties":{
                "name": {"type":"string","description":"工作流名称"},
                "steps": {"type":"array","description":"节点数组。类型：start(开始,可选) / shell(命令) / agent(AI) / delay(等待) / env(变量)。shell 形如 {\"type\":\"shell\",\"name\":\"构建\",\"command\":\"pnpm build\",\"cwd\":\"\",\"shell\":\"cmd|powershell|python\",\"timeout_sec\":null,\"continue_on_error\":false}；agent 形如 {\"type\":\"agent\",\"name\":\"\",\"context_id\":\"<上下文id>\",\"agent_type\":\"codex|zcode\",\"prompt\":\"...\",\"session_id\":null,\"timeout_sec\":null,\"continue_on_error\":false}；env 形如 {\"type\":\"env\",\"name\":\"\",\"vars\":{\"KEY\":\"value\"}}；delay 形如 {\"type\":\"delay\",\"name\":\"\",\"seconds\":5}","items": {"type":"object"}},
                "edges": {"type":"array","description":"连线 [{\"from\":0,\"to\":1}]，索引指向 steps；省略则按顺序链接","items": {"type":"object"}},
                "env": {"type":"object","description":"工作流级环境变量（值支持 {{date:格式}} 插值）"},
                "schedule": {"type":"object","description":"定时调度 {\"kind\":\"interval|daily|weekly|once\", ...}；省略为手动"},
                "description": {"type":"string"},
                "enabled": {"type":"boolean","description":"默认 true"},
                "project_name": {"type":"string"},
                "context_id": {"type":"string"}
            }}
        },
        {
            "name": "workflow_update",
            "description": "更新工作流（可改名称/描述/步骤/连线/环境变量/调度/启用）。未提供的字段保持不变。",
            "inputSchema": { "type":"object", "required":["name"], "properties":{
                "name": {"type":"string","description":"要更新的工作流名称或 id"},
                "new_name": {"type":"string"},
                "description": {"type":"string"},
                "steps": {"type":"array"},
                "edges": {"type":"array"},
                "env": {"type":"object"},
                "schedule": {"type":"object"},
                "enabled": {"type":"boolean"},
                "project_name": {"type":"string"},
                "context_id": {"type":"string"}
            }}
        },
        {
            "name": "workflow_delete",
            "description": "删除工作流（运行历史一并删除）",
            "inputSchema": { "type":"object", "required":["name"], "properties":{
                "name": {"type":"string"},
                "project_name": {"type":"string"},
                "context_id": {"type":"string"}
            }}
        },
        {
            "name": "workflow_run",
            "description": "立即运行工作流（异步启动，日志见使驾界面）",
            "inputSchema": { "type":"object", "required":["name"], "properties":{
                "name": {"type":"string"},
                "project_name": {"type":"string"},
                "context_id": {"type":"string"}
            }}
        }
    ])
}

fn tool_error(msg: String) -> Result<Value, String> {
    Err(msg)
}

const KNOWN_TOOLS: [&str; 11] = [
    "context_get",
    "context_get_version",
    "context_update",
    "context_history",
    "context_search",
    "workflow_list",
    "workflow_get",
    "workflow_create",
    "workflow_update",
    "workflow_delete",
    "workflow_run",
];

pub(crate) fn call_tool(state: &McpState, _app: &AppHandle, name: &str, args: &Value) -> Result<Value, String> {
    // 工具名校验优先于 context 校验，报错指引才准确
    if !KNOWN_TOOLS.contains(&name) {
        return tool_error(format!(
            "未知工具 {name}。可用工具：{}。",
            KNOWN_TOOLS.join("、")
        ));
    }
    // 工作流工具不依赖共享上下文，走独立分发
    if name.starts_with("workflow_") {
        return call_workflow_tool(state, name, args);
    }
    let ctx_id = args.get("context_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if ctx_id.is_empty() {
        return tool_error("缺少必填参数 context_id。它来自你的接入提示词（/skills?sharedsession=<id>），或直接向用户询问。".into());
    }
    let ctx = match state.db.get_context_row(&ctx_id) {
        Some(c) => c,
        None => {
            return tool_error(format!(
                "共享上下文 {ctx_id} 不存在。请确认 context_id 是否正确（使驾界面的「复制接入提示词」按钮可重新获取）。"
            ))
        }
    };

    match name {
        "context_get" => {
            let (head, snap) = snapshot_live(state, &ctx_id)?;
            Ok(json_bytes(&json!({
                "version": head,
                "context": { "id": ctx.id, "name": ctx.name },
                "sections": snap,
                "hint": "提交更新时请携带该 version 作为 base_version；只传你实际改动的章节。"
            })))
        }
        "context_get_version" => {
            let head = state.db.sc_head_seq(&ctx_id)?;
            let last = state.db.sc_list_commits(&ctx_id, 1)?.into_iter().next();
            Ok(json_bytes(&json!({
                "version": head,
                "last_commit": last.map(|c| json!({
                    "seq": c.seq, "summary": c.summary, "agent": c.agent_type, "time": c.created_at
                })).unwrap_or(Value::Null)
            })))
        }
        "context_update" => {
            let base = args.get("base_version").and_then(|v| v.as_i64()).ok_or_else(|| {
                "缺少必填参数 base_version（整数，来自 context_get 返回的 version）。".to_string()
            })?;
            let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            if summary.trim().is_empty() {
                return tool_error("缺少必填参数 summary（本次提交摘要）。请简述这轮做了哪些工作。".into());
            }
            // Serialize update flows so the conflict message below can quote the exact
            // commits that won; the authoritative CAS check happens inside the DB
            // transaction (sc_commit_patch) together with every write.
            let _guard = state.commit_lock.lock().unwrap_or_else(|p| p.into_inner());
            let head = state.db.sc_head_seq(&ctx_id)?;
            if base != head {
                let last = state.db.sc_list_commits(&ctx_id, 3)?;
                return tool_error(format!(
                    "版本冲突：你提交的 base_version={base}，但当前已是 {head}（其他 Agent 或用户在你读取后更新了上下文，类似 git 冲突）。请先调用 context_get 获取最新内容，合并你的改动后用新的 version 重新提交。最近提交：{}",
                    serde_json::to_string(&last.iter().map(|c| json!({"seq":c.seq,"summary":c.summary})).collect::<Vec<_>>()).unwrap_or_default()
                ));
            }

            // FIX-01：同一请求可能同时带 overview 与 constraints —— 合并为一次写入。
            let updates = args.get("updates").cloned().unwrap_or(json!({}));
            let conv = |list: &Value| -> Vec<ScEntry> {
                list.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| {
                                let content = e.get("content").and_then(|c| c.as_str())?.to_string();
                                Some(ScEntry {
                                    content: clamp_str(&content, MAX_ENTRY, "entry"),
                                    status: e.get("status").and_then(|s| s.as_str()).unwrap_or("open").to_string(),
                                })
                            })
                            .take(MAX_ENTRIES)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let patch = crate::db::ScPatch {
                overview: updates.get("overview").and_then(|v| v.as_str()).map(|v| clamp_str(v, MAX_OVERVIEW, "overview")),
                constraints: updates.get("constraints").and_then(|v| v.as_str()).map(|v| clamp_str(v, MAX_CONSTRAINTS, "constraints")),
                todos: updates.get("todos").map(&conv),
                progress: updates.get("progress").map(&conv),
                notes: updates.get("notes").map(&conv),
            };
            let files: Vec<String> = args
                .get("files")
                .and_then(|f| f.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).take(MAX_FILES).collect())
                .unwrap_or_default();
            let meta = crate::db::ScCommitMeta {
                agent_type: args.get("agent_type").and_then(|v| v.as_str()).unwrap_or("unknown").to_string(),
                session_id: args.get("session_id").and_then(|v| v.as_str()).map(|s| s.to_string()),
                summary: clamp_str(&summary, MAX_SUMMARY, "summary"),
                files: files.clone(),
            };
            // One transaction: version check → overview/constraints → entry lists →
            // snapshot → commit row. Any failure rolls everything back.
            let commit = state.db.sc_commit_patch(&ctx_id, head, &patch, &meta).map_err(|e| {
                if let Some(actual) = e.strip_prefix("__conflict__:") {
                    format!(
                        "版本冲突：提交时检测到当前版本已是 {actual}（其他 Agent 在你读取后先提交了）。请先调用 context_get 获取最新内容，合并你的改动后用新的 version 重新提交。"
                    )
                } else {
                    e
                }
            })?;
            let _ = _app.emit(
                "sc://commit",
                json!({"contextId": ctx_id, "seq": commit.seq, "summary": commit.summary, "agent": commit.agent_type}),
            );
            Ok(json_bytes(&json!({
                "version": commit.seq,
                "message": format!("提交成功：v{} 已写入共享上下文，界面已同步。", commit.seq),
                "summary": commit.summary,
                "files": files
            })))
        }
        "context_history" => {
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20).clamp(1, 100);
            let commits = state.db.sc_list_commits(&ctx_id, limit)?;
            let list: Vec<Value> = commits
                .iter()
                .map(|c| json!({"seq": c.seq, "time": c.created_at, "agent": c.agent_type, "summary": c.summary, "files": serde_json::from_str::<Value>(&c.files).unwrap_or(json!([]))}))
                .collect();
            Ok(json_bytes(&json!({"head": state.db.sc_head_seq(&ctx_id)?, "commits": list})))
        }
        "context_search" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            if q.trim().is_empty() {
                return tool_error("缺少必填参数 query（搜索关键词）。".into());
            }
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10).clamp(1, 50);
            let hits = state.db.sc_search(&ctx_id, &q, limit)?;
            if hits.is_empty() {
                return Ok(json_bytes(&json!({"matches": [], "hint": "没有匹配的提交；试试更短的关键词，或到 context_history 浏览。"})));
            }
            let list: Vec<Value> = hits
                .iter()
                .map(|c| json!({"seq": c.seq, "time": c.created_at, "agent": c.agent_type, "summary": c.summary, "files": serde_json::from_str::<Value>(&c.files).unwrap_or(json!([]))}))
                .collect();
            Ok(json_bytes(&json!({"matches": list})))
        }
        other => tool_error(format!("未知工具 {other}。")),
    }
}

fn json_bytes(v: &Value) -> Value {
    // tools return text content; pass pretty JSON string
    Value::String(serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()))
}

fn clamp_str(s: &str, max: usize, field: &str) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(&format!("…（超过 {field} 上限 {max} 字，已被截断；请保持精简，细节写入提交摘要或用 context_search 查历史）"));
    out
}

/// Snapshot the CURRENT live context tables (working tree).
fn snapshot_live(state: &McpState, context_id: &str) -> Result<(i64, ScSnapshot), String> {
    state.db.sc_snapshot(context_id)
}

// ---------- workflow MCP tools ----------

/// Resolve the target project: explicit project_name → context_id's project → 无项目.
fn resolve_project_id(state: &McpState, args: &Value) -> Result<String, String> {
    if let Some(name) = args.get("project_name").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
        let projects = state.db.list_projects()?;
        return projects
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.id.clone())
            .ok_or_else(|| format!("项目「{name}」不存在。可先在使驾界面创建，或用 workflow_list 查看现有项目的工作流。"));
    }
    if let Some(cid) = args.get("context_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        if let Some(c) = state.db.get_context_row(cid) {
            return Ok(c.project_id);
        }
        return Err(format!("共享上下文 {cid} 不存在。可改用 project_name 参数直接指定项目。"));
    }
    Ok(crate::models::NO_PROJECT_ID.to_string())
}

fn find_workflow_by_name_or_id(state: &McpState, project_id: &str, key: &str) -> Result<Option<Workflow>, String> {
    let list = state.db.list_workflows(project_id)?;
    Ok(list.into_iter().find(|w| w.id == key || w.name == key))
}

fn wf_public(w: &Workflow) -> Value {
    json!({
        "id": w.id,
        "name": w.name,
        "description": w.description,
        "enabled": w.enabled,
        "trigger_type": w.trigger_type,
        "schedule": w.schedule,
        "steps": w.steps,
        "edges": w.edges,
        "env": w.env,
        "next_run_at": w.next_run_at,
        "last_run_at": w.last_run_at,
    })
}

fn parse_steps(raw: &Value) -> Result<Vec<WorkflowStep>, String> {
    match raw {
        Value::Array(_) => serde_json::from_value::<Vec<WorkflowStep>>(raw.clone()).map_err(|e| {
            format!("steps 格式不正确：{e}。每个步骤形如 {{\"type\":\"shell\",\"name\":\"构建\",\"command\":\"pnpm build\",\"cwd\":\"\",\"shell\":\"cmd\"}}；可选 type: start/agent/delay/env。")
        }),
        _ => Err("steps 必须是数组。".into()),
    }
}

fn call_workflow_tool(state: &McpState, name: &str, args: &Value) -> Result<Value, String> {
    let project_id = resolve_project_id(state, args)?;
    match name {
        "workflow_list" => {
            let list = state.db.list_workflows(&project_id)?;
            let projects = state.db.list_projects()?;
            let pname = projects.iter().find(|p| p.id == project_id).map(|p| p.name.clone()).unwrap_or_default();
            let items: Vec<Value> = list
                .iter()
                .map(|w| {
                    json!({
                        "id": w.id,
                        "name": w.name,
                        "trigger": if w.trigger_type == "schedule" && w.enabled { "schedule" } else { "manual" },
                        "steps": w.steps.len(),
                        "next_run_at": w.next_run_at,
                    })
                })
                .collect();
            Ok(json_bytes(&json!({ "project": pname, "workflows": items })))
        }
        "workflow_get" => {
            let key = args.get("name").or_else(|| args.get("id")).and_then(|v| v.as_str()).unwrap_or_default();
            if key.is_empty() {
                return tool_error("缺少参数 name（工作流名称或 id）。".into());
            }
            let w = find_workflow_by_name_or_id(state, &project_id, key)?
                .ok_or_else(|| format!("工作流「{key}」不存在于该项目。可用 workflow_list 查看。"))?;
            Ok(json_bytes(&wf_public(&w)))
        }
        "workflow_create" => {
            let wname = args.get("name").and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
            if wname.is_empty() {
                return tool_error("缺少参数 name（工作流名称）。".into());
            }
            if find_workflow_by_name_or_id(state, &project_id, &wname)?.is_some() {
                return tool_error(format!("工作流「{wname}」已存在。如需修改请用 workflow_update，或换一个名称。"));
            }
            let steps = match args.get("steps") {
                Some(raw) => parse_steps(raw)?,
                None => Vec::new(),
            };
            let edges: Vec<Edge> = args.get("edges").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
            let n = steps.len();
            if edges.iter().any(|e| e.from as usize >= n || e.to as usize >= n) {
                return tool_error(format!("edges 引用了不存在的步骤索引（共 {n} 个步骤）。"));
            }
            let env: std::collections::BTreeMap<String, String> = args
                .get("env")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let schedule: Option<ScheduleConfig> = args.get("schedule").and_then(|v| serde_json::from_value(v.clone()).ok());
            let trigger_type = args.get("trigger_type").and_then(|v| v.as_str()).unwrap_or(if schedule.is_some() { "schedule" } else { "manual" });
            let enabled = args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            let description = args.get("description").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            // 自动补「开始」节点：首节点不是 start 时插入
            let mut steps = steps;
            let mut edges = edges;
            if !steps.first().map(|s| s.is_start()).unwrap_or(false) {
                steps.insert(0, WorkflowStep::Start { name: String::new(), x: 60.0, y: 30.0 });
                for e in edges.iter_mut() {
                    e.from += 1;
                    e.to += 1;
                }
                // 开始节点连到原首节点
                edges.insert(0, Edge { from: 0, to: 1 });
            }
            // 未显式给连线时按顺序成链
            if args.get("edges").is_none() {
                edges = (0..steps.len().saturating_sub(1)).map(|i| Edge { from: i as i64, to: (i + 1) as i64 }).collect();
            }
            // 自动布局：无坐标时按索引排一列
            for (i, st) in steps.iter_mut().enumerate() {
                let m = st.meta();
                if m.x == 0.0 && m.y == 0.0 {
                    st.set_pos(60.0, 30.0 + i as f64 * 100.0);
                }
            }
            let w = state.db.create_workflow(&project_id, &wname, &description, enabled, trigger_type, schedule.as_ref(), &steps, &env, &edges)?;
            Ok(json_bytes(&json!({ "created": true, "workflow": wf_public(&w) })))
        }
        "workflow_update" => {
            let key = args.get("name").or_else(|| args.get("id")).and_then(|v| v.as_str()).unwrap_or_default();
            if key.is_empty() {
                return tool_error("缺少参数 name（工作流名称或 id）。".into());
            }
            let mut w = find_workflow_by_name_or_id(state, &project_id, key)?
                .ok_or_else(|| format!("工作流「{key}」不存在。"))?;
            if let Some(v) = args.get("new_name").and_then(|v| v.as_str()) {
                w.name = v.to_string();
            }
            if let Some(v) = args.get("description").and_then(|v| v.as_str()) {
                w.description = v.to_string();
            }
            let steps_given = args.get("steps").map(|raw| parse_steps(raw)).transpose()?;
            let edges_given = match args.get("edges") {
                Some(raw) => Some(serde_json::from_value::<Vec<Edge>>(raw.clone()).map_err(|e| format!("edges 格式不正确：{e}"))?),
                None => None,
            };
            if let Some(mut steps) = steps_given {
                let mut edges = edges_given.clone().unwrap_or_default();
                if !steps.first().map(|s| s.is_start()).unwrap_or(false) {
                    steps.insert(0, WorkflowStep::Start { name: String::new(), x: 60.0, y: 30.0 });
                    for e in edges.iter_mut() {
                        e.from += 1;
                        e.to += 1;
                    }
                    edges.insert(0, Edge { from: 0, to: 1 });
                }
                if edges_given.is_none() {
                    edges = (0..steps.len().saturating_sub(1)).map(|i| Edge { from: i as i64, to: (i + 1) as i64 }).collect();
                }
                let n = steps.len();
                if edges.iter().any(|e| e.from as usize >= n || e.to as usize >= n) {
                    return tool_error(format!("edges 引用了不存在的步骤索引（共 {n} 个步骤）。"));
                }
                w.steps = steps;
                w.edges = edges;
            } else if let Some(edges) = edges_given {
                let n = w.steps.len();
                if edges.iter().any(|e| e.from as usize >= n || e.to as usize >= n) {
                    return tool_error(format!("edges 引用了不存在的步骤索引（共 {n} 个步骤）。"));
                }
                w.edges = edges;
            }
            if let Some(raw) = args.get("env") {
                w.env = serde_json::from_value(raw.clone()).map_err(|e| format!("env 格式不正确：{e}"))?;
            }
            if let Some(raw) = args.get("schedule") {
                w.schedule = serde_json::from_value(raw.clone()).ok();
                if w.schedule.is_some() && w.trigger_type != "schedule" {
                    w.trigger_type = "schedule".into();
                }
            }
            if let Some(v) = args.get("trigger_type").and_then(|v| v.as_str()) {
                w.trigger_type = v.to_string();
            }
            if let Some(v) = args.get("enabled").and_then(|v| v.as_bool()) {
                w.enabled = v;
            }
            let w = state.db.update_workflow(&w)?;
            Ok(json_bytes(&json!({ "updated": true, "workflow": wf_public(&w) })))
        }
        "workflow_delete" => {
            let key = args.get("name").or_else(|| args.get("id")).and_then(|v| v.as_str()).unwrap_or_default();
            if key.is_empty() {
                return tool_error("缺少参数 name（工作流名称或 id）。".into());
            }
            let w = find_workflow_by_name_or_id(state, &project_id, key)?
                .ok_or_else(|| format!("工作流「{key}」不存在。"))?;
            state.db.delete_workflow(&w.id)?;
            Ok(json_bytes(&json!({ "deleted": true, "name": w.name })))
        }
        "workflow_run" => {
            let key = args.get("name").or_else(|| args.get("id")).and_then(|v| v.as_str()).unwrap_or_default();
            if key.is_empty() {
                return tool_error("缺少参数 name（工作流名称或 id）。".into());
            }
            let w = find_workflow_by_name_or_id(state, &project_id, key)?
                .ok_or_else(|| format!("工作流「{key}」不存在。"))?;
            if let Some(engine) = state.engine.get() {
                engine.run_now(&w.id)?;
                Ok(json_bytes(&json!({ "started": true, "name": w.name, "hint": "运行日志见使驾「工作流」页的运行历史，或右上角「运行中任务」。" })))
            } else {
                tool_error("工作流引擎尚未就绪。".into())
            }
        }
        _ => tool_error(format!("未知工具 {name}。")),
    }
}
