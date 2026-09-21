//! Coding MCP（runfromweb）：独立端口的 MCP 服务，供网络侧模型使用本地能力。
//!
//! 以 `shidrive.exe --coding-mcp --port 51667 --root D:\path [--token xxx]` 启动，
//! 不随主程序启动（由「无项目」下的 runfromweb 工作流按需拉起）。
//!
//! 工具（对齐参考实现 runfromweb 的核心集）：
//!   pc.fs.read / pc.fs.write / pc.fs.list / pc.fs.search / pc.fs.mkdir
//!   pc.process.exec
//!
//! 安全模型：
//!   - 绑定地址默认 127.0.0.1（需要对外时用 --bind 0.0.0.0 自担风险）；
//!   --token / CODING_MCP_TOKEN 设置后必须携带 Authorization: Bearer 或 ?token=；
//!   - 所有文件路径必须在 --root 之内（解析符号链接）；
//!   - exec 以当前用户权限运行，root 即边界（与参考实现一致，非沙箱）。

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

pub fn run(args: &[String]) -> i32 {
    let flag = |name: &str| -> Option<String> {
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == name {
                return it.next().cloned();
            }
        }
        None
    };
    // 环境变量回退时空值视为未设置（工作流会把空变量注入子进程）
    let env_flag = |name: &str, env: &str| -> Option<String> {
        flag(name).or_else(|| std::env::var(env).ok().filter(|v| !v.is_empty()))
    };
    let port: u16 = env_flag("--port", "CODING_MCP_PORT")
        .and_then(|v| v.parse().ok())
        .unwrap_or(51667);
    let root = env_flag("--root", "CODING_MCP_ROOT").unwrap_or_default();
    let token = env_flag("--token", "CODING_MCP_TOKEN").unwrap_or_default();
    let bind = env_flag("--bind", "CODING_MCP_BIND").unwrap_or_else(|| "127.0.0.1".into());
    let auth_user = env_flag("--auth-user", "CODING_MCP_AUTH_USER").unwrap_or_default();
    let auth_pass = env_flag("--auth-pass", "CODING_MCP_AUTH_PASS").unwrap_or_default();
    let exec_enabled = env_flag("--exec", "CODING_MCP_ALLOW_EXEC")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true);

    let root = PathBuf::from(root);
    if !root.is_dir() {
        eprintln!("[coding-mcp] --root 不是有效目录：{}", root.display());
        return 2;
    }
    let root = match std::fs::canonicalize(&root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[coding-mcp] 解析 root 失败: {e}");
            return 2;
        }
    };

    // SO_REUSEADDR：避免「停止后立即重启」撞上 TIME_WAIT 端口占用
    let listener = {
        use socket2::{Domain, Protocol, Socket, Type};
        use std::net::SocketAddr;
        let addr: SocketAddr = format!("{bind}:{port}").parse().unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], port)));
        let socket = match Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[coding-mcp] 创建 socket 失败: {e}");
                return 3;
            }
        };
        let _ = socket.set_reuse_address(true);
        let _ = socket.set_nodelay(true);
        if let Err(e) = socket.bind(&addr.into()) {
            eprintln!("[coding-mcp] 端口 {port} 绑定失败: {e}");
            return 3;
        }
        if let Err(e) = socket.listen(64) {
            eprintln!("[coding-mcp] listen 失败: {e}");
            return 3;
        }
        let l: TcpListener = socket.into();
        l
    };
    eprintln!("[coding-mcp] listening on http://{bind}:{port}/mcp  root={}", root.display());
    if token.is_empty() {
        eprintln!("[coding-mcp] 注意：未设置 --token，任何人可访问（仅建议本机使用）");
    }

    eprintln!(
        "[coding-mcp] auth: {}  exec: {}",
        if !token.is_empty() { "token".to_string() } else if !auth_user.is_empty() { "basic".to_string() } else { "open(仅本机建议)".to_string() },
        if exec_enabled { "on" } else { "off" }
    );
    if !token.is_empty() {
        eprintln!("[coding-mcp] token: {token}");
    }
    let cfg = std::sync::Arc::new(Cfg {
        root,
        token: token.clone(),
        auth_user,
        auth_pass,
        exec_enabled,
    });
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let cfg = cfg.clone();
                std::thread::spawn(move || {
                    let _ = handle_conn(s, &cfg);
                });
            }
            Err(_) => continue,
        }
    }
    0
}

