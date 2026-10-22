//! ShiDrive 内置 ACP Agent（`shidrive.exe --acp-agent`）。
//!
//! 轻量实现：纯 std 线程 + ureq + serde，按行（newline-delimited JSON-RPC）走 stdio。
//! 模型走 OpenAI 兼容 /chat/completions（流式优先，非流式回退）；文件与命令工具复用
//! `coding_mcp::call_tool`（外部编程接入同一套实现，含根目录越界 / 符号链接校验）。
//! 会话持久化到 `SHIDRIVE_AGENT_HOME/sessions`，支持 session/new|load|resume|list|close。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::coding_mcp::{self, Cfg};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MODEL_TOOL_CHARS: usize = 24_000;
const UI_TOOL_CHARS: usize = 4_000;
const DIFF_MAX_BYTES: usize = 200 * 1024;
pub const NOT_CONFIGURED: &str = "ShiDrive Agent 尚未配置：请在「设置 → Agent 管理 → ShiDrive」填写接口地址、模型与 API Key 后保存。";

// ---------------------------------------------------------------- log

static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 诊断日志：`SHIDRIVE_AGENT_HOME/agent.log`（超过 2MB 轮转为 agent.log.old），同时写 stderr。
/// 只记录元信息与错误（接口状态码 / 错误正文、工具名与结果状态），不记 API Key 与对话正文。
fn alog(msg: impl AsRef<str>) {
    let line = format!("[{}] {}", chrono::Local::now().format("%m-%d %H:%M:%S"), msg.as_ref());
    eprintln!("[shidrive-agent] {}", msg.as_ref());
    let Some(path) = LOG_PATH.lock().ok().and_then(|g| g.clone()) else { return };
    if std::fs::metadata(&path).map(|m| m.len() > 2 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

fn init_log(home: &Path) {
    let _ = std::fs::create_dir_all(home);
    if let Ok(mut g) = LOG_PATH.lock() { *g = Some(home.join("agent.log")); }
}

// ---------------------------------------------------------------- config

#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub models: Vec<String>,
    pub max_steps: usize,
    pub context_chars: usize,
    pub temperature: Option<f64>,
    pub home: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let home = var("SHIDRIVE_AGENT_HOME").map(PathBuf::from).unwrap_or_else(|| {
            match std::env::var_os("APPDATA") {
                Some(a) => PathBuf::from(a).join("com.shidrive.desktop").join("agent"),
                None => std::env::temp_dir().join("shidrive-agent"),
            }
        });
        Config {
            base_url: var("SHIDRIVE_AGENT_BASE_URL").unwrap_or_default(),
            api_key: var("SHIDRIVE_AGENT_API_KEY").unwrap_or_default(),
            model: var("SHIDRIVE_AGENT_MODEL").unwrap_or_default(),
            models: split_models(&var("SHIDRIVE_AGENT_MODELS").unwrap_or_default()),
            max_steps: var("SHIDRIVE_AGENT_MAX_STEPS").and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(60),
            context_chars: var("SHIDRIVE_AGENT_CONTEXT_CHARS").and_then(|v| v.parse().ok()).filter(|n| *n >= 8000).unwrap_or(360_000),
            temperature: var("SHIDRIVE_AGENT_TEMPERATURE").and_then(|v| v.parse().ok()),
            home,
        }
    }
    fn configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
    fn model_list(&self) -> Vec<String> {
        let mut v = Vec::new();
        for m in std::iter::once(&self.model).chain(self.models.iter()) {
            if !m.is_empty() && !v.contains(m) { v.push(m.clone()); }
        }
        v
    }
}

pub(crate) fn split_models(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in s.split([',', '，', '\n', ';']).map(str::trim).filter(|m| !m.is_empty()) {
        if !out.iter().any(|x| x == m) { out.push(m.to_string()); }
    }
    out
}

pub(crate) fn chat_url(base: &str) -> Result<String, String> {
    let b = base.trim().trim_end_matches('/');
    if b.is_empty() { return Err("请先填写接口地址（Base URL）".into()); }
    if !(b.starts_with("http://") || b.starts_with("https://")) { return Err("接口地址需以 http:// 或 https:// 开头".into()); }
    Ok(if b.ends_with("/chat/completions") { b.to_string() } else { format!("{b}/chat/completions") })
}

pub(crate) fn models_url(base: &str) -> Result<String, String> {
    let c = chat_url(base)?;
    Ok(format!("{}/models", c.trim_end_matches("/chat/completions")))
}

// ---------------------------------------------------------------- session data

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
struct SessionData {
    id: String,
    cwd: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    messages: Vec<Value>,
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn valid_sid(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn sessions_dir(cfg: &Config) -> PathBuf {
    cfg.home.join("sessions")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!("tmp{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&tmp, bytes).map_err(|e| format!("写入会话失败：{e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("保存会话失败：{e}")
    })
}

fn save_session(cfg: &Config, d: &SessionData) -> Result<(), String> {
    if !valid_sid(&d.id) { return Err("非法会话 ID".into()); }
    let dir = sessions_dir(cfg);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建会话目录失败：{e}"))?;
    let body = serde_json::to_vec(d).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(format!("{}.json", d.id)), &body)?;
    let meta = json!({
        "sessionId": d.id, "cwd": d.cwd, "title": d.title, "createdAt": d.created_at,
        "updatedAt": d.updated_at, "messages": d.messages.len(),
    });
    write_atomic(&dir.join(format!("{}.meta.json", d.id)), meta.to_string().as_bytes())
}

fn load_session(cfg: &Config, id: &str) -> Result<SessionData, String> {
    if !valid_sid(id) { return Err("非法会话 ID".into()); }
    let p = sessions_dir(cfg).join(format!("{id}.json"));
    let raw = std::fs::read(&p).map_err(|_| format!("会话不存在：{id}"))?;
    serde_json::from_slice(&raw).map_err(|e| format!("会话文件损坏：{e}"))
}

fn norm_cwd(s: &str) -> String {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    s.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn disp(p: &str) -> String {
    p.strip_prefix(r"\\?\").unwrap_or(p).to_string()
}

// ---------------------------------------------------------------- shared state

type Out = Box<dyn Fn(&str) + Send + Sync>;
type RpcResult = Result<Value, (i64, String)>;

struct McpTool {
    api_name: String,
    client: usize,
    tool: String,
    spec: Value,
}

struct Live {
    data: Mutex<SessionData>,
    cancel: Mutex<Arc<AtomicBool>>,
    busy: AtomicBool,
    allow: Mutex<HashSet<String>>,
    mcp: Vec<McpClient>,
    mcp_tools: Vec<McpTool>,
}

struct Shared {
    cfg: Config,
    out: Out,
    next_id: AtomicU64,
    pending: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    sessions: Mutex<HashMap<String, Arc<Live>>>,
}

impl Shared {
    fn new(cfg: Config, out: Out) -> Self {
        Shared { cfg, out, next_id: AtomicU64::new(0), pending: Mutex::new(HashMap::new()), sessions: Mutex::new(HashMap::new()) }
    }
    fn send(&self, v: &Value) {
        (self.out)(&v.to_string());
    }
    fn respond(&self, id: Value, r: RpcResult) {
        let msg = match r {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        };
        self.send(&msg);
    }
    fn update(&self, sid: &str, update: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": sid, "update": update } }));
    }
    /// 向客户端发请求（如 session/request_permission）；取消后最多再等 3 秒客户端回包。
    fn client_request(&self, method: &str, params: Value, cancel: &AtomicBool) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id.to_string(), tx);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let start = Instant::now();
        let mut cancelled_at: Option<Instant> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(msg) => {
                    if let Some(e) = msg.get("error") {
                        return Err(e.get("message").and_then(Value::as_str).unwrap_or("客户端返回错误").to_string());
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancel.load(Ordering::SeqCst) {
                        let t = *cancelled_at.get_or_insert_with(Instant::now);
                        if t.elapsed() > Duration::from_secs(3) {
                            self.pending.lock().unwrap().remove(&id.to_string());
                            return Err("已取消".into());
                        }
                    }
                    if start.elapsed() > Duration::from_secs(1800) {
                        self.pending.lock().unwrap().remove(&id.to_string());
                        return Err("等待客户端响应超时".into());
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("客户端连接已断开".into()),
            }
        }
    }
    fn live(&self, sid: &str) -> Option<Arc<Live>> {
        self.sessions.lock().unwrap().get(sid).cloned()
    }
}

// ---------------------------------------------------------------- entry / dispatch

pub fn run() -> i32 {
    let cfg = Config::from_env();
    init_log(&cfg.home);
    alog(format!("启动 v{VERSION}：接口 {}，模型 {}，已配置={}", cfg.base_url, cfg.model, cfg.configured()));
    let out: Out = Box::new(|line: &str| {
        let so = std::io::stdout();
        let mut l = so.lock();
        let _ = l.write_all(line.as_bytes());
        let _ = l.write_all(b"\n");
        let _ = l.flush();
    });
    let sh = Arc::new(Shared::new(cfg, out));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        match line {
            Ok(l) => handle_line(&sh, &l),
            Err(_) => break,
        }
    }
    0
}

fn handle_line(sh: &Arc<Shared>, line: &str) {
    let line = line.trim();
    if line.is_empty() { return; }
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Array(items)) => items.into_iter().for_each(|m| handle_msg(sh, m)),
        Ok(m) => handle_msg(sh, m),
        Err(e) => sh.respond(Value::Null, Err((-32700, format!("Parse error: {e}")))),
    }
}

fn handle_msg(sh: &Arc<Shared>, msg: Value) {
    let method = msg.get("method").and_then(Value::as_str).map(str::to_string);
    let id = msg.get("id").cloned().filter(|v| !v.is_null());
    match (method, id) {
        (Some(m), Some(id)) => {
            let sh = sh.clone();
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            std::thread::spawn(move || {
                let r = dispatch(&sh, &m, &params);
                sh.respond(id, r);
            });
        }
        (Some(m), None) => {
            if m == "session/cancel" {
                if let Some(sid) = msg["params"]["sessionId"].as_str() {
                    if let Some(l) = sh.live(sid) { l.cancel.lock().unwrap().store(true, Ordering::SeqCst); }
                }
            }
        }
        (None, Some(id)) => {
            let key = id.to_string();
            if let Some(tx) = sh.pending.lock().unwrap().remove(&key) { let _ = tx.send(msg); }
        }
        _ => {}
    }
}

