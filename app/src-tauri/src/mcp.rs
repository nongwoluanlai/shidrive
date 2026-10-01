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
use crate::models::{ScheduleConfig, ScEntry, ScSnapshot, Workflow, WorkflowStep};

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
    let mut origin = String::new();
    let mut host = String::new();
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if lower.starts_with("origin:") {
            origin = line[7..].trim().to_string();
        } else if lower.starts_with("host:") {
            host = line[5..].trim().to_string();
        }
    }
    // 安全：本服务无鉴权且可创建/运行工作流（= 执行命令）。浏览器里任意网页都能向
    // 127.0.0.1 发「简单请求」（text/plain POST 无需预检，body 照样被解析执行），
    // DNS 重绑定还能绕过同源读取响应。Agent/CLI 客户端不带 Origin，且 Host 为本机，
    // 因此拒绝「非本机 Origin」与「非本机 Host」即可挡住浏览器侧攻击而不影响正常接入。
    if !local_host_ok(&host) || (!origin.is_empty() && !local_origin_ok(&origin)) {
        let body = "forbidden: non-local Origin/Host";
        let resp = format!("HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let _ = stream.write_all(resp.as_bytes()).await;
        return Ok(());
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

    // 按「请求」判定（而非响应体）：通知 / 客户端回包只回 202 空体；initialize 才下发会话头。
    let mut is_initialize = false;
    let (status, content_type, payload) = match (method.as_str(), path_only.as_str()) {
        ("GET", "/skills") => (200, "text/markdown; charset=utf-8", skills_document(&query, port)),
        ("POST", "/mcp") => {
            let parsed: Result<Value, _> = serde_json::from_slice(&body);
            match parsed {
                Ok(msg) => match classify_mcp_message(&msg) {
                    McpMessageKind::NoReply => {
                        // 通知 / 客户端回包不产生副作用（当前的通知均为 initialized、cancelled 等），
                        // 不调用 handle_mcp，免得无 id 的 tools/call 被当作通知静默执行。
                        // Streamable HTTP 规范要求回 202 Accepted + 空体，返回 JSON 会让 rmcp 判定通道关闭。
                        (202, "application/json", String::new())
                    }
                    McpMessageKind::Initialize => {
                        is_initialize = true;
                        (200, "application/json", handle_mcp(&state, &app, msg).to_string())
                    }
                    McpMessageKind::Request => (200, "application/json", handle_mcp(&state, &app, msg).to_string()),
                },
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

    // initialize 响应附带 Mcp-Session-Id（Streamable HTTP MCP 规范），
    // 部分客户端（rmcp）依赖它维持会话
    let session_header = if is_initialize && status == 200 {
        format!("Mcp-Session-Id: {}\r\n", uuid::Uuid::new_v4())
    } else {
        String::new()
    };
    let status_text = match status {
        200 => "OK",
        202 => "Accepted",
        _ => "Not Found",
    };
    let resp = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{session_header}Connection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(resp.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.write_all(payload.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Debug, PartialEq)]
enum McpMessageKind {
    /// 有 id 的 initialize 请求：响应附带 Mcp-Session-Id
    Initialize,
    /// 其他有 id 的请求：正常返回 JSON-RPC 响应
    Request,
    /// 通知（有 method 无 id）或客户端回包（无 method）：202 空体
    NoReply,
}

fn classify_mcp_message(msg: &Value) -> McpMessageKind {
    let has_id = msg.get("id").is_some_and(|id| !id.is_null());
    match msg.get("method").and_then(Value::as_str) {
        Some(_) if !has_id => McpMessageKind::NoReply,
        Some("initialize") => McpMessageKind::Initialize,
        Some(_) => McpMessageKind::Request,
        // 无 method：客户端对服务器请求的 result/error 回包
        None if msg.get("result").is_some() || msg.get("error").is_some() => McpMessageKind::NoReply,
        None => McpMessageKind::Request,
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn notifications_get_202_and_only_initialize_gets_session() {
        assert_eq!(classify_mcp_message(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})), McpMessageKind::NoReply);
        assert_eq!(classify_mcp_message(&json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{}})), McpMessageKind::NoReply);
        assert_eq!(classify_mcp_message(&json!({"jsonrpc":"2.0","id":1,"result":{}})), McpMessageKind::NoReply);
        assert_eq!(classify_mcp_message(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}})), McpMessageKind::Initialize);
        assert_eq!(classify_mcp_message(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})), McpMessageKind::Request);
        // 畸形消息（无 method 无 result）仍走正常路径，由 handle_mcp 返回错误
        assert_eq!(classify_mcp_message(&json!({"jsonrpc":"2.0","id":3})), McpMessageKind::Request);
    }
}