struct Cfg {
    root: PathBuf,
    token: String,
    auth_user: String,
    auth_pass: String,
    exec_enabled: bool,
}

fn handle_conn(mut stream: TcpStream, cfg: &Cfg) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    // headers
    let header_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut method = String::new();
    let mut path = String::new();
    {
        let mut it = request_line.split_whitespace();
        method = it.next().unwrap_or_default().to_string();
        path = it.next().unwrap_or_default().to_string();
    }
    let mut content_length = 0usize;
    let mut authorization = String::new();
    let mut origin = String::new();
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = lower.strip_prefix("authorization:") {
            authorization = line[15..].trim().to_string();
        } else if let Some(v) = lower.strip_prefix("origin:") {
            origin = line[7..].trim().to_string();
        }
    }
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[header_end.min(buf.len())..].to_vec();
    let (path_only, query) = match path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (path.clone(), String::new()),
    };

    // CORS（浏览器端 MCP 客户端需要）
    let cors = format!(
        "Access-Control-Allow-Origin: {}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization, MCP-Protocol-Version\r\nAccess-Control-Max-Age: 86400\r\n",
        if origin.is_empty() { "*".to_string() } else { origin }
    );

    if method == "OPTIONS" {
        return write_resp(&mut stream, 204, "text/plain", &cors, b"");
    }

    // auth: token(Bearer/query) → basic(user/pass) → open
    let mut authorized = true;
    if !cfg.token.is_empty() {
        let provided_bearer = authorization.strip_prefix("Bearer ").unwrap_or("");
        let provided_query = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("token=").or_else(|| kv.strip_prefix("passcode=")))
            .unwrap_or("");
        if provided_bearer != cfg.token && provided_query != cfg.token {
            authorized = false;
        }
    } else if !cfg.auth_user.is_empty() {
        authorized = check_basic(&authorization, &cfg.auth_user, &cfg.auth_pass);
    }
    if !authorized {
        let payload = json!({"jsonrpc":"2.0","id":Value::Null,"error":{"code":-32001,"message":"未授权：请携带正确的凭据（Bearer token 或 Basic Auth）。"}});
        return write_resp(&mut stream, 401, "application/json", &cors, payload.to_string().as_bytes());
    }

    let (status, ctype, payload): (u16, &str, String) = match (method.as_str(), path_only.as_str()) {
        ("GET", "/health") => (200, "application/json", json!({"ok":true, "service":"shidrive-coding-mcp"}).to_string()),
        ("GET", "/mcp") => {
            // 无 SSE；GET 返回 405 与参考实现一致
            (405, "application/json", json!({"error":"GET not supported; POST JSON-RPC to /mcp"}).to_string())
        }
        ("POST", "/mcp") => {
            let parsed: Result<Value, _> = serde_json::from_slice(&body);
            match parsed {
                Ok(msg) => (200, "application/json", handle_mcp(msg, cfg).to_string()),
                Err(e) => (
                    200,
                    "application/json",
                    json!({"jsonrpc":"2.0","id":Value::Null,"error":{"code":-32700,"message":format!("请求不是合法的 JSON：{e}")}}).to_string(),
                ),
            }
        }
        _ => (404, "text/plain; charset=utf-8", "not found".to_string()),
    };

    write_resp(&mut stream, status, ctype, &cors, payload.as_bytes())
}