fn dispatch(sh: &Arc<Shared>, method: &str, p: &Value) -> RpcResult {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
                "mcpCapabilities": { "http": true, "sse": false },
                "sessionCapabilities": { "list": {} }
            },
            "authMethods": [],
            "agentInfo": { "name": "shidrive-agent", "title": "ShiDrive", "version": VERSION }
        })),
        "authenticate" => Ok(json!({})),
        "session/new" => session_new(sh, p),
        "session/load" => session_open(sh, p, true),
        "session/resume" => session_open(sh, p, false),
        "session/list" => session_list(sh, p),
        "session/close" => {
            let sid = p["sessionId"].as_str().unwrap_or_default();
            if let Some(l) = sh.sessions.lock().unwrap().remove(sid) { l.cancel.lock().unwrap().store(true, Ordering::SeqCst); }
            Ok(json!({}))
        }
        "session/set_mode" => {
            let live = need_live(sh, p)?;
            let mode = p["modeId"].as_str().unwrap_or_default();
            if !["ask", "auto", "read"].contains(&mode) { return Err((-32602, format!("未知模式：{mode}"))); }
            let mut d = live.data.lock().unwrap();
            d.mode = mode.to_string();
            let _ = save_session(&sh.cfg, &d);
            Ok(json!({}))
        }
        "session/set_config_option" | "session/set_model" => {
            let live = need_live(sh, p)?;
            let id = p.get("configId").or_else(|| p.get("configOptionId")).and_then(Value::as_str)
                .unwrap_or(if method == "session/set_model" { "model" } else { "" });
            let value = p.get("value").or_else(|| p.get("modelId")).and_then(Value::as_str).unwrap_or_default().trim().to_string();
            if id != "model" { return Err((-32602, format!("未知配置项：{id}"))); }
            if value.is_empty() { return Err((-32602, "模型不能为空".into())); }
            let mut d = live.data.lock().unwrap();
            d.model = value;
            let _ = save_session(&sh.cfg, &d);
            Ok(if method == "session/set_model" { json!({}) } else { json!({ "configOptions": config_options(&sh.cfg, &d) }) })
        }
        "session/prompt" => session_prompt(sh, p),
        _ => Err((-32601, format!("Method not found: {method}"))),
    }
}

fn need_live(sh: &Arc<Shared>, p: &Value) -> Result<Arc<Live>, (i64, String)> {
    let sid = p["sessionId"].as_str().unwrap_or_default();
    if let Some(l) = sh.live(sid) { return Ok(l); }
    // 进程重启后客户端可能直接对旧会话操作：静默从磁盘恢复（不重放）
    let d = load_session(&sh.cfg, sid).map_err(|e| (-32002, e))?;
    let l = Arc::new(make_live(d, &Value::Null));
    sh.sessions.lock().unwrap().insert(sid.to_string(), l.clone());
    Ok(l)
}

fn modes(d: &SessionData) -> Value {
    json!({
        "currentModeId": if d.mode.is_empty() { "ask" } else { d.mode.as_str() },
        "availableModes": [
            { "id": "ask", "name": "询问", "description": "修改文件、执行命令前请求批准" },
            { "id": "auto", "name": "自动", "description": "工具调用自动执行，不再询问" },
            { "id": "read", "name": "只读", "description": "仅查看与搜索，不修改文件、不执行命令" }
        ]
    })
}

fn config_options(cfg: &Config, d: &SessionData) -> Value {
    let mut list = cfg.model_list();
    let cur = if d.model.is_empty() { cfg.model.clone() } else { d.model.clone() };
    if !cur.is_empty() && !list.contains(&cur) { list.push(cur.clone()); }
    let options: Vec<Value> = list.iter().map(|m| json!({ "value": m, "name": m })).collect();
    json!([{ "id": "model", "name": "模型", "category": "model", "type": "select", "currentValue": cur, "options": options }])
}

fn make_live(d: SessionData, mcp_servers: &Value) -> Live {
    let mut clients = Vec::new();
    let mut tools = Vec::new();
    for s in mcp_servers.as_array().into_iter().flatten() {
        let kind = s.get("type").and_then(Value::as_str).unwrap_or("stdio");
        let name = s.get("name").and_then(Value::as_str).unwrap_or("mcp").to_string();
        let url = s.get("url").and_then(Value::as_str).unwrap_or_default();
        if kind != "http" || url.is_empty() {
            alog(format!("跳过不支持的 MCP 服务器 {name}（{kind}）"));
            continue;
        }
        let headers: Vec<(String, String)> = s.get("headers").and_then(Value::as_array).into_iter().flatten()
            .filter_map(|h| Some((h.get("name")?.as_str()?.to_string(), h.get("value")?.as_str()?.to_string())))
            .collect();
        match McpClient::connect(&name, url, headers) {
            Ok(c) => {
                let idx = clients.len();
                for t in &c.tools {
                    let Some(tn) = t.get("name").and_then(Value::as_str) else { continue };
                    let api_name = mcp_api_name(&name, tn);
                    if tools.iter().any(|x: &McpTool| x.api_name == api_name) { continue; }
                    let params = t.get("inputSchema").cloned().filter(Value::is_object)
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                    let desc = t.get("description").and_then(Value::as_str).unwrap_or_default();
                    tools.push(McpTool {
                        api_name: api_name.clone(), client: idx, tool: tn.to_string(),
                        spec: json!({ "type": "function", "function": {
                            "name": api_name, "description": format!("[MCP {name}] {}", cap(desc, 1000)), "parameters": params } }),
                    });
                }
                clients.push(c);
            }
            Err(e) => alog(format!("连接 MCP 服务器 {name} 失败：{e}")),
        }
    }
    Live {
        data: Mutex::new(d),
        cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
        busy: AtomicBool::new(false),
        allow: Mutex::new(HashSet::new()),
        mcp: clients,
        mcp_tools: tools,
    }
}

fn mcp_api_name(server: &str, tool: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>();
    let mut n = format!("mcp__{}__{}", clean(server), clean(tool));
    n.truncate(64);
    n
}

fn canonical_cwd(cwd: &str) -> Result<PathBuf, (i64, String)> {
    if cwd.trim().is_empty() { return Err((-32602, "缺少 cwd".into())); }
    let p = Path::new(cwd);
    if !p.is_absolute() { return Err((-32602, format!("cwd 必须为绝对路径：{cwd}"))); }
    std::fs::canonicalize(p).ok().filter(|c| c.is_dir()).ok_or_else(|| (-32602, format!("工作目录不存在：{cwd}")))
}

fn session_new(sh: &Arc<Shared>, p: &Value) -> RpcResult {
    if !sh.cfg.configured() { return Err((-32603, NOT_CONFIGURED.into())); }
    let cwd = p["cwd"].as_str().unwrap_or_default();
    canonical_cwd(cwd)?;
    let now = now_iso();
    let d = SessionData {
        id: uuid::Uuid::new_v4().to_string(),
        cwd: Some(cwd.trim_end_matches(['\\', '/'])).filter(|c| !c.is_empty()).unwrap_or(cwd).to_string(),
        created_at: now.clone(), updated_at: now, mode: "ask".into(), model: sh.cfg.model.clone(),
        ..Default::default()
    };
    let resp = json!({ "sessionId": d.id, "modes": modes(&d), "configOptions": config_options(&sh.cfg, &d) });
    let id = d.id.clone();
    let live = Arc::new(make_live(d, &p["mcpServers"]));
    alog(format!("新会话 {id}：cwd={cwd}，MCP 工具 {} 个", live.mcp_tools.len()));
    sh.sessions.lock().unwrap().insert(id, live);
    Ok(resp)
}

fn session_open(sh: &Arc<Shared>, p: &Value, replay: bool) -> RpcResult {
    let sid = p["sessionId"].as_str().unwrap_or_default().to_string();
    if let Some(l) = sh.live(&sid) {
        if l.busy.load(Ordering::SeqCst) { return Err((-32603, "会话正在运行，请稍后再试".into())); }
    }
    let mut d = load_session(&sh.cfg, &sid).map_err(|e| (-32002, e))?;
    if let Some(cwd) = p["cwd"].as_str().filter(|c| !c.trim().is_empty()) {
        canonical_cwd(cwd)?;
        d.cwd = cwd.trim_end_matches(['\\', '/']).to_string();
    }
    if replay { replay_history(sh, &d); }
    let resp = json!({ "modes": modes(&d), "configOptions": config_options(&sh.cfg, &d) });
    let live = Arc::new(make_live(d, &p["mcpServers"]));
    alog(format!("{}会话 {sid}：MCP 工具 {} 个", if replay { "加载" } else { "恢复" }, live.mcp_tools.len()));
    sh.sessions.lock().unwrap().insert(sid, live);
    Ok(resp)
}

fn session_list(sh: &Arc<Shared>, p: &Value) -> RpcResult {
    let filter = p["cwd"].as_str().filter(|c| !c.is_empty()).map(norm_cwd);
    let mut items: Vec<Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(sessions_dir(&sh.cfg)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".meta.json") { continue; }
            let Ok(raw) = std::fs::read(e.path()) else { continue };
            let Ok(m) = serde_json::from_slice::<Value>(&raw) else { continue };
            if m["messages"].as_u64().unwrap_or(0) == 0 { continue; }
            let cwd = m["cwd"].as_str().unwrap_or_default();
            if let Some(f) = &filter { if &norm_cwd(cwd) != f { continue; } }
            let mut it = json!({ "sessionId": m["sessionId"], "cwd": cwd, "updatedAt": m["updatedAt"] });
            if let Some(t) = m["title"].as_str().filter(|t| !t.is_empty()) { it["title"] = json!(t); }
            items.push(it);
        }
    }
    items.sort_by(|a, b| b["updatedAt"].as_str().unwrap_or("").cmp(a["updatedAt"].as_str().unwrap_or("")));
    let offset: usize = p["cursor"].as_str().and_then(|c| c.parse().ok()).unwrap_or(0);
    let page: Vec<Value> = items.iter().skip(offset).take(100).cloned().collect();
    let mut resp = json!({ "sessions": page });
    if offset + 100 < items.len() { resp["nextCursor"] = json!((offset + 100).to_string()); }
    Ok(resp)
}

fn text_chunk(kind: &str, text: &str) -> Value {
    json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
}