/// Host 头必须是本机（防 DNS 重绑定）。缺失 Host 的 HTTP/1.0 简易客户端放行。
pub(crate) fn local_host_ok(host: &str) -> bool {
    if host.is_empty() { return true; }
    let h = host.to_ascii_lowercase();
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.rsplit_once(':').map(|(n, p)| if p.chars().all(|c| c.is_ascii_digit()) { n } else { h.as_str() }).unwrap_or(&h)
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

/// 浏览器来源只允许本机页面（含使驾自身 WebView）；"null"（沙箱 iframe / file://）拒绝。
pub(crate) fn local_origin_ok(origin: &str) -> bool {
    let o = origin.trim().to_ascii_lowercase();
    if o == "tauri://localhost" { return true; }
    let rest = match o.strip_prefix("http://").or_else(|| o.strip_prefix("https://")) {
        Some(r) => r,
        None => return false,
    };
    let host = rest.split('/').next().unwrap_or("");
    host == "tauri.localhost" || local_host_ok(host)
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
| `workflow_run` | 立即运行某个工作流，返回 run_id |
| `workflow_run_status` | 按 run_id 查询运行状态与日志末尾（运行后用它确认结果） |

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
            "description": "在项目中创建工作流。返回 graph（最终索引与连线的简明结构）和 auto_adjustments（自动做了什么），请据此核对。连线写错（越界/自环/成环/连到注释）会直接报错而不是静默丢弃。",
            "inputSchema": { "type":"object", "required":["name","steps"], "properties":{
                "name": {"type":"string","description":"工作流名称"},
                "steps": {"type":"array","description":"节点数组，数组下标即节点索引（连线用它）。可省略 start：会自动插入到最前（此时你的索引整体 +1，返回的 graph 字段会给出最终索引）。类型：\n- shell：{\"type\":\"shell\",\"name\":\"构建\",\"command\":\"pnpm build\",\"cwd\":\"\",\"shell\":\"cmd|powershell|python\",\"timeout_sec\":600,\"continue_on_error\":false}（cwd 留空=项目根目录）\n- agent：{\"type\":\"agent\",\"name\":\"审查\",\"context_id\":\"<上下文id>\",\"agent_type\":\"codex|zcode\",\"prompt\":\"...\",\"session_id\":null}（session_id 为空=临时会话）\n- env：{\"type\":\"env\",\"name\":\"\",\"vars\":{\"KEY\":\"value\"}}\n- delay：{\"type\":\"delay\",\"seconds\":5}\n- balloon（系统气泡通知）：{\"type\":\"balloon\",\"title\":\"完成\",\"message\":\"...\",\"click_action\":\"none|open|url\",\"click_target\":\"\",\"sound\":false}\n- note（画布注释，不执行、不要连线）：{\"type\":\"note\",\"text\":\"...\"}\n模板：{{env.KEY}}、{{date:%Y%m%d}}；shell 节点结束后产出 {{env.<节点名>_stdout}} / _stderr / _exit（无名时为 step<序号>）。坐标 x/y 可省略——会按连线自动分层布局。","items": {"type":"object"}},
                "edges": {"type":"array","description":"有向连线（无环）。写法任选：{\"from\":0,\"to\":1}、{\"source\":\"构建\",\"target\":\"测试\"}、[0,1]；端点可用索引或唯一的节点名。索引基于你传入的 steps。省略或 [] = 按顺序串联。所有无前驱节点会自动从开始节点并行起跑。并行示例（打包后同时上传和通知）：steps=[打包,上传,通知] edges=[[0,1],[0,2]]。"},
                "auto_layout": {"type":"boolean","description":"强制按连线重新布局（默认仅在有节点未给坐标时布局）"},
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
            "description": "更新工作流（可改名称/描述/步骤/连线/环境变量/调度/启用）。未提供的字段保持不变。传 steps 即整体替换节点（规则同 workflow_create）；只传 edges 时索引基于当前节点（先用 workflow_get 查看 graph）；只想整理画布可仅传 auto_layout=true。",
            "inputSchema": { "type":"object", "required":["name"], "properties":{
                "name": {"type":"string","description":"要更新的工作流名称或 id"},
                "new_name": {"type":"string"},
                "description": {"type":"string"},
                "steps": {"type":"array","description":"整体替换节点，格式同 workflow_create.steps","items":{"type":"object"}},
                "edges": {"type":"array","description":"格式同 workflow_create.edges"},
                "auto_layout": {"type":"boolean","description":"按连线重新自动布局"},
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
            "description": "立即运行工作流（异步启动），返回 run_id；随后用 workflow_run_status 轮询结果与日志",
            "inputSchema": { "type":"object", "required":["name"], "properties":{
                "name": {"type":"string"},
                "project_name": {"type":"string"},
                "context_id": {"type":"string"}
            }}
        },
        {
            "name": "workflow_run_status",
            "description": "查询一次工作流运行的状态（running/success/failed/stopped）与日志末尾",
            "inputSchema": { "type":"object", "required":["run_id"], "properties":{
                "run_id": {"type":"string","description":"workflow_run 返回的 run_id"},
                "tail_lines": {"type":"integer","description":"返回日志末尾行数，默认 80"}
            }}
        }
    ])
}

fn tool_error(msg: String) -> Result<Value, String> {
    Err(msg)
}

const KNOWN_TOOLS: [&str; 12] = [
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
    "workflow_run_status",
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
            let mut v = wf_public(&w);
            v["graph"] = json!(crate::wf_graph::describe(&w.steps, &w.edges));
            Ok(json_bytes(&v))
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
            let raw_edges = match args.get("edges") {
                Some(raw) if !raw.is_null() => Some(crate::wf_graph::parse_edges(raw, &steps)?),
                _ => None,
            };
            let relayout = args.get("auto_layout").and_then(|v| v.as_bool()).unwrap_or(false);
            let env: std::collections::BTreeMap<String, String> = args
                .get("env")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let schedule: Option<ScheduleConfig> = args.get("schedule").and_then(|v| serde_json::from_value(v.clone()).ok());
            let trigger_type = args.get("trigger_type").and_then(|v| v.as_str()).unwrap_or(if schedule.is_some() { "schedule" } else { "manual" });
            let enabled = args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            let description = args.get("description").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let norm = crate::wf_graph::normalize(steps, raw_edges, relayout)?;
            let (steps, edges) = (norm.steps, norm.edges);
            let w = state.db.create_workflow(&project_id, &wname, &description, enabled, trigger_type, schedule.as_ref(), &steps, &env, &edges)?;
            Ok(json_bytes(&json!({
                "created": true,
                "auto_adjustments": norm.notes,
                "graph": crate::wf_graph::describe(&w.steps, &w.edges),
                "workflow": wf_public(&w),
            })))
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
            let relayout = args.get("auto_layout").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut adjustments: Vec<String> = Vec::new();
            let graph_touched = steps_given.is_some() || args.get("edges").is_some() || relayout;
            if graph_touched {
                // 只改连线时，端点索引基于现有 steps（含已有的开始节点）
                let steps = steps_given.unwrap_or_else(|| w.steps.clone());
                let raw_edges = match args.get("edges") {
                    Some(raw) if !raw.is_null() => Some(crate::wf_graph::parse_edges(raw, &steps)?),
                    // 只重排布局：沿用现有连线
                    _ if args.get("steps").is_none() => Some(w.edges.iter().map(|e| (e.from as usize, e.to as usize)).collect()),
                    _ => None,
                };
                let norm = crate::wf_graph::normalize(steps, raw_edges, relayout)?;
                w.steps = norm.steps;
                w.edges = norm.edges;
                adjustments = norm.notes;
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
            Ok(json_bytes(&json!({
                "updated": true,
                "auto_adjustments": adjustments,
                "graph": crate::wf_graph::describe(&w.steps, &w.edges),
                "workflow": wf_public(&w),
            })))
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
                let run_id = engine.run_now(&w.id)?;
                Ok(json_bytes(&json!({
                    "started": true,
                    "name": w.name,
                    "run_id": run_id,
                    "hint": "用 workflow_run_status 传入 run_id 查看状态与日志（status: running/success/failed/stopped）。",
                })))
            } else {
                tool_error("工作流引擎尚未就绪。".into())
            }
        }
        "workflow_run_status" => {
            let run_id = args.get("run_id").and_then(|v| v.as_str()).unwrap_or_default();
            if run_id.is_empty() {
                return tool_error("缺少参数 run_id（workflow_run 的返回值）。".into());
            }
            let r = state.db.get_run(run_id)?.ok_or_else(|| format!("运行记录 {run_id} 不存在。"))?;
            let tail_lines = args.get("tail_lines").and_then(|v| v.as_u64()).unwrap_or(80).clamp(1, 1000) as usize;
            let lines: Vec<&str> = r.log.lines().collect();
            let tail = lines[lines.len().saturating_sub(tail_lines)..].join("\n");
            Ok(json_bytes(&json!({
                "run_id": r.id,
                "status": r.status,
                "trigger": r.trigger,
                "started_at": r.started_at,
                "finished_at": r.finished_at,
                "log_tail": tail,
                "log_lines_total": lines.len(),
            })))
        }
        _ => tool_error(format!("未知工具 {name}。")),
    }
}