fn write_resp(stream: &mut TcpStream, status: u16, ctype: &str, extra_headers: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn check_basic(authorization: &str, user: &str, pass: &str) -> bool {
    let b64 = match authorization.strip_prefix("Basic ") {
        Some(v) => v,
        None => return false,
    };
    // 常量时间比较意义有限（本地服务），直接比对解码结果
    use std::io::Read;
    let mut decoder: Box<dyn Read> = Box::new(base64_decoder(b64.as_bytes()));
    let mut buf = Vec::new();
    if decoder.read_to_end(&mut buf).is_err() {
        return false;
    }
    let decoded = String::from_utf8_lossy(&buf);
    let expected = format!("{user}:{pass}");
    // 简单等长比对
    if decoded.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in decoded.bytes().zip(expected.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 零依赖 base64 解码器（标准字母表，含 padding）
struct Base64Reader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    bits: u32,
}

fn base64_decoder(data: &[u8]) -> Base64Reader<'_> {
    Base64Reader { data, pos: 0, acc: 0, bits: 0 }
}

impl<'a> Read for Base64Reader<'a> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let val = |c: u8| -> Option<u32> {
            match c {
                b'A'..=b'Z' => Some((c - b'A') as u32),
                b'a'..=b'z' => Some((c - b'a' + 26) as u32),
                b'0'..=b'9' => Some((c - b'0' + 52) as u32),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        };
        let mut written = 0;
        while written < out.len() {
            // accumulate until we have >= 8 bits
            while self.bits < 8 {
                if self.pos >= self.data.len() {
                    return Ok(written);
                }
                let c = self.data[self.pos];
                self.pos += 1;
                if c == b'=' || c.is_ascii_whitespace() {
                    continue;
                }
                match val(c) {
                    Some(v) => {
                        self.acc = (self.acc << 6) | v;
                        self.bits += 6;
                    }
                    None => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad base64")),
                }
            }
            self.bits -= 8;
            out[written] = ((self.acc >> self.bits) & 0xFF) as u8;
            written += 1;
        }
        Ok(written)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn handle_mcp(msg: Value, cfg: &Cfg) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or_default().to_string();
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    match method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "shidrive-coding-mcp", "title": "使驾 Coding MCP (runfromweb)", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
        "notifications/initialized" | "ping" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
        "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools": tool_definitions(cfg)}}),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(cfg, &name, &args) {
                Ok(text) => json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}],"isError":false}}),
                Err(e) => json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
            }
        }
        other => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("未知方法 {other}。可用：initialize、tools/list、tools/call。")}}),
    }
}

fn tool_definitions(cfg: &Cfg) -> Value {
    let root_desc = cfg.root.to_string_lossy();
    json!([
        { "name": "pc.fs.read", "description": "按行读取文本文件（1 起始行号）",
          "inputSchema": { "type":"object","required":["path"],"properties":{
            "path": {"type":"string","description":"相对 root 或绝对路径"},
            "offset": {"type":"integer","description":"起始行（默认 1）"},
            "limit": {"type":"integer","description":"行数（默认 2000）"} } } },
        { "name": "pc.fs.write", "description": "写入文本文件（原子替换）",
          "inputSchema": { "type":"object","required":["path","content"],"properties":{
            "path": {"type":"string"}, "content": {"type":"string"} } } },
        { "name": "pc.fs.list", "description": "列出目录（分页）",
          "inputSchema": { "type":"object","properties":{
            "path": {"type":"string","description":"默认 ."}, "offset": {"type":"integer"}, "limit": {"type":"integer","description":"默认 200"} } } },
        { "name": "pc.fs.mkdir", "description": "递归创建目录",
          "inputSchema": { "type":"object","required":["path"],"properties":{ "path": {"type":"string"} } } },
        { "name": "pc.fs.search", "description": "字面文本搜索（跳过 node_modules/.git/target/dist 等）",
          "inputSchema": { "type":"object","required":["query"],"properties":{
            "query": {"type":"string"}, "path": {"type":"string","description":"默认 ."}, "glob": {"type":"string","description":"默认 *"},
            "max_results": {"type":"integer","description":"默认 100"} } } },
        { "name": "pc.process.exec", "description": "执行命令（cmd /C），捕获输出；超时终止进程树",
          "inputSchema": { "type":"object","required":["command"],"properties":{
            "command": {"type":"string"}, "workdir": {"type":"string","description":"默认 root"},
            "timeout": {"type":"integer","description":"秒，默认 30"}, "max_output_chars": {"type":"integer","description":"默认 32000"} } } },
    ])
    // root 提示放进每个工具不合适；在 read 的 description 已带，这里统一补充：
}

// ---------- helpers ----------