fn replay_history(sh: &Arc<Shared>, d: &SessionData) {
    let results: HashMap<&str, &Value> = d.messages.iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| Some((m["tool_call_id"].as_str()?, m)))
        .collect();
    for m in &d.messages {
        match m["role"].as_str().unwrap_or_default() {
            "user" => {
                let text = m["_display"].as_str().map(str::to_string).unwrap_or_else(|| content_text(&m["content"]));
                if !text.is_empty() { sh.update(&d.id, text_chunk("user_message_chunk", &text)); }
            }
            "assistant" => {
                if let Some(t) = m["content"].as_str().filter(|t| !t.is_empty()) {
                    sh.update(&d.id, text_chunk("agent_message_chunk", t));
                }
                for tc in m["tool_calls"].as_array().into_iter().flatten() {
                    let id = tc["id"].as_str().unwrap_or_default();
                    let r = results.get(id);
                    let raw: Value = serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                    let name = tc["function"]["name"].as_str().unwrap_or_default();
                    let title = r.and_then(|r| r["_title"].as_str()).map(str::to_string).unwrap_or_else(|| tool_title(name, &raw));
                    let status = r.and_then(|r| r["_status"].as_str()).unwrap_or("completed");
                    let mut u = json!({
                        "sessionUpdate": "tool_call", "toolCallId": id, "title": title,
                        "kind": tool_kind(name), "status": status, "rawInput": raw,
                    });
                    if let Some(out) = r.and_then(|r| r["content"].as_str()) {
                        u["content"] = json!([{ "type": "content", "content": { "type": "text", "text": cap(out, 2000) } }]);
                    }
                    sh.update(&d.id, u);
                }
            }
            _ => {}
        }
    }
}

fn content_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().map(|p| match p["type"].as_str() {
            Some("text") => p["text"].as_str().unwrap_or_default().to_string(),
            Some("image_url") => "[图片]".to_string(),
            _ => String::new(),
        }).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

fn cap(s: &str, n: usize) -> String {
    if s.chars().count() <= n { return s.to_string(); }
    let mut o: String = s.chars().take(n).collect();
    o.push('…');
    o
}

/// 头尾保留的截断（模型看工具输出时尾部往往含关键错误信息）
fn cap_mid(s: &str, n: usize) -> String {
    let total = s.chars().count();
    if total <= n { return s.to_string(); }
    let head = n * 2 / 3;
    let tail = n - head;
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().skip(total - tail).collect();
    format!("{h}\n…[省略 {} 字符]…\n{t}", total - head - tail)
}

// ---------------------------------------------------------------- prompt turn

/// RAII：一轮结束（含 panic）时释放 busy 标记
struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); }
}

fn session_prompt(sh: &Arc<Shared>, p: &Value) -> RpcResult {
    if !sh.cfg.configured() { return Err((-32603, NOT_CONFIGURED.into())); }
    let live = need_live(sh, p)?;
    if live.busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        alog("拒绝新消息：上一轮回复仍在进行中");
        return Err((-32603, "上一轮回复仍在进行中".into()));
    }
    let _guard = BusyGuard(&live.busy);
    let cancel = Arc::new(AtomicBool::new(false));
    *live.cancel.lock().unwrap() = cancel.clone();
    let (user_msg, display) = build_user_message(&p["prompt"]);
    let turn_start;
    {
        let mut d = live.data.lock().unwrap();
        turn_start = d.messages.len();
        let mut m = user_msg;
        m["_display"] = json!(display);
        d.messages.push(m);
        if d.title.is_empty() {
            let first = display.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("新会话");
            d.title = cap(first, 40);
        }
        d.updated_at = now_iso();
        let _ = save_session(&sh.cfg, &d);
    }
    let result = run_turn(sh, &live, turn_start, &cancel);
    {
        // 图片只在本轮内发给模型；落盘时替换为占位文字，避免会话文件膨胀
        let mut d = live.data.lock().unwrap();
        if let Some(m) = d.messages.get_mut(turn_start) {
            if m["content"].is_array() {
                let t = content_text(&m["content"]);
                m["content"] = json!(t);
            }
        }
        d.updated_at = now_iso();
        let _ = save_session(&sh.cfg, &d);
    }
    let sid = live.data.lock().unwrap().id.clone();
    match result {
        Ok(reason) => {
            alog(format!("会话 {sid} 本轮结束：{reason}"));
            Ok(json!({ "stopReason": reason }))
        }
        Err(e) if cancel.load(Ordering::SeqCst) => { alog(format!("会话 {sid} 已取消（{e}）")); Ok(json!({ "stopReason": "cancelled" })) }
        Err(e) => {
            alog(format!("会话 {sid} 本轮失败：{e}"));
            // 同时作为正文输出，避免客户端只显示「已中断」而看不到原因（不写入模型历史）
            sh.update(&sid, text_chunk("agent_message_chunk", &format!("\n\n⚠️ {e}")));
            Err((-32603, e))
        }
    }
}

fn build_user_message(prompt: &Value) -> (Value, String) {
    let mut text = String::new();
    let mut display = String::new();
    let mut images: Vec<Value> = Vec::new();
    for b in prompt.as_array().into_iter().flatten() {
        match b["type"].as_str().unwrap_or_default() {
            "text" => {
                let t = b["text"].as_str().unwrap_or_default();
                text.push_str(t);
                display.push_str(t);
            }
            "image" => {
                let data = b["data"].as_str().unwrap_or_default();
                let mime = b["mimeType"].as_str().unwrap_or("image/png");
                if !data.is_empty() {
                    images.push(json!({ "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{data}") } }));
                    display.push_str(" [图片]");
                }
            }
            "resource" => {
                let r = &b["resource"];
                let uri = r["uri"].as_str().unwrap_or_default();
                if let Some(t) = r["text"].as_str() {
                    text.push_str(&format!("\n<file uri=\"{uri}\">\n{}\n</file>\n", cap(t, 100_000)));
                } else {
                    text.push_str(&format!("\n[附件 {uri}（二进制，未内联）]\n"));
                }
                display.push_str(&format!(" @{}", uri.rsplit(['/', '\\']).next().unwrap_or(uri)));
            }
            "resource_link" => {
                let uri = b["uri"].as_str().unwrap_or_default();
                let name = b["name"].as_str().unwrap_or(uri);
                let path = uri.strip_prefix("file:///").or_else(|| uri.strip_prefix("file://")).unwrap_or(uri);
                text.push_str(&format!("\n[引用文件 {name}：{path}]\n"));
                display.push_str(&format!(" @{name}"));
            }
            _ => {}
        }
    }
    let display = display.trim().to_string();
    let msg = if images.is_empty() {
        json!({ "role": "user", "content": text })
    } else {
        let mut parts = vec![json!({ "type": "text", "text": if text.is_empty() { "（见图片）".to_string() } else { text } })];
        parts.extend(images);
        json!({ "role": "user", "content": parts })
    };
    (msg, display)
}

struct ToolCall {
    id: String,
    name: String,
    args: String,
    /// 最近一个参数分片：个别接口每个分片都发完整参数（而非增量），据此纠正
    last: String,
    /// tool_call 上的其他字段（如 Gemini 的 extra_content.google.thought_signature），回传时原样带上
    extra: serde_json::Map<String, Value>,
}

struct Reply {
    content: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
    finish: String,
}