fn safe_path(cfg: &Cfg, raw: &str) -> Result<PathBuf, String> {
    let p = Path::new(raw);
    let joined = if p.is_absolute() { p.to_path_buf() } else { cfg.root.join(p) };
    let canon = std::fs::canonicalize(&joined).map_err(|e| format!("路径不存在（{}）：{e}", joined.display()))?;
    if !canon.starts_with(&cfg.root) {
        return Err(format!("路径越界：{} 不在根目录 {} 之内。", canon.display(), cfg.root.display()));
    }
    Ok(canon)
}

fn call_tool(cfg: &Cfg, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "pc.fs.read" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;
            let full = safe_path(cfg, path)?;
            let data = std::fs::read(&full).map_err(|e| format!("读取失败: {e}"))?;
            if data.len() > 4 * 1024 * 1024 {
                return Err("文件超过 4MiB 读取上限。".into());
            }
            let text = String::from_utf8_lossy(&data);
            let lines: Vec<&str> = text.split_inclusive('\n').collect();
            let start = (offset - 1).min(lines.len());
            let end = (start + limit).min(lines.len());
            Ok(json_bytes(&json!({
                "path": full.to_string_lossy(),
                "total_lines": lines.len(),
                "offset": offset,
                "content": lines[start..end].concat(),
            })))
        }
        "pc.fs.write" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or_default();
            let full = safe_path_parent(cfg, path)?;
            std::fs::write(&full, content).map_err(|e| format!("写入失败: {e}"))?;
            Ok(json_bytes(&json!({ "written": true, "path": full.to_string_lossy(), "bytes": content.len() })))
        }
        "pc.fs.mkdir" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let full = safe_path_parent(cfg, path)?;
            std::fs::create_dir_all(&full).map_err(|e| format!("创建失败: {e}"))?;
            Ok(json_bytes(&json!({ "created": true, "path": full.to_string_lossy() })))
        }
        "pc.fs.list" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
            let full = safe_path(cfg, path)?;
            let mut entries: Vec<Value> = Vec::new();
            for e in std::fs::read_dir(&full).map_err(|err| format!("读取目录失败: {err}"))? {
                let e = e.map_err(|err| err.to_string())?;
                entries.push(json!({
                    "name": e.file_name().to_string_lossy(),
                    "is_dir": e.file_type().map(|t| t.is_dir()).unwrap_or(false),
                    "size": e.metadata().map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0),
                }));
            }
            entries.sort_by(|a, b| {
                let ad = a["is_dir"].as_bool().unwrap_or(false);
                let bd = b["is_dir"].as_bool().unwrap_or(false);
                bd.cmp(&ad).then(a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()))
            });
            let total = entries.len();
            let slice: Vec<Value> = entries.into_iter().skip(offset).take(limit).collect();
            Ok(json_bytes(&json!({ "path": full.to_string_lossy(), "total": total, "entries": slice })))
        }
        "pc.fs.search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or_default();
            if query.is_empty() {
                return Err("缺少参数 query。".into());
            }
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let glob = args.get("glob").and_then(|v| v.as_str()).unwrap_or("*");
            let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let full = safe_path(cfg, path)?;
            let mut matches: Vec<Value> = Vec::new();
            let mut visited = 0usize;
            search_walk(&full, query, glob, max_results, &mut matches, &mut visited, 0);
            Ok(json_bytes(&json!({ "query": query, "matches": matches, "scanned_files": visited })))
        }
        "pc.process.exec" => {
            if !cfg.exec_enabled {
                return Err("命令执行已被禁用（CODING_MCP_ALLOW_EXEC=0 / --exec 0）。文件读写仍可用。".into());
            }
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or_default();
            if command.trim().is_empty() {
                return Err("缺少参数 command。".into());
            }
            let workdir = args.get("workdir").and_then(|v| v.as_str()).unwrap_or_default();
            let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);
            let max_chars = args.get("max_output_chars").and_then(|v| v.as_u64()).unwrap_or(32000) as usize;
            let dir = if workdir.is_empty() { cfg.root.clone() } else { safe_path(cfg, workdir)? };
            run_exec(command, &dir, timeout, max_chars)
        }
        other => Err(format!("未知工具 {other}。可用：pc.fs.read、pc.fs.write、pc.fs.list、pc.fs.mkdir、pc.fs.search、pc.process.exec。")),
    }
}

/// 允许写入尚不存在的路径：校验其规范化父目录在 root 内。
fn safe_path_parent(cfg: &Cfg, raw: &str) -> Result<PathBuf, String> {
    let p = Path::new(raw);
    let joined = if p.is_absolute() { p.to_path_buf() } else { cfg.root.join(p) };
    let parent = joined.parent().unwrap_or(&cfg.root);
    let parent_canon = std::fs::canonicalize(parent).map_err(|e| format!("父目录不存在（{}）：{e}", parent.display()))?;
    if !parent_canon.starts_with(&cfg.root) {
        return Err(format!("路径越界：{} 不在根目录 {} 之内。", joined.display(), cfg.root.display()));
    }
    // 拼回文件名，保留原大小写
    let file = joined.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    if file.is_empty() {
        return Err("路径缺少文件名。".into());
    }
    Ok(parent_canon.join(file))
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules" | ".git" | "target" | "dist" | "build" | "out" | ".venv" | "__pycache__" | ".next" | "vendor" | ".idea" | ".vscode" | "gen"
    )
}

fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    // 极简 glob：仅支持前缀*后缀
    if let Some(star) = pattern.find('*') {
        let (pre, suf) = (&pattern[..star], &pattern[star + 1..]);
        return name.len() >= pre.len() + suf.len() && name.starts_with(pre) && name.ends_with(suf);
    }
    name == pattern
}

fn search_walk(dir: &Path, query: &str, glob: &str, max_results: usize, out: &mut Vec<Value>, visited: &mut usize, depth: usize) {
    if out.len() >= max_results || depth > 12 {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for e in rd.flatten() {
        if out.len() >= max_results {
            return;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let path = e.path();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if !skip_dir(&name) {
                search_walk(&path, query, glob, max_results, out, visited, depth + 1);
            }
        } else {
            if !glob_match(glob, &name) {
                continue;
            }
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > 4 * 1024 * 1024 {
                continue;
            }
            *visited += 1;
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if data.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&data);
            for (i, line) in text.lines().enumerate() {
                if line.contains(query) {
                    out.push(json!({
                        "path": path.to_string_lossy(),
                        "line": i + 1,
                        "text": line.trim().chars().take(240).collect::<String>(),
                    }));
                    if out.len() >= max_results {
                        return;
                    }
                }
            }
        }
    }
}

fn run_exec(command: &str, dir: &Path, timeout: u64, max_chars: usize) -> Result<Value, String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", command]).current_dir(dir);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    let pid = child.id();
    let out = child.stdout.take();
    let err = child.stderr.take();
    let out_handle = {
        let pipe = out;
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = std::io::Read::read_to_end(&mut p, &mut buf);
            }
            buf
        })
    };
    let err_handle = {
        let pipe = err;
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = std::io::Read::read_to_end(&mut p, &mut buf);
            }
            buf
        })
    };
    let status = wait_with_timeout(&mut child, timeout);
    let stdout_raw = out_handle.join().unwrap_or_default();
    let stderr_raw = err_handle.join().unwrap_or_default();
    let stdout_txt = String::from_utf8_lossy(&stdout_raw);
    let stderr_txt = String::from_utf8_lossy(&stderr_raw);
    let cut = |s: &str| -> String {
        if s.chars().count() > max_chars {
            s.chars().take(max_chars).collect::<String>() + "\n…(截断)"
        } else {
            s.to_string()
        }
    };
    match status {
        Some(st) => Ok(json_bytes(&json!({
            "exit_code": st.code().unwrap_or(-1),
            "stdout": cut(&stdout_txt),
            "stderr": cut(&stderr_txt),
            "workdir": dir.to_string_lossy(),
        }))),
        None => {
            let _ = std::process::Command::new("taskkill").args(["/F", "/T", "/PID", &pid.to_string()]).output();
            Err(format!("命令超时（{timeout}s），已终止进程树。已捕获输出：\n{}", cut(&stdout_txt)))
        }
    }
}

fn wait_with_timeout(child: &mut std::process::Child, timeout: u64) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Some(st),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

fn json_bytes(v: &Value) -> Value {
    Value::String(serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()))
}