fn run_turn(sh: &Arc<Shared>, live: &Arc<Live>, turn_start: usize, cancel: &AtomicBool) -> Result<&'static str, String> {
    for step in 0..sh.cfg.max_steps {
        if cancel.load(Ordering::SeqCst) { return Ok("cancelled"); }
        let (mode, model, history, cwd) = {
            let d = live.data.lock().unwrap();
            let model = if d.model.is_empty() { sh.cfg.model.clone() } else { d.model.clone() };
            (if d.mode.is_empty() { "ask".to_string() } else { d.mode.clone() }, model, d.messages.clone(), d.cwd.clone())
        };
        let tools = tool_specs(&mode, live);
        let has_ctx = live.mcp_tools.iter().any(|t| t.tool.starts_with("context_"));
        let (api_msgs, api_start) = api_messages(&history, turn_start);
        let (msgs, dropped) = fit_budget(api_msgs, api_start, sh.cfg.context_chars);
        let mut sys = system_prompt(&cwd, &mode, has_ctx);
        if dropped { sys.push_str("\n\n（注意：更早的对话因上下文长度已省略。）"); }
        let mut all = vec![json!({ "role": "system", "content": sys })];
        all.extend(msgs);
        let (all_len, tools_len) = (all.len(), tools.len());
        let mut body = json!({ "model": model, "messages": all, "stream": true, "tools": tools });
        if let Some(t) = sh.cfg.temperature { body["temperature"] = json!(t); }
        let sid = live.data.lock().unwrap().id.clone();
        let reply = chat(sh, &sid, &body, cancel)?;
        alog(format!("会话 {sid} 第 {} 步：模型 {model}，消息 {} 条，工具 {} 个 → finish={}，正文 {} 字，思考 {} 字，工具调用 [{}]",
            step + 1, all_len, tools_len, if reply.finish.is_empty() { "-" } else { reply.finish.as_str() },
            reply.content.chars().count(), reply.reasoning.chars().count(),
            reply.tool_calls.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")));
        let cancelled = cancel.load(Ordering::SeqCst);
        let mut tool_calls = if cancelled { Vec::new() } else { reply.tool_calls };
        for (i, tc) in tool_calls.iter_mut().enumerate() {
            if tc.id.is_empty() { tc.id = format!("call_{}_{}_{i}", turn_start, step); }
            if tc.args.trim().is_empty() { tc.args = "{}".into(); }
        }
        {
            let mut d = live.data.lock().unwrap();
            if !reply.content.is_empty() || !tool_calls.is_empty() {
                let mut m = json!({ "role": "assistant", "content": if reply.content.is_empty() && !tool_calls.is_empty() { Value::Null } else { json!(reply.content) } });
                if !tool_calls.is_empty() {
                    m["tool_calls"] = json!(tool_calls.iter().map(|t| {
                        let mut o = t.extra.clone();
                        o.insert("id".into(), json!(t.id));
                        o.insert("type".into(), json!("function"));
                        o.insert("function".into(), json!({ "name": t.name, "arguments": t.args }));
                        Value::Object(o)
                    }).collect::<Vec<_>>());
                }
                if !reply.reasoning.is_empty() { m["_reasoning"] = json!(reply.reasoning); }
                d.messages.push(m);
            }
            d.updated_at = now_iso();
            let _ = save_session(&sh.cfg, &d);
        }
        if cancelled { return Ok("cancelled"); }
        if tool_calls.is_empty() {
            return Ok(if reply.finish == "length" { "max_tokens" } else { "end_turn" });
        }
        let root = canonical_cwd(&cwd).map_err(|e| e.1)?;
        let cfg = Cfg { root, token: String::new(), auth_user: String::new(), auth_pass: String::new(), exec_enabled: true };
        for tc in &tool_calls {
            let msg = if cancel.load(Ordering::SeqCst) {
                json!({ "role": "tool", "tool_call_id": tc.id, "content": "已取消（用户中断）", "_status": "failed" })
            } else {
                run_tool_call(sh, live, &sid, &cfg, &mode, tc, cancel)
            };
            let mut d = live.data.lock().unwrap();
            d.messages.push(msg);
            d.updated_at = now_iso();
            let _ = save_session(&sh.cfg, &d);
        }
    }
    Ok("max_turn_requests")
}

fn system_prompt(cwd: &str, mode: &str, has_ctx: bool) -> String {
    let mode_desc = match mode {
        "auto" => "自动：工具调用直接执行。",
        "read" => "只读：只能查看与搜索，不能修改文件或执行命令；如需修改请告诉用户切换到「询问」或「自动」模式。",
        _ => "询问：修改文件、移动/删除、执行命令前会请求用户批准；被拒绝时不要重复同一操作，改为说明或询问。",
    };
    let shell = if cfg!(windows) { "Windows，run_command 通过 cmd /C 执行" } else { "类 Unix，run_command 通过 sh -c 执行" };
    let mut s = format!(
        "你是 ShiDrive Agent（使驾内置编程助手），通过工具直接读写用户电脑上的文件、执行命令来完成任务。\n\
工作目录：{}（相对路径以此为根；只能访问该目录内的文件）。\n系统：{shell}。\n当前日期：{}。\n当前模式：{mode_desc}\n\n\
准则：\n- 动手前先了解：用 list_dir / search / read_file 查看相关代码，不要臆测文件内容。\n\
- 修改已有文件优先用 edit_file（old_string 必须与原文逐字一致且唯一，包含足够上下文）；新建或整体重写用 write_file。\n\
- 命令需非交互、可在超时内结束；不要启动常驻服务或打开编辑器。\n\
- 不要编造工具结果；失败时阅读错误信息再调整。\n\
- 完成后用用户的语言简洁总结做了什么、还有什么需要注意。",
        disp(cwd), chrono::Local::now().format("%Y-%m-%d"),
    );
    if has_ctx {
        s.push_str("\n- 已接入 ShiDrive 共享上下文工具（mcp__shidrive__context_*）：开始任务前可先读取上下文，阶段完成后记录进展。");
    }
    s
}

/// 历史 → API 消息：去掉内部字段（以 _ 开头）；只有本轮的 assistant 消息回传 reasoning_content
/// （部分推理模型要求同一轮工具调用链携带思考内容，跨轮携带则会报错）。
/// 历史 → 接口消息：去掉内部字段（`_` 开头），并修复会让接口直接 400 的结构问题：
/// - 思考内容：本轮全部回传；更早轮次只在带 tool_calls 的消息上回传（DeepSeek / Kimi 等思考模型要求）；
/// - tool_calls 参数不是合法 JSON 时改为 `{}`；
/// - 缺少结果的 tool_call 补一条「未执行」结果，没有对应调用的 tool 消息丢弃（中断 / 崩溃后的残留）。
/// 返回 (消息, 本轮在新数组中的起始下标)。
fn api_messages(history: &[Value], turn_start: usize) -> (Vec<Value>, usize) {
    let mut out: Vec<Value> = Vec::with_capacity(history.len());
    let mut new_start = None;
    let mut pending: Vec<String> = Vec::new();
    let close_pending = |out: &mut Vec<Value>, pending: &mut Vec<String>| {
        for id in pending.drain(..) {
            out.push(json!({ "role": "tool", "tool_call_id": id, "content": "（未执行：上一次回复中断）" }));
        }
    };
    for (i, m) in history.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or_default();
        if role == "tool" {
            let id = m["tool_call_id"].as_str().unwrap_or_default();
            let Some(pos) = pending.iter().position(|p| p == id) else { continue };
            pending.remove(pos);
        } else {
            close_pending(&mut out, &mut pending);
        }
        if i >= turn_start && new_start.is_none() { new_start = Some(out.len()); }
        let has_tools = m["tool_calls"].as_array().is_some_and(|a| !a.is_empty());
        let mut o = serde_json::Map::new();
        if let Some(obj) = m.as_object() {
            for (k, v) in obj {
                if k == "_reasoning" {
                    if i >= turn_start || has_tools { o.insert("reasoning_content".into(), v.clone()); }
                } else if k == "tool_calls" {
                    if !has_tools { continue; }
                    let mut calls = v.clone();
                    for c in calls.as_array_mut().into_iter().flatten() {
                        let ok = c["function"]["arguments"].as_str()
                            .is_some_and(|a| serde_json::from_str::<Value>(a).map(|v| v.is_object()).unwrap_or(false));
                        if !ok { c["function"]["arguments"] = json!("{}"); }
                        if let Some(id) = c["id"].as_str() { pending.push(id.to_string()); }
                    }
                    o.insert(k.clone(), calls);
                } else if !k.starts_with('_') {
                    o.insert(k.clone(), v.clone());
                }
            }
        }
        out.push(Value::Object(o));
    }
    // 末尾仍缺结果的调用（本轮正在执行的调用不会出现在这里：执行完才发下一次请求）
    close_pending(&mut out, &mut pending);
    let start = new_start.unwrap_or(out.len());
    (out, start)
}

fn msg_chars(m: &Value) -> usize {
    let mut n = 20;
    match &m["content"] {
        Value::String(s) => n += s.chars().count(),
        Value::Array(parts) => for p in parts {
            n += if p["type"] == "image_url" { 3000 } else { p["text"].as_str().map(|s| s.chars().count()).unwrap_or(0) };
        },
        _ => {}
    }
    if let Some(tc) = m.get("tool_calls") { n += tc.to_string().chars().count(); }
    if let Some(r) = m.get("reasoning_content").and_then(Value::as_str) { n += r.chars().count(); }
    n
}

/// 预算：先省略早期工具输出，再按整轮丢弃最早的对话，最后截断本轮较早的工具输出。
/// 返回 (消息, 是否丢弃过整轮)。
fn fit_budget(mut msgs: Vec<Value>, turn_start: usize, budget: usize) -> (Vec<Value>, bool) {
    let total = |ms: &[Value]| ms.iter().map(msg_chars).sum::<usize>();
    let mut start = turn_start.min(msgs.len());
    if total(&msgs) <= budget { return (msgs, false); }
    for m in msgs[..start].iter_mut() {
        if m["role"] == "tool" {
            if let Some(s) = m["content"].as_str() {
                let n = s.chars().count();
                if n > 600 { m["content"] = json!(format!("{}…[早期工具输出已省略，共 {n} 字符]", cap(s, 300))); }
            }
        }
    }
    let mut dropped = false;
    while start > 0 && total(&msgs) > budget {
        let next_user = msgs[1..start].iter().position(|m| m["role"] == "user").map(|p| p + 1);
        let k = next_user.unwrap_or(start);
        msgs.drain(0..k);
        start -= k;
        dropped = true;
    }
    if total(&msgs) > budget {
        let idx: Vec<usize> = (start..msgs.len()).filter(|&i| msgs[i]["role"] == "tool").collect();
        let keep = idx.len().saturating_sub(2);
        for &i in &idx[..keep] {
            if total(&msgs) <= budget { break; }
            if let Some(s) = msgs[i]["content"].as_str() {
                if s.chars().count() > 800 { msgs[i]["content"] = json!(format!("{}…[工具输出过长已截断]", cap(s, 400))); }
            }
        }
    }
    (msgs, dropped)
}

// ---------------------------------------------------------------- model API

enum Ev {
    Text(String),
    Reasoning(String),
    Tool { index: usize, id: String, name: String, args: String, extra: serde_json::Map<String, Value> },
    Finish(String),
    Done(Result<(), (bool, String)>),
}

fn http_agent(read_secs: u64) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(read_secs)).build()
}

fn classify(e: ureq::Error) -> (bool, String) {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            (code == 429 || code >= 500, format!("接口返回 HTTP {code}：{}", cap(body.trim(), 600)))
        }
        other => (true, format!("请求接口失败：{other}")),
    }
}

fn emit_delta(delta: &Value, tx: &mpsc::Sender<Ev>) -> bool {
    let mut ok = true;
    if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) { ok &= tx.send(Ev::Text(t.into())).is_ok(); }
    let r = delta["reasoning_content"].as_str().or_else(|| delta["reasoning"].as_str());
    if let Some(t) = r.filter(|t| !t.is_empty()) { ok &= tx.send(Ev::Reasoning(t.into())).is_ok(); }
    for (pos, tc) in delta["tool_calls"].as_array().into_iter().flatten().enumerate() {
        ok &= tx.send(Ev::Tool {
            index: tc["index"].as_u64().map(|i| i as usize).unwrap_or(pos),
            id: tc["id"].as_str().unwrap_or_default().into(),
            name: tc["function"]["name"].as_str().unwrap_or_default().into(),
            args: match &tc["function"]["arguments"] {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            },
            extra: tc.as_object().map(|o| o.iter()
                .filter(|(k, v)| !["index", "id", "type", "function"].contains(&k.as_str()) && !v.is_null())
                .map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default(),
        }).is_ok();
    }
    ok
}

fn spawn_chat(url: String, key: String, body: String, tx: mpsc::Sender<Ev>) {
    std::thread::spawn(move || {
        let res = (|| -> Result<(), (bool, String)> {
            let mut req = http_agent(300).post(&url)
                .set("Content-Type", "application/json")
                .set("Accept", "text/event-stream, application/json");
            if !key.is_empty() { req = req.set("Authorization", &format!("Bearer {key}")); }
            let resp = req.send_string(&body).map_err(classify)?;
            let ct = resp.content_type().to_ascii_lowercase();
            if ct.contains("event-stream") {
                let rd = BufReader::new(resp.into_reader());
                for line in rd.lines() {
                    let line = line.map_err(|e| (true, format!("读取流式响应失败：{e}")))?;
                    let Some(data) = line.trim().strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data == "[DONE]" { break; }
                    let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                        return Err((false, format!("接口错误：{}", e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.to_string()))));
                    }
                    let ch = &v["choices"][0];
                    if !emit_delta(&ch["delta"], &tx) { return Ok(()); }
                    if let Some(f) = ch["finish_reason"].as_str() { let _ = tx.send(Ev::Finish(f.into())); }
                }
            } else {
                let text = resp.into_string().map_err(|e| (true, format!("读取响应失败：{e}")))?;
                let v: Value = serde_json::from_str(&text).map_err(|_| (false, format!("接口响应无法解析：{}", cap(&text, 300))))?;
                if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                    return Err((false, format!("接口错误：{e}")));
                }
                let ch = &v["choices"][0];
                let msg = &ch["message"];
                if !emit_delta(msg, &tx) { return Ok(()); }
                if let Some(f) = ch["finish_reason"].as_str() { let _ = tx.send(Ev::Finish(f.into())); }
            }
            Ok(())
        })();
        let _ = tx.send(Ev::Done(res));
    });
}

/// 一次模型调用：流式转发文本 / 思考；未产出任何内容前遇到网络错误、429、5xx 最多重试 2 次。
fn chat(sh: &Arc<Shared>, sid: &str, body: &Value, cancel: &AtomicBool) -> Result<Reply, String> {
    let url = chat_url(&sh.cfg.base_url)?;
    let body_s = body.to_string();
    let mut attempt = 0;
    'retry: loop {
        let (tx, rx) = mpsc::channel();
        spawn_chat(url.clone(), sh.cfg.api_key.clone(), body_s.clone(), tx);
        let mut reply = Reply { content: String::new(), reasoning: String::new(), tool_calls: Vec::new(), finish: String::new() };
        let mut tools: BTreeMap<usize, ToolCall> = BTreeMap::new();
        let mut got_any = false;
        loop {
            if cancel.load(Ordering::SeqCst) { break; }
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(Ev::Text(t)) => { got_any = true; sh.update(sid, text_chunk("agent_message_chunk", &t)); reply.content.push_str(&t); }
                Ok(Ev::Reasoning(t)) => { got_any = true; sh.update(sid, text_chunk("agent_thought_chunk", &t)); reply.reasoning.push_str(&t); }
                Ok(Ev::Tool { index, id, name, args, extra }) => {
                    got_any = true;
                    let e = tools.entry(index).or_insert(ToolCall {
                        id: String::new(), name: String::new(), args: String::new(), last: String::new(), extra: Default::default() });
                    if !id.is_empty() && e.id.is_empty() { e.id = id; }
                    if !name.is_empty() && e.name.is_empty() { e.name = name; }
                    e.args.push_str(&args);
                    if !args.is_empty() { e.last = args; }
                    e.extra.extend(extra);
                }
                Ok(Ev::Finish(f)) => reply.finish = f,
                Ok(Ev::Done(Ok(()))) => break,
                Ok(Ev::Done(Err((retryable, msg)))) => {
                    alog(format!("模型请求失败（第 {} 次，可重试={retryable}）：{msg}", attempt + 1));
                    if !got_any && retryable && attempt < 2 {
                        attempt += 1;
                        let wait = Instant::now();
                        while wait.elapsed() < Duration::from_secs(attempt * 2) {
                            if cancel.load(Ordering::SeqCst) { return Ok(reply); }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        continue 'retry;
                    }
                    return Err(msg);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("模型请求线程意外退出".into()),
            }
        }
        reply.tool_calls = tools.into_values().filter(|t| !t.name.is_empty()).map(fix_args).collect();
        return Ok(reply);
    }
}

/// 参数拼接后不是合法 JSON、但最后一个分片本身是完整对象时（接口每片都发完整参数），改用最后一片。
fn fix_args(mut t: ToolCall) -> ToolCall {
    let ok = |s: &str| serde_json::from_str::<Value>(s).map(|v| v.is_object()).unwrap_or(false);
    if !t.args.trim().is_empty() && !ok(&t.args) && ok(&t.last) {
        alog(format!("工具 {} 的参数分片为完整重复，已按最后一片解析", t.name));
        t.args = std::mem::take(&mut t.last);
    }
    t
}

// ---------------------------------------------------------------- tools

fn fn_spec(name: &str, desc: &str, required: &[&str], props: Value) -> Value {
    json!({ "type": "function", "function": { "name": name, "description": desc,
        "parameters": { "type": "object", "required": required, "properties": props } } })
}

const READ_TOOLS: &[&str] = &["read_file", "list_dir", "search"];

fn tool_specs(mode: &str, live: &Live) -> Vec<Value> {
    let mut v = vec![
        fn_spec("read_file", "读取文本文件（自动识别 UTF-8/UTF-16 BOM）。大文件用 offset/limit 分段读取。", &["path"], json!({
            "path": { "type": "string", "description": "相对工作目录或绝对路径" },
            "offset": { "type": "integer", "description": "起始行（1 起，默认 1）" },
            "limit": { "type": "integer", "description": "行数（默认 1000）" } })),
        fn_spec("list_dir", "列出目录内容（目录在前）。", &[], json!({
            "path": { "type": "string", "description": "默认工作目录" } })),
        fn_spec("search", "在文件中搜索字面文本（跳过 node_modules/.git/target/dist 等），返回 路径:行号: 内容。", &["query"], json!({
            "query": { "type": "string" },
            "path": { "type": "string", "description": "目录或文件，默认工作目录" },
            "glob": { "type": "string", "description": "文件名通配，如 *.rs，默认 *" },
            "ignore_case": { "type": "boolean" },
            "max_results": { "type": "integer", "description": "默认 100" } })),
    ];
    if mode != "read" {
        v.extend([
            fn_spec("write_file", "写入完整文件内容（不存在则创建，父目录自动创建；已存在则整体覆盖）。", &["path", "content"], json!({
                "path": { "type": "string" }, "content": { "type": "string" } })),
            fn_spec("edit_file", "精确替换文件中的一段文本。old_string 必须与文件内容逐字一致（含缩进与换行），默认要求唯一匹配。", &["path", "old_string", "new_string"], json!({
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean", "description": "替换全部匹配，默认 false" } })),
            fn_spec("make_dir", "递归创建目录。", &["path"], json!({ "path": { "type": "string" } })),
            fn_spec("move_path", "移动或重命名文件/目录（dst 为已存在目录时移入其中）。", &["src", "dst"], json!({
                "src": { "type": "string" }, "dst": { "type": "string" } })),
            fn_spec("copy_path", "复制文件或目录（目录递归）。", &["src", "dst"], json!({
                "src": { "type": "string" }, "dst": { "type": "string" } })),
            fn_spec("delete_path", "删除文件或目录（目录需 recursive=true；不可恢复）。", &["path"], json!({
                "path": { "type": "string" }, "recursive": { "type": "boolean" } })),
            fn_spec("run_command", if cfg!(windows) { "执行命令（cmd /C），返回退出码与输出。多行脚本请用 stdin 传给解释器（如 command=python, stdin=脚本）。" } else { "执行命令（sh -c），返回退出码与输出。" }, &["command"], json!({
                "command": { "type": "string" },
                "workdir": { "type": "string", "description": "工作目录，默认会话工作目录" },
                "stdin": { "type": "string", "description": "写入标准输入的内容" },
                "timeout": { "type": "integer", "description": "秒，默认 120，最大 1800" } })),
        ]);
    }
    v.extend(live.mcp_tools.iter().map(|t| t.spec.clone()));
    v
}

fn tool_kind(name: &str) -> &'static str {
    match name {
        "read_file" | "list_dir" => "read",
        "search" => "search",
        "write_file" | "edit_file" | "make_dir" => "edit",
        "move_path" | "copy_path" => "move",
        "delete_path" => "delete",
        "run_command" => "execute",
        _ => "other",
    }
}

fn tool_title(name: &str, a: &Value) -> String {
    let s = |k: &str| a[k].as_str().unwrap_or_default().to_string();
    match name {
        "read_file" => format!("读取 {}", s("path")),
        "list_dir" => format!("列出 {}", a["path"].as_str().unwrap_or(".")),
        "search" => format!("搜索 “{}”", cap(&s("query"), 60)),
        "write_file" => format!("写入 {}", s("path")),
        "edit_file" => format!("编辑 {}", s("path")),
        "make_dir" => format!("创建目录 {}", s("path")),
        "move_path" => format!("移动 {} → {}", s("src"), s("dst")),
        "copy_path" => format!("复制 {} → {}", s("src"), s("dst")),
        "delete_path" => format!("删除 {}", s("path")),
        "run_command" => format!("运行 {}", cap(&s("command"), 80)),
        other => match other.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
            Some((srv, t)) => format!("{srv}: {t}"),
            None => other.to_string(),
        },
    }
}

fn abs_display(root: &Path, raw: &str) -> String {
    let p = Path::new(raw);
    disp(&if p.is_absolute() { p.to_path_buf() } else { root.join(p) }.to_string_lossy())
}

fn run_tool_call(sh: &Arc<Shared>, live: &Live, sid: &str, cfg: &Cfg, mode: &str, tc: &ToolCall, cancel: &AtomicBool) -> Value {
    let kind = tool_kind(&tc.name);
    let parsed: Result<Value, String> = serde_json::from_str::<Value>(&tc.args)
        .map_err(|e| format!("参数不是合法 JSON：{e}"))
        .and_then(|v| if v.is_object() { Ok(v) } else { Err("参数必须是 JSON 对象".into()) });
    let args = parsed.clone().unwrap_or_else(|_| json!({}));
    let title = tool_title(&tc.name, &args);
    let mut locations = Vec::new();
    for k in ["path", "src", "dst"] {
        if let Some(p) = args[k].as_str().filter(|p| !p.is_empty()) { locations.push(json!({ "path": abs_display(&cfg.root, p) })); }
    }
    let tool_call = json!({
        "toolCallId": tc.id, "title": title, "kind": kind, "status": "pending",
        "rawInput": args, "locations": locations,
    });
    let mut u = tool_call.clone();
    u["sessionUpdate"] = json!("tool_call");
    sh.update(sid, u);
    let finish = |status: &str, model_text: String, content: Vec<Value>| -> Value {
        if status != "completed" { alog(format!("工具 {} {status}：{}", tc.name, cap(&model_text, 300))); }
        let mut upd = json!({ "sessionUpdate": "tool_call_update", "toolCallId": tc.id, "status": status });
        if !content.is_empty() { upd["content"] = json!(content); }
        sh.update(sid, upd);
        json!({ "role": "tool", "tool_call_id": tc.id, "content": cap_mid(&model_text, MODEL_TOOL_CHARS),
                "_title": title, "_kind": kind, "_status": status })
    };
    let text_content = |t: &str| json!({ "type": "content", "content": { "type": "text", "text": cap_mid(t, UI_TOOL_CHARS) } });
    if let Err(e) = parsed {
        return finish("failed", e.clone(), vec![text_content(&e)]);
    }
    let is_mcp = tc.name.starts_with("mcp__");
    let builtin_write = !is_mcp && !READ_TOOLS.contains(&tc.name.as_str());
    if mode == "read" && builtin_write {
        let e = "当前为只读模式，无法修改文件或执行命令。请告诉用户切换到「询问」或「自动」模式。".to_string();
        return finish("failed", e.clone(), vec![text_content(&e)]);
    }
    // 询问模式：写入 / 移动 / 删除 / 执行需要用户批准；「本会话始终允许」按类别记住
    if mode == "ask" && builtin_write && !live.allow.lock().unwrap().contains(kind) {
        let params = json!({
            "sessionId": sid,
            "toolCall": tool_call,
            "options": [
                { "optionId": "allow_once", "name": "允许", "kind": "allow_once" },
                { "optionId": "allow_always", "name": "本会话始终允许此类操作", "kind": "allow_always" },
                { "optionId": "reject_once", "name": "拒绝", "kind": "reject_once" }
            ]
        });
        let outcome = sh.client_request("session/request_permission", params, cancel);
        let chosen = match &outcome {
            Ok(r) if r["outcome"]["outcome"] == "selected" => r["outcome"]["optionId"].as_str().unwrap_or_default().to_string(),
            _ => String::new(),
        };
        match chosen.as_str() {
            "allow_once" => {}
            "allow_always" => { live.allow.lock().unwrap().insert(kind.to_string()); }
            "" if cancel.load(Ordering::SeqCst) || outcome.is_err() => {
                return finish("failed", "已取消（用户中断）".into(), vec![]);
            }
            _ => {
                let e = "用户拒绝了此操作。不要重复尝试同一操作；如有必要请说明原因或换一种方式。".to_string();
                return finish("failed", e, vec![text_content("已拒绝")]);
            }
        }
    }
    sh.update(sid, json!({ "sessionUpdate": "tool_call_update", "toolCallId": tc.id, "status": "in_progress" }));
    let res = if is_mcp {
        match live.mcp_tools.iter().find(|t| t.api_name == tc.name) {
            Some(t) => live.mcp[t.client].call(&t.tool, &args).map(|s| (s, None)),
            None => Err(format!("未知工具：{}", tc.name)),
        }
    } else {
        exec_builtin(cfg, &tc.name, &args)
    };
    match res {
        Ok((text, diff)) => {
            let mut content = vec![];
            if let Some((path, old, new)) = diff {
                content.push(json!({ "type": "diff", "path": path, "oldText": old, "newText": new }));
            } else {
                content.push(text_content(&text));
            }
            finish("completed", text, content)
        }
        Err(e) => {
            let msg = format!("错误：{}", disp(&e));
            finish("failed", msg.clone(), vec![text_content(&msg)])
        }
    }
}

type Diff = (String, Option<String>, String);

fn small_text(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() as usize > DIFF_MAX_BYTES { return None; }
    let b = std::fs::read(path).ok()?;
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&b);
    String::from_utf8(b.to_vec()).ok()
}

/// 创建目标路径的缺失父目录：拒绝 ..，最近的已存在祖先必须在 root 内。
fn ensure_dirs(cfg: &Cfg, raw: &str, include_self: bool) -> Result<(), String> {
    let p = Path::new(raw);
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("路径包含 ..，已拒绝。".into());
    }
    let joined = if p.is_absolute() { p.to_path_buf() } else { cfg.root.join(p) };
    let target = if include_self { joined.clone() } else { match joined.parent() { Some(x) => x.to_path_buf(), None => return Ok(()) } };
    let mut anc = target.as_path();
    while !anc.exists() {
        anc = match anc.parent() { Some(x) => x, None => return Err("路径无效".into()) };
    }
    let canon = std::fs::canonicalize(anc).map_err(|e| format!("路径检查失败：{e}"))?;
    if !coding_mcp::within_root(&canon, &cfg.root) {
        return Err(format!("路径越界：{} 不在工作目录内。", joined.display()));
    }
    if !target.exists() {
        std::fs::create_dir_all(&target).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    Ok(())
}

fn exec_builtin(cfg: &Cfg, name: &str, a: &Value) -> Result<(String, Option<Diff>), String> {
    let path = a["path"].as_str().unwrap_or_default();
    match name {
        "read_file" => {
            let limit = a["limit"].as_u64().unwrap_or(1000).clamp(1, 5000);
            let mut args = json!({ "path": path, "limit": limit });
            if let Some(o) = a["offset"].as_u64() { args["offset"] = json!(o); }
            let r = coding_mcp::call_tool(cfg, "pc.fs.read", &args)?;
            let total = r["total_lines"].as_u64().unwrap_or(0);
            let off = r["offset"].as_u64().unwrap_or(1);
            let content = r["content"].as_str().unwrap_or_default();
            let shown = content.split_inclusive('\n').count() as u64;
            let end = off + shown.saturating_sub(1);
            let mut s = format!("{}（共 {total} 行，显示第 {off}-{end} 行）\n{content}", disp(r["path"].as_str().unwrap_or(path)));
            if end < total { s.push_str(&format!("\n…还有 {} 行，可用 offset={} 继续读取", total - end, end + 1)); }
            Ok((s, None))
        }
        "list_dir" => {
            let p = if path.is_empty() { "." } else { path };
            let r = coding_mcp::call_tool(cfg, "pc.fs.list", &json!({ "path": p, "limit": 500 }))?;
            let mut s = format!("{}（共 {} 项）\n", disp(r["path"].as_str().unwrap_or(p)), r["total"]);
            for e in r["entries"].as_array().into_iter().flatten() {
                let n = e["name"].as_str().unwrap_or_default();
                if e["is_dir"].as_bool().unwrap_or(false) { s.push_str(&format!("{n}/\n")); } else { s.push_str(&format!("{n}  ({} B)\n", e["size"])); }
            }
            Ok((s, None))
        }
        "search" => {
            let mut args = a.clone();
            if path.is_empty() { args["path"] = json!("."); }
            let r = coding_mcp::call_tool(cfg, "pc.fs.search", &args)?;
            let m = r["matches"].as_array().cloned().unwrap_or_default();
            let root = disp(&cfg.root.to_string_lossy());
            let mut s = format!("找到 {} 处（扫描 {} 个文件）\n", m.len(), r["scanned_files"]);
            for x in &m {
                let p = disp(x["path"].as_str().unwrap_or_default());
                let rel = p.strip_prefix(&root).map(|r| r.trim_start_matches(['\\', '/']).to_string()).unwrap_or(p);
                s.push_str(&format!("{rel}:{}: {}\n", x["line"], x["text"].as_str().unwrap_or_default()));
            }
            let skipped = r["skipped"].as_array().map(|v| v.len()).unwrap_or(0);
            if skipped > 0 { s.push_str(&format!("（跳过 {skipped} 个文件）\n")); }
            Ok((s, None))
        }
        "write_file" => {
            let Some(content) = a["content"].as_str() else { return Err("缺少参数 content".into()) };
            ensure_dirs(cfg, path, false)?;
            let full_guess = { let p = Path::new(path); if p.is_absolute() { p.to_path_buf() } else { cfg.root.join(p) } };
            let old = small_text(&full_guess);
            let existed = full_guess.exists();
            let r = coding_mcp::call_tool(cfg, "pc.fs.write", &json!({ "path": path, "content": content }))?;
            let shown = disp(r["path"].as_str().unwrap_or(path));
            let diff = (content.len() <= DIFF_MAX_BYTES).then(|| (shown.clone(), old, content.to_string()));
            Ok((format!("已{}{}（{} 字节）", if existed { "覆盖" } else { "创建" }, shown, r["bytes"]), diff))
        }
        "edit_file" => edit_file(cfg, path, a),
        "make_dir" => {
            ensure_dirs(cfg, path, true)?;
            Ok((format!("已创建目录 {path}"), None))
        }
        "move_path" | "copy_path" => {
            let dst = a["dst"].as_str().unwrap_or_default();
            ensure_dirs(cfg, dst, false)?;
            let tool = if name == "move_path" { "pc.fs.move" } else { "pc.fs.copy" };
            let r = coding_mcp::call_tool(cfg, tool, a)?;
            Ok((format!("已{} {} → {}", if name == "move_path" { "移动" } else { "复制" },
                disp(r["src"].as_str().unwrap_or_default()), disp(r["dst"].as_str().unwrap_or_default())), None))
        }
        "delete_path" => {
            let r = coding_mcp::call_tool(cfg, "pc.fs.delete", a)?;
            Ok((format!("已删除 {}", disp(r["path"].as_str().unwrap_or(path))), None))
        }
        "run_command" => {
            let mut args = json!({
                "command": a["command"],
                "timeout": a["timeout"].as_u64().unwrap_or(120).clamp(1, 1800),
                "max_output_chars": 20_000,
            });
            if let Some(w) = a["workdir"].as_str().or_else(|| a["cwd"].as_str()).filter(|w| !w.is_empty()) { args["workdir"] = json!(w); }
            if let Some(i) = a["stdin"].as_str() { args["stdin"] = json!(i); }
            let r = coding_mcp::call_tool(cfg, "pc.process.exec", &args)?;
            let mut s = format!("exit_code: {}\n", r["exit_code"]);
            let out = r["stdout"].as_str().unwrap_or_default();
            let err = r["stderr"].as_str().unwrap_or_default();
            if !out.is_empty() { s.push_str(&format!("--- stdout ---\n{out}\n")); }
            if !err.is_empty() { s.push_str(&format!("--- stderr ---\n{err}\n")); }
            for k in ["stdout_truncated_note", "stderr_truncated_note"] {
                if let Some(n) = r[k].as_str().filter(|n| !n.is_empty()) { s.push_str(n); s.push('\n'); }
            }
            Ok((s, None))
        }
        other => Err(format!("未知工具：{other}")),
    }
}

fn edit_file(cfg: &Cfg, path: &str, a: &Value) -> Result<(String, Option<Diff>), String> {
    let old = a["old_string"].as_str().unwrap_or_default();
    let new = a["new_string"].as_str().ok_or("缺少参数 new_string")?;
    if old.is_empty() { return Err("old_string 不能为空（新建文件请用 write_file）".into()); }
    if old == new { return Err("old_string 与 new_string 相同，无需修改".into()); }
    let replace_all = a["replace_all"].as_bool().unwrap_or(false);
    let full = coding_mcp::safe_path(cfg, path)?;
    let bytes = std::fs::read(&full).map_err(|e| format!("读取失败：{e}"))?;
    if bytes.len() > 8 * 1024 * 1024 { return Err("文件超过 8MiB，无法编辑".into()); }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err("UTF-16 编码文件暂不支持 edit_file，请用 write_file 整体重写（会转为 UTF-8）".into());
    }
    let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let body = if bom { &bytes[3..] } else { &bytes[..] };
    let text = std::str::from_utf8(body).map_err(|_| "文件不是 UTF-8 文本，无法编辑".to_string())?;
    let (mut o, mut n) = (old.to_string(), new.to_string());
    let mut count = text.matches(&o).count();
    if count == 0 && text.contains("\r\n") && !old.contains("\r\n") && old.contains('\n') {
        // 模型给出的片段多为 LF；文件为 CRLF 时按 CRLF 再匹配一次，并保持文件原换行风格
        o = old.replace('\n', "\r\n");
        n = new.replace("\r\n", "\n").replace('\n', "\r\n");
        count = text.matches(&o).count();
    }
    if count == 0 { return Err("未找到 old_string：需与文件内容逐字一致（含缩进、空格与换行）。请先 read_file 确认原文。".into()); }
    if count > 1 && !replace_all {
        return Err(format!("old_string 在文件中出现 {count} 次，不唯一：请包含更多上下文使其唯一，或设置 replace_all=true。"));
    }
    let updated = if replace_all { text.replace(&o, &n) } else { text.replacen(&o, &n, 1) };
    let mut out = String::with_capacity(updated.len() + 3);
    if bom { out.push('\u{feff}'); }
    out.push_str(&updated);
    coding_mcp::call_tool(cfg, "pc.fs.write", &json!({ "path": full.to_string_lossy(), "content": out }))?;
    let shown = disp(&full.to_string_lossy());
    let diff = (text.len() <= DIFF_MAX_BYTES && updated.len() <= DIFF_MAX_BYTES).then(|| (shown.clone(), Some(text.to_string()), updated.clone()));
    Ok((format!("已编辑 {shown}（替换 {} 处）", if replace_all { count } else { 1 }), diff))
}

// ---------------------------------------------------------------- MCP (HTTP) client

struct McpClient {
    url: String,
    headers: Vec<(String, String)>,
    session: Mutex<Option<String>>,
    next: AtomicU64,
    tools: Vec<Value>,
}

impl McpClient {
    fn connect(name: &str, url: &str, headers: Vec<(String, String)>) -> Result<Self, String> {
        let mut c = McpClient { url: url.to_string(), headers, session: Mutex::new(None), next: AtomicU64::new(0), tools: Vec::new() };
        c.rpc("initialize", json!({ "protocolVersion": "2025-03-26", "capabilities": {},
            "clientInfo": { "name": "shidrive-agent", "version": VERSION } }), 15)?;
        let _ = c.post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), 15);
        let list = c.rpc("tools/list", json!({}), 15)?;
        c.tools = list["tools"].as_array().cloned().unwrap_or_default();
        alog(format!("MCP {name}: {} 个工具", c.tools.len()));
        Ok(c)
    }
    fn post(&self, body: &Value, read_secs: u64) -> Result<(String, String), String> {
        let mut req = http_agent(read_secs).post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        for (k, v) in &self.headers { req = req.set(k, v); }
        if let Some(s) = self.session.lock().unwrap().clone() { req = req.set("Mcp-Session-Id", &s); }
        let resp = req.send_string(&body.to_string()).map_err(|e| classify(e).1)?;
        if let Some(s) = resp.header("mcp-session-id") { *self.session.lock().unwrap() = Some(s.to_string()); }
        let ct = resp.content_type().to_ascii_lowercase();
        let text = resp.into_string().map_err(|e| format!("读取 MCP 响应失败：{e}"))?;
        Ok((ct, text))
    }
    fn rpc(&self, method: &str, params: Value, read_secs: u64) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let (ct, text) = self.post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }), read_secs)?;
        let msg = if ct.contains("event-stream") {
            text.lines()
                .filter_map(|l| l.trim().strip_prefix("data:"))
                .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
                .find(|v| v["id"] == json!(id))
                .ok_or("MCP 流式响应中没有结果")?
        } else {
            serde_json::from_str::<Value>(&text).map_err(|_| format!("MCP 响应无法解析：{}", cap(&text, 200)))?
        };
        if let Some(e) = msg.get("error").filter(|e| !e.is_null()) {
            return Err(e["message"].as_str().map(str::to_string).unwrap_or_else(|| e.to_string()));
        }
        Ok(msg["result"].clone())
    }
    fn call(&self, tool: &str, args: &Value) -> Result<String, String> {
        let r = self.rpc("tools/call", json!({ "name": tool, "arguments": args }), 600)?;
        let mut parts: Vec<String> = Vec::new();
        for c in r["content"].as_array().into_iter().flatten() {
            match c["type"].as_str() {
                Some("text") => parts.push(c["text"].as_str().unwrap_or_default().to_string()),
                Some(t) => parts.push(format!("[{t}]")),
                None => {}
            }
        }
        if parts.is_empty() {
            if let Some(sc) = r.get("structuredContent").filter(|v| !v.is_null()) { parts.push(sc.to_string()); }
        }
        let text = parts.join("\n");
        if r["isError"].as_bool().unwrap_or(false) { Err(text) } else { Ok(text) }
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1 as protocol;
    use std::io::Read;
    use std::net::TcpListener;

    fn test_dir() -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("acp-agent-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn check<T: serde::de::DeserializeOwned>(v: &Value) {
        if let Err(e) = serde_json::from_value::<T>(v.clone()) {
            panic!("schema mismatch for {}: {e}\n{v}", std::any::type_name::<T>());
        }
    }

    fn sse(chunks: &[Value]) -> String {
        let mut s: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        s.push_str("data: [DONE]\n\n");
        s
    }

    fn fake_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let b2 = bodies.clone();
        std::thread::spawn(move || {
            for (i, stream) in l.incoming().enumerate() {
                let Ok(mut s) = stream else { continue };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let n = s.read(&mut tmp).unwrap_or(0);
                    if n == 0 { break; }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                        let cl = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                        while buf.len() < p + 4 + cl {
                            let n = s.read(&mut tmp).unwrap_or(0);
                            if n == 0 { break; }
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        b2.lock().unwrap().push(String::from_utf8_lossy(&buf[p + 4..]).to_string());
                        break;
                    }
                }
                let body = responses.get(i).cloned().unwrap_or_else(|| sse(&[json!({"choices":[{"delta":{"content":"(end)"},"finish_reason":"stop"}]})]));
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = s.flush();
            }
        });
        (format!("http://127.0.0.1:{port}/v1"), bodies)
    }

    fn harness(home: &Path, base: &str) -> (Arc<Shared>, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let out: Out = Box::new(move |l: &str| { let _ = tx.lock().unwrap().send(serde_json::from_str(l).unwrap()); });
        let cfg = Config {
            base_url: base.into(), api_key: "k".into(), model: "m1".into(), models: vec!["m2".into()],
            max_steps: 10, context_chars: 360_000, temperature: None, home: home.to_path_buf(),
        };
        (Arc::new(Shared::new(cfg, out)), rx)
    }

    fn req(sh: &Arc<Shared>, id: u64, method: &str, params: Value) {
        handle_line(sh, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string());
    }

    /// 等待满足条件的消息；期间的其他消息收进 seen
    fn wait(rx: &mpsc::Receiver<Value>, seen: &mut Vec<Value>, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            let v = rx.recv_timeout(Duration::from_secs(20)).expect("timeout waiting for message");
            if pred(&v) { return v; }
            seen.push(v);
        }
    }

    fn resp_of(id: u64) -> impl Fn(&Value) -> bool {
        move |v: &Value| v.get("method").is_none() && v["id"] == json!(id)
    }

    #[test]
    fn end_to_end_tool_loop_permissions_and_sessions() {
        let dir = test_dir();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let home = dir.join("home");
        let step1 = sse(&[
            json!({"choices":[{"delta":{"reasoning_content":"想一想"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"sub/a.txt\","}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"content\":\"hello\\nworld\\n\"}"}}]},"finish_reason":"tool_calls"}]}),
        ]);
        let step2 = sse(&[json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_b","function":{"name":"edit_file","arguments":"{\"path\":\"sub/a.txt\",\"old_string\":\"world\",\"new_string\":\"there\"}"}}]},"finish_reason":"tool_calls"}]})]);
        let step3 = sse(&[
            json!({"choices":[{"delta":{"content":"完成"}}]}),
            json!({"choices":[{"delta":{"content":"了"},"finish_reason":"stop"}]}),
        ]);
        let (base, bodies) = fake_server(vec![step1, step2, step3]);
        let (sh, rx) = harness(&home, &base);
        let mut seen = Vec::new();

        req(&sh, 1, "initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }));
        let r = wait(&rx, &mut seen, resp_of(1));
        check::<protocol::InitializeResponse>(&r["result"]);

        req(&sh, 2, "session/new", json!({ "cwd": work.to_string_lossy(), "mcpServers": [] }));
        let r = wait(&rx, &mut seen, resp_of(2));
        check::<protocol::NewSessionResponse>(&r["result"]);
        let sid = r["result"]["sessionId"].as_str().unwrap().to_string();

        req(&sh, 3, "session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "创建并修改文件" }] }));
        let perm = wait(&rx, &mut seen, |v| v["method"] == "session/request_permission");
        check::<protocol::RequestPermissionRequest>(&perm["params"]);
        handle_line(&sh, &json!({ "jsonrpc": "2.0", "id": perm["id"], "result": { "outcome": { "outcome": "selected", "optionId": "allow_always" } } }).to_string());
        let r = wait(&rx, &mut seen, resp_of(3));
        check::<protocol::PromptResponse>(&r["result"]);
        assert_eq!(r["result"]["stopReason"], "end_turn", "{r}");
        assert!(!seen.iter().any(|v| v["method"] == "session/request_permission"), "allow_always should cover edit_file");
        for n in seen.iter().filter(|v| v["method"] == "session/update") { check::<protocol::SessionNotification>(&n["params"]); }
        let texts: String = seen.iter().filter(|v| v["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
            .map(|v| v["params"]["update"]["content"]["text"].as_str().unwrap().to_string()).collect();
        assert_eq!(texts, "完成了");
        assert!(seen.iter().any(|v| v["params"]["update"]["sessionUpdate"] == "agent_thought_chunk"));
        assert!(seen.iter().any(|v| v["params"]["update"]["content"][0]["type"] == "diff"));
        assert_eq!(std::fs::read_to_string(work.join("sub").join("a.txt")).unwrap(), "hello\nthere\n");

        let b = bodies.lock().unwrap().clone();
        assert_eq!(b.len(), 3);
        let b2: Value = serde_json::from_str(&b[1]).unwrap();
        let msgs = b2["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert!(msgs.iter().any(|m| m["reasoning_content"] == "想一想"));
        assert!(msgs.iter().any(|m| m["role"] == "tool" && m["tool_call_id"] == "call_a"));
        assert!(!b[1].contains("\"_status\""), "internal keys must be stripped");

        req(&sh, 4, "session/list", json!({ "cwd": work.to_string_lossy() }));
        let r = wait(&rx, &mut seen, resp_of(4));
        check::<protocol::ListSessionsResponse>(&r["result"]);
        assert_eq!(r["result"]["sessions"][0]["title"], "创建并修改文件");

        req(&sh, 5, "session/set_config_option", json!({ "sessionId": sid, "configId": "model", "value": "m2" }));
        let r = wait(&rx, &mut seen, resp_of(5));
        check::<protocol::SetSessionConfigOptionResponse>(&r["result"]);
        req(&sh, 6, "session/set_mode", json!({ "sessionId": sid, "modeId": "read" }));
        let r = wait(&rx, &mut seen, resp_of(6));
        assert!(r.get("error").is_none(), "{r}");

        // 模拟进程重启：新实例从磁盘加载并重放
        let (sh2, rx2) = harness(&home, &base);
        let mut seen2 = Vec::new();
        req(&sh2, 7, "session/load", json!({ "sessionId": sid, "cwd": work.to_string_lossy(), "mcpServers": [] }));
        let r = wait(&rx2, &mut seen2, resp_of(7));
        check::<protocol::LoadSessionResponse>(&r["result"]);
        assert_eq!(r["result"]["modes"]["currentModeId"], "read");
        assert_eq!(r["result"]["configOptions"][0]["currentValue"], "m2");
        for n in &seen2 { check::<protocol::SessionNotification>(&n["params"]); }
        let kinds: Vec<&str> = seen2.iter().filter_map(|v| v["params"]["update"]["sessionUpdate"].as_str()).collect();
        assert_eq!(kinds, vec!["user_message_chunk", "tool_call", "tool_call", "agent_message_chunk"]);

        req(&sh2, 8, "session/bogus", json!({}));
        let r = wait(&rx2, &mut seen2, resp_of(8));
        assert_eq!(r["error"]["code"], -32601);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reject_and_cancel() {
        let dir = test_dir();
        let home = dir.join("home");
        let call = |id: &str| sse(&[json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":id,"function":{"name":"delete_path","arguments":"{\"path\":\"x.txt\"}"}}]},"finish_reason":"tool_calls"}]})]);
        let (base, _bodies) = fake_server(vec![call("c1"), sse(&[json!({"choices":[{"delta":{"content":"好的"},"finish_reason":"stop"}]})]), call("c2")]);
        std::fs::write(dir.join("x.txt"), "keep").unwrap();
        let (sh, rx) = harness(&home, &base);
        let mut seen = Vec::new();
        req(&sh, 1, "session/new", json!({ "cwd": dir.to_string_lossy(), "mcpServers": [] }));
        let sid = wait(&rx, &mut seen, resp_of(1))["result"]["sessionId"].as_str().unwrap().to_string();
        req(&sh, 2, "session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "删掉" }] }));
        let perm = wait(&rx, &mut seen, |v| v["method"] == "session/request_permission");
        handle_line(&sh, &json!({ "jsonrpc": "2.0", "id": perm["id"], "result": { "outcome": { "outcome": "selected", "optionId": "reject_once" } } }).to_string());
        let r = wait(&rx, &mut seen, resp_of(2));
        assert_eq!(r["result"]["stopReason"], "end_turn");
        assert!(dir.join("x.txt").exists());
        // 第二轮：权限等待中取消
        req(&sh, 3, "session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "再删" }] }));
        let perm = wait(&rx, &mut seen, |v| v["method"] == "session/request_permission");
        handle_line(&sh, &json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": sid } }).to_string());
        handle_line(&sh, &json!({ "jsonrpc": "2.0", "id": perm["id"], "result": { "outcome": { "outcome": "cancelled" } } }).to_string());
        let r = wait(&rx, &mut seen, resp_of(3));
        assert_eq!(r["result"]["stopReason"], "cancelled", "{r}");
        assert!(dir.join("x.txt").exists());
        let d = load_session(&sh.cfg, &sid).unwrap();
        // 每个 tool_call 都有对应的 tool 结果，历史可继续发送给模型
        let ids: Vec<&str> = d.messages.iter().flat_map(|m| m["tool_calls"].as_array().into_iter().flatten()).filter_map(|t| t["id"].as_str()).collect();
        for id in ids { assert!(d.messages.iter().any(|m| m["role"] == "tool" && m["tool_call_id"] == id), "missing tool result {id}"); }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_configured_is_reported() {
        let dir = test_dir();
        let (sh, rx) = harness(&dir, "");
        let mut seen = Vec::new();
        req(&sh, 1, "session/new", json!({ "cwd": dir.to_string_lossy(), "mcpServers": [] }));
        let r = wait(&rx, &mut seen, resp_of(1));
        assert!(r["error"]["message"].as_str().unwrap().contains("设置 → Agent 管理"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_file_rules() {
        let dir = test_dir();
        let cfg = Cfg { root: std::fs::canonicalize(&dir).unwrap(), token: String::new(), auth_user: String::new(), auth_pass: String::new(), exec_enabled: false };
        std::fs::write(dir.join("a.txt"), "x = 1\nx = 1\n").unwrap();
        let e = edit_file(&cfg, "a.txt", &json!({ "old_string": "x = 1", "new_string": "x = 2" })).unwrap_err();
        assert!(e.contains("2 次"), "{e}");
        edit_file(&cfg, "a.txt", &json!({ "old_string": "x = 1", "new_string": "x = 2", "replace_all": true })).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "x = 2\nx = 2\n");
        // BOM + CRLF：LF 片段可匹配，写回保持 BOM 与 CRLF
        std::fs::write(dir.join("b.txt"), b"\xEF\xBB\xBFline1\r\nline2\r\n").unwrap();
        edit_file(&cfg, "b.txt", &json!({ "old_string": "line1\nline2", "new_string": "L1\nL2" })).unwrap();
        assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"\xEF\xBB\xBFL1\r\nL2\r\n");
        assert!(edit_file(&cfg, "missing.txt", &json!({ "old_string": "a", "new_string": "b" })).is_err());
        assert!(edit_file(&cfg, "a.txt", &json!({ "old_string": "nope", "new_string": "b" })).unwrap_err().contains("未找到"));
        // 越界与 .. 被拒绝；写入自动建父目录
        assert!(exec_builtin(&cfg, "write_file", &json!({ "path": "../evil.txt", "content": "x" })).is_err());
        exec_builtin(&cfg, "write_file", &json!({ "path": "d1/d2/c.txt", "content": "ok" })).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("d1").join("d2").join("c.txt")).unwrap(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn budget_drops_oldest_turns_whole() {
        let big = "x".repeat(5000);
        let mut h = Vec::new();
        for t in 0..3 {
            h.push(json!({ "role": "user", "content": format!("q{t}") }));
            h.push(json!({ "role": "assistant", "content": null, "tool_calls": [{ "id": format!("c{t}"), "type": "function", "function": { "name": "read_file", "arguments": "{}" } }] }));
            h.push(json!({ "role": "tool", "tool_call_id": format!("c{t}"), "content": big }));
            h.push(json!({ "role": "assistant", "content": "ok" }));
        }
        let (m, dropped) = fit_budget(h.clone(), 8, 1_000_000);
        assert!(!dropped);
        assert_eq!(m.len(), 12);
        // 省略早期工具输出即可满足
        let (m, dropped) = fit_budget(h.clone(), 8, 7000);
        assert!(!dropped);
        assert_eq!(m.len(), 12);
        assert!(m[2]["content"].as_str().unwrap().contains("已省略"));
        assert_eq!(m[10]["content"].as_str().unwrap().len(), 5000);
        // 需要丢整轮：从 user 开始，当前轮完整
        let (m, dropped) = fit_budget(h, 8, 5600);
        assert!(dropped);
        assert_eq!(m[0]["role"], "user");
        assert_eq!(m.last().unwrap()["content"], "ok");
        assert!(m.iter().any(|x| x["content"] == "q2"));
    }

    #[test]
    fn api_messages_strip_internal_and_old_reasoning() {
        let h = vec![
            json!({ "role": "user", "content": "a", "_display": "a" }),
            json!({ "role": "assistant", "content": "b", "_reasoning": "old" }),
            json!({ "role": "user", "content": "c" }),
            json!({ "role": "assistant", "content": "d", "_reasoning": "new" }),
        ];
        let (m, start) = api_messages(&h, 2);
        assert_eq!(start, 2);
        assert!(m[0].get("_display").is_none());
        assert!(m[1].get("reasoning_content").is_none());
        assert_eq!(m[3]["reasoning_content"], "new");
        assert_eq!(mcp_api_name("shidrive", "context.get"), "mcp__shidrive__context_get");
    }

    #[test]
    fn api_messages_repair_tool_history() {
        let call = |id: &str, args: &str| json!({ "id": id, "type": "function", "function": { "name": "read_file", "arguments": args },
            "extra_content": { "google": { "thought_signature": "sig" } } });
        let h = vec![
            json!({ "role": "user", "content": "a" }),
            // 旧轮的工具调用：思考需回传；参数坏掉；c2 缺结果
            json!({ "role": "assistant", "content": null, "_reasoning": "r1", "tool_calls": [call("c1", "{bad"), call("c2", "{}")] }),
            json!({ "role": "tool", "tool_call_id": "c1", "content": "x", "_status": "failed" }),
            json!({ "role": "tool", "tool_call_id": "zz", "content": "orphan" }),
            json!({ "role": "user", "content": "b" }),
        ];
        let (m, start) = api_messages(&h, 4);
        assert_eq!(m.len(), 5);
        assert_eq!(m[1]["reasoning_content"], "r1");
        assert_eq!(m[1]["tool_calls"][0]["function"]["arguments"], "{}");
        assert_eq!(m[1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"], "sig");
        assert_eq!(m[2]["tool_call_id"], "c1");
        assert_eq!(m[3]["tool_call_id"], "c2");
        assert_eq!(m[4]["content"], "b");
        assert_eq!(start, 4);
        let f = fix_args(ToolCall { id: "i".into(), name: "n".into(), args: "{\"a\":1}{\"a\":1}".into(), last: "{\"a\":1}".into(), extra: Default::default() });
        assert_eq!(f.args, "{\"a\":1}");
    }
}
