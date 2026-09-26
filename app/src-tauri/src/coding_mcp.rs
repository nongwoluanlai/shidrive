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
    // FIX-05：默认关闭命令执行；确需开启时显式传 --exec 1 / CODING_MCP_ALLOW_EXEC=1
    let exec_enabled = env_flag("--exec", "CODING_MCP_ALLOW_EXEC")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);

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

pub(crate) struct Cfg {
    pub(crate) root: PathBuf,
    pub(crate) token: String,
    pub(crate) auth_user: String,
    pub(crate) auth_pass: String,
    pub(crate) exec_enabled: bool,
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
                Ok(v) => {
                    // call_tool 返回结构化 JSON，这里做唯一一次序列化。
                    // FIX：此前 json_bytes 先转成字符串、外层再转义一次，
                    // 客户端看到的是 JSON-in-JSON 多层转义。
                    let text = serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string());
                    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}],"isError":false}})
                }
                Err(e) => json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}}),
            }
        }
        other => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("未知方法 {other}。可用：initialize、tools/list、tools/call。")}}),
    }
}

fn tool_definitions(cfg: &Cfg) -> Value {
    let root_desc = cfg.root.to_string_lossy();
    json!([
        { "name": "pc.fs.read", "description": format!("按行读取文本文件（1 起始行号）。root：{root_desc}。自动处理 UTF-8/UTF-16 BOM"),
          "inputSchema": { "type":"object","required":["path"],"properties":{
            "path": {"type":"string","description":"相对 root 或绝对路径"},
            "offset": {"type":"integer","description":"起始行（默认 1）"},
            "limit": {"type":"integer","description":"行数（默认 2000）"} } } },
        { "name": "pc.fs.write", "description": "写入文件（原子替换）。encoding=text（默认，UTF-8 文本）或 base64（二进制资源如 PNG 直接解码写入）",
          "inputSchema": { "type":"object","required":["path","content"],"properties":{
            "path": {"type":"string"},
            "content": {"type":"string","description":"文本内容，或 encoding=base64 时的 base64 字符串"},
            "encoding": {"type":"string","enum":["text","base64"],"description":"默认 text"} } } },
        { "name": "pc.fs.writeBatch", "description": "批量写入多个文件（单次最多 50），减少往返",
          "inputSchema": { "type":"object","required":["items"],"properties":{
            "items": {"type":"array","maxItems":50,"items":{"type":"object","properties":{
                "path":{"type":"string"},"content":{"type":"string"},"encoding":{"type":"string","enum":["text","base64"]} },
                "required":["path","content"]} } } } },
        { "name": "pc.fs.list", "description": "列出目录（分页，含 size 与 mtime epoch 秒）",
          "inputSchema": { "type":"object","properties":{
            "path": {"type":"string","description":"默认 ."}, "offset": {"type":"integer"}, "limit": {"type":"integer","description":"默认 200"} } } },
        { "name": "pc.fs.mkdir", "description": "递归创建目录",
          "inputSchema": { "type":"object","required":["path"],"properties":{ "path": {"type":"string"} } } },
        { "name": "pc.fs.search", "description": "字面文本搜索（跳过 node_modules/.git/target/dist 等）。path 可为目录或单个文件；自动解码 UTF-8/UTF-16 BOM；跳过原因见返回的 skipped 数组",
          "inputSchema": { "type":"object","required":["query"],"properties":{
            "query": {"type":"string"}, "path": {"type":"string","description":"目录或文件，默认 ."}, "glob": {"type":"string","description":"文件名通配，默认 *"},
            "ignore_case": {"type":"boolean","description":"默认 false"},
            "max_results": {"type":"integer","description":"默认 100"} } } },
        { "name": "pc.fs.exists", "description": "检查文件/目录是否存在（返回 exists/is_dir/size）",
          "inputSchema": { "type":"object","required":["path"],"properties":{ "path": {"type":"string"} } } },
        { "name": "pc.fs.move", "description": "移动/重命名（同盘符；dst 为已存在目录时移入其中）",
          "inputSchema": { "type":"object","required":["src","dst"],"properties":{
            "src": {"type":"string"}, "dst": {"type":"string"} } } },
        { "name": "pc.fs.copy", "description": "复制文件或目录（目录递归；符号链接跳过）",
          "inputSchema": { "type":"object","required":["src","dst"],"properties":{
            "src": {"type":"string"}, "dst": {"type":"string"} } } },
        { "name": "pc.fs.delete", "description": "删除文件或目录（目录必须 recursive=true；不可恢复，谨慎使用）",
          "inputSchema": { "type":"object","required":["path"],"properties":{
            "path": {"type":"string"}, "recursive": {"type":"boolean","description":"目录递归删除，默认 false"} } } },
        { "name": "pc.process.exec", "description": "执行命令（Windows cmd /C），捕获输出；超时终止进程树。多行脚本用 stdin 传给解释器（如 command=python + stdin=脚本），避免命令行转义",
          "inputSchema": { "type":"object","required":["command"],"properties":{
            "command": {"type":"string","description":"单行命令；解释器脚本内容请放 stdin"},
            "stdin": {"type":"string","description":"可选：写入子进程标准输入的内容（多行脚本）"},
            "workdir": {"type":"string","description":"工作目录，默认 root；cwd 为其别名"},
            "cwd": {"type":"string","description":"同 workdir"},
            "timeout": {"type":"integer","description":"秒，默认 30"},
            "max_output_chars": {"type":"integer","description":"默认 32000"} } } },
    ])
}

// ---------- helpers ----------

/// Windows 下 canonicalize 返回 \?\ 开头的 verbatim 路径，直接 starts_with 会因
/// 前缀类型不同而恒为 false。统一剥前缀 + 小写比较。
fn norm_prefix(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    let s = s.replace('/', "\\");
    s.trim_end_matches(['\\', '/']).to_ascii_lowercase()
}

pub(crate) fn within_root(child: &Path, root: &Path) -> bool {
    let c = norm_prefix(child);
    let r = norm_prefix(root);
    // 字符串前缀会误把 C:\\work-old 当作 C:\\work 的子目录；必须在目录分隔处截断。
    if c == r { return true; }
    if r.is_empty() { return root == Path::new("/") && c.starts_with('\\'); }
    c.strip_prefix(&r).is_some_and(|rest| rest.starts_with('\\'))
}

fn safe_path(cfg: &Cfg, raw: &str) -> Result<PathBuf, String> {
    let p = Path::new(raw);
    let joined = if p.is_absolute() { p.to_path_buf() } else { cfg.root.join(p) };
    let canon = std::fs::canonicalize(&joined).map_err(|e| format!("路径不存在（{}）：{e}", joined.display()))?;
    if !within_root(&canon, &cfg.root) {
        return Err(format!("路径越界：{} 不在根目录 {} 之内。", canon.display(), cfg.root.display()));
    }
    Ok(canon)
}

/// 原子写入（字节版）：同目录临时文件 + flush + rename；失败清理临时文件。
///
/// 安全：临时文件名随机且以 `create_new`（O_EXCL）独占创建——固定名字的临时文件
/// 可被预先放置的符号链接/重解析点劫持，`File::create` 会跟随链接把内容写到授权
/// 目录之外。最终路径本身已由 `safe_path_parent` 校验不是链接；rename 替换的是
/// 目录项而不会跟随目标链接。
fn atomic_write_bytes(full: &Path, bytes: &[u8]) -> Result<usize, String> {
    use std::io::Write;
    let dir = full.parent().ok_or("路径缺少父目录")?;
    let stem = full.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    let mut tmp = None;
    let mut file = None;
    for _ in 0..8 {
        let candidate = dir.join(format!(".{stem}.{}.tmp-shidrive", &uuid::Uuid::new_v4().simple().to_string()[..12]));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(f) => {
                file = Some(f);
                tmp = Some(candidate);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("创建临时文件失败: {e}")),
        }
    }
    let (Some(tmp), Some(mut f)) = (tmp, file) else { return Err("创建临时文件失败：名字冲突".into()) };
    let cleanup = |e: String| { let _ = std::fs::remove_file(&tmp); e };
    f.write_all(bytes).map_err(|e| cleanup(format!("写入失败: {e}")))?;
    f.flush().map_err(|e| cleanup(format!("flush 失败: {e}")))?;
    drop(f);
    // 最终目标在校验后又变成了链接（竞态/预置）：拒绝，不替换链接目标之外的任何东西
    if is_symlink(full) {
        return Err(cleanup(format!("目标是符号链接，已拒绝：{}", full.display())));
    }
    std::fs::rename(&tmp, full).map_err(|e| cleanup(format!("替换失败: {e}")))?;
    Ok(bytes.len())
}

/// 符号链接 / Windows 重解析点判定（不跟随）。
fn is_symlink(p: &Path) -> bool {
    p.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

fn b64_val(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a') as u32 + 26),
        b'0'..=b'9' => Some((c - b'0') as u32 + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// 标准字母表 base64 解码（容忍空白与 URL-safe 变体，校验填充）。
/// 不引入外部依赖，供 pc.fs.write/pc.fs.writeBatch 的 encoding=base64 使用。
fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let end = cleaned.iter().rposition(|&b| b != b'=').map(|i| i + 1).unwrap_or(0);
    let body = &cleaned[..end];
    if cleaned.len() - end > 2 {
        return Err("base64 无效：填充符过多".into());
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4 + 3);
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    for &c in body {
        let v = b64_val(c).ok_or_else(|| format!("base64 含非法字符: {}", c as char))?;
        acc = (acc << 6) | v;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push(((acc >> nbits) & 0xFF) as u8);
        }
    }
    if nbits >= 6 {
        return Err("base64 长度无效".into());
    }
    Ok(out)
}

pub(crate) fn call_tool(cfg: &Cfg, name: &str, args: &Value) -> Result<Value, String> {
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
            let text = decode_text(&data);
            let lines: Vec<&str> = text.split_inclusive('\n').collect();
            let start = (offset - 1).min(lines.len());
            let end = (start + limit).min(lines.len());
            Ok(json!({
                "path": full.to_string_lossy(),
                "total_lines": lines.len(),
                "offset": offset,
                "content": lines[start..end].concat(),
            }))
        }
        "pc.fs.write" => {
            let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                return Err("缺少必填参数 path".into());
            };
            let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
                return Err("缺少必填参数 content（如需清空文件请传空字符串）".into());
            };
            let encoding = args.get("encoding").and_then(|v| v.as_str()).unwrap_or("text");
            let full = safe_path_parent(cfg, path)?;
            let bytes: Vec<u8> = match encoding {
                "text" | "" => content.as_bytes().to_vec(),
                // FIX：二进制资源（PNG/图标等）直接 base64 解码写入，不再绕道 python 脚本
                "base64" => base64_decode(content)?,
                other => return Err(format!("不支持的 encoding: {other}（可选 text / base64）")),
            };
            let n = atomic_write_bytes(&full, &bytes)?;
            Ok(json!({ "written": true, "path": full.to_string_lossy(), "bytes": n }))
        }
        "pc.fs.mkdir" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let full = safe_path_parent(cfg, path)?;
            std::fs::create_dir_all(&full).map_err(|e| format!("创建失败: {e}"))?;
            Ok(json!({ "created": true, "path": full.to_string_lossy() }))
        }
        "pc.fs.list" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
            let full = safe_path(cfg, path)?;
            let mut entries: Vec<Value> = Vec::new();
            for e in std::fs::read_dir(&full).map_err(|err| format!("读取目录失败: {err}"))? {
                let e = e.map_err(|err| err.to_string())?;
                let meta = e.metadata();
                let mtime = meta.as_ref().ok().and_then(|m| m.modified().ok()).and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
                });
                entries.push(json!({
                    "name": e.file_name().to_string_lossy(),
                    "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                    "size": meta.as_ref().map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0),
                    "mtime": mtime,
                }));
            }
            entries.sort_by(|a, b| {
                let ad = a["is_dir"].as_bool().unwrap_or(false);
                let bd = b["is_dir"].as_bool().unwrap_or(false);
                bd.cmp(&ad).then(a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()))
            });
            let total = entries.len();
            let slice: Vec<Value> = entries.into_iter().skip(offset).take(limit).collect();
            Ok(json!({ "path": full.to_string_lossy(), "total": total, "entries": slice }))
        }
        "pc.fs.search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or_default();
            if query.is_empty() {
                return Err("缺少参数 query。".into());
            }
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let glob = args.get("glob").and_then(|v| v.as_str()).unwrap_or("*");
            let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let ignore_case = args.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);
            let full = safe_path(cfg, path)?;
            let mut matches: Vec<Value> = Vec::new();
            let mut skipped: Vec<Value> = Vec::new();
            let mut visited = 0usize;
            if full.is_file() {
                // FIX：path 允许直接指向单个文件（此前 read_dir 失败静默返回，
                // scanned_files 恒为 0，调用方完全不知道原因）
                scan_one_file(&full, query, ignore_case, max_results, &mut matches, &mut skipped, &mut visited);
            } else {
                search_walk(&full, query, glob, ignore_case, max_results, &mut matches, &mut skipped, &mut visited, 0);
            }
            Ok(json!({ "query": query, "matches": matches, "scanned_files": visited, "skipped": skipped }))
        }
        "pc.fs.exists" => {
            let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            // 存在性检查允许目标不存在：校验父目录在 root 内，再看目标本身（防探测 root 外路径）
            let p = Path::new(raw);
            let joined = if p.is_absolute() { p.to_path_buf() } else { cfg.root.join(p) };
            if joined.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                return Err("路径包含 ..，已拒绝。".into());
            }
            match std::fs::symlink_metadata(&joined) {
                Ok(m) => {
                    // 存在的绝对路径也要校验根目录（此前此分支直接返回元数据，可探测任意文件）。
                    // 符号链接目标不可指向开放目录外；断链只允许读取目录内的链接元数据。
                    let canonical = if m.file_type().is_symlink() {
                        std::fs::canonicalize(&joined).or_else(|_| {
                            std::fs::canonicalize(joined.parent().unwrap_or(&cfg.root))
                        })
                    } else {
                        std::fs::canonicalize(&joined)
                    }.map_err(|e| format!("路径检查失败：{e}"))?;
                    if !within_root(&canonical, &cfg.root) {
                        return Err("路径越界：不在开放目录内".into());
                    }
                    Ok(json!({
                        "exists": true,
                        "is_dir": m.is_dir(),
                        "is_symlink": m.file_type().is_symlink(),
                        "size": if m.is_file() { m.len() } else { 0 },
                    }))
                }
                Err(_) => {
                    let parent = joined.parent().unwrap_or(&cfg.root).to_path_buf();
                    let canon = std::fs::canonicalize(&parent)
                        .map_err(|e| format!("父目录不存在（{}）：{e}", parent.display()))?;
                    if !within_root(&canon, &cfg.root) {
                        return Err(format!("路径越界：{} 不在根目录 {} 之内。", joined.display(), cfg.root.display()));
                    }
                    Ok(json!({ "exists": false }))
                }
            }
        }
        "pc.fs.move" | "pc.fs.copy" => {
            let Some(src) = args.get("src").and_then(|v| v.as_str()) else {
                return Err("缺少必填参数 src".into());
            };
            let Some(dst) = args.get("dst").and_then(|v| v.as_str()) else {
                return Err("缺少必填参数 dst".into());
            };
            let from = safe_path(cfg, src)?;
            let root_canon = std::fs::canonicalize(&cfg.root).unwrap_or_else(|_| cfg.root.clone());
            if norm_prefix(&from) == norm_prefix(&root_canon) {
                return Err("拒绝操作根目录本身。".into());
            }
            let to = safe_path_parent(cfg, dst)?;
            // dst 为已存在目录时移入该目录（保留原文件名）。此时真正被写的是拼接后的
            // 路径，必须对它（而不只是用户给的 dst）再做一次链接检查。
            let target = if to.is_dir() {
                to.join(from.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default())
            } else {
                to
            };
            if is_symlink(&target) {
                return Err(format!("目标是符号链接，已拒绝：{}", target.display()));
            }
            if name == "pc.fs.move" {
                std::fs::rename(&from, &target).map_err(|e| format!("移动失败（跨盘符或目标占用时会失败，可改用 copy+delete）: {e}"))?;
                Ok(json!({ "moved": true, "src": from.to_string_lossy(), "dst": target.to_string_lossy() }))
            } else {
                let n = copy_path_recursive(&from, &target, 0)?;
                Ok(json!({ "copied": true, "src": from.to_string_lossy(), "dst": target.to_string_lossy(), "bytes": n }))
            }
        }
        "pc.fs.delete" => {
            let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                return Err("缺少必填参数 path".into());
            };
            let full = safe_path(cfg, path)?;
            let root_canon = std::fs::canonicalize(&cfg.root).unwrap_or_else(|_| cfg.root.clone());
            if norm_prefix(&full) == norm_prefix(&root_canon) {
                return Err("拒绝删除根目录本身。".into());
            }
            let m = std::fs::symlink_metadata(&full).map_err(|e| format!("读取失败: {e}"))?;
            if m.is_dir() {
                let recursive = args.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false);
                if !recursive {
                    return Err("目标是目录：需传 recursive=true 才会递归删除（删除不可恢复，请谨慎）。".into());
                }
                std::fs::remove_dir_all(&full).map_err(|e| format!("删除目录失败: {e}"))?;
            } else {
                std::fs::remove_file(&full).map_err(|e| format!("删除文件失败: {e}"))?;
            }
            Ok(json!({ "deleted": true, "path": full.to_string_lossy() }))
        }
        "pc.fs.writeBatch" => {
            let Some(items) = args.get("items").and_then(|v| v.as_array()) else {
                return Err("缺少必填参数 items（数组，每项 {path, content, encoding?}，单次最多 50）".into());
            };
            if items.len() > 50 {
                return Err("单次最多 50 个文件。".into());
            }
            let mut results: Vec<Value> = Vec::new();
            let mut ok_count = 0usize;
            for (i, item) in items.iter().enumerate() {
                let mut r = (|| -> Result<Value, String> {
                    let path = item.get("path").and_then(|v| v.as_str()).ok_or("缺少 path")?;
                    let content = item.get("content").and_then(|v| v.as_str()).ok_or("缺少 content")?;
                    let encoding = item.get("encoding").and_then(|v| v.as_str()).unwrap_or("text");
                    let full = safe_path_parent(cfg, path)?;
                    let bytes: Vec<u8> = match encoding {
                        "text" | "" => content.as_bytes().to_vec(),
                        "base64" => base64_decode(content)?,
                        o => return Err(format!("不支持的 encoding: {o}")),
                    };
                    let n = atomic_write_bytes(&full, &bytes)?;
                    Ok(json!({ "written": true, "path": full.to_string_lossy(), "bytes": n }))
                })()
                .unwrap_or_else(|e| json!({ "written": false, "error": e }));
                if r.get("written").and_then(|v| v.as_bool()).unwrap_or(false) {
                    ok_count += 1;
                }
                r["index"] = json!(i);
                results.push(r);
            }
            Ok(json!({ "total": items.len(), "written": ok_count, "results": results }))
        }
        "pc.process.exec" => {
            if !cfg.exec_enabled {
                return Err("命令执行已被禁用（CODING_MCP_ALLOW_EXEC=0 / --exec 0）。文件读写仍可用。".into());
            }
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or_default();
            if command.trim().is_empty() {
                return Err("缺少参数 command。".into());
            }
            // FIX：cwd 为 workdir 的别名；多行脚本经 stdin 传入，绕开命令行三层转义
            let workdir = args
                .get("workdir")
                .and_then(|v| v.as_str())
                .or_else(|| args.get("cwd").and_then(|v| v.as_str()))
                .unwrap_or_default();
            let stdin_data = args.get("stdin").and_then(|v| v.as_str()).map(|s| s.to_string());
            let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);
            let max_chars = args.get("max_output_chars").and_then(|v| v.as_u64()).unwrap_or(32000) as usize;
            let dir = if workdir.is_empty() { cfg.root.clone() } else { safe_path(cfg, workdir)? };
            run_exec(command, &dir, timeout, max_chars, stdin_data.as_deref())
        }
        other => Err(format!("未知工具 {other}。可用：pc.fs.read、pc.fs.write、pc.fs.list、pc.fs.mkdir、pc.fs.search、pc.fs.exists、pc.fs.move、pc.fs.copy、pc.fs.delete、pc.fs.writeBatch、pc.process.exec。")),
    }
}

/// 允许写入尚不存在的路径：校验其规范化父目录在 root 内。
fn safe_path_parent(cfg: &Cfg, raw: &str) -> Result<PathBuf, String> {
    let p = Path::new(raw);
    let joined = if p.is_absolute() { p.to_path_buf() } else { cfg.root.join(p) };
    if joined.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("路径包含 ..，已拒绝。".into());
    }
    let parent = joined.parent().unwrap_or(&cfg.root);
    let parent_canon = std::fs::canonicalize(parent).map_err(|e| format!("父目录不存在（{}）：{e}", parent.display()))?;
    if !within_root(&parent_canon, &cfg.root) {
        return Err(format!("路径越界：{} 不在根目录 {} 之内。", joined.display(), cfg.root.display()));
    }
    // 拼回文件名，保留原大小写；若目标已是符号链接/重解析点，同样拒绝（防逃逸）
    let file = joined.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    if file.is_empty() {
        return Err("路径缺少文件名。".into());
    }
    let full = parent_canon.join(&file);
    if full.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Err(format!("目标是符号链接，已拒绝：{}", full.display()));
    }
    Ok(full)
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

/// 搜索跳过记录（上限 20 条）：让调用方知道为什么没扫到
fn push_skip(skipped: &mut Vec<Value>, path: &Path, reason: &str) {
    if skipped.len() < 20 {
        skipped.push(json!({ "path": path.to_string_lossy(), "reason": reason }));
    }
}

/// UTF-16LE/BE 字节流解码（Windows 编辑器保存的带 BOM 文本文件常见）
fn utf16_to_string(data: &[u8], little: bool) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| if little { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
        .collect();
    String::from_utf16_lossy(&units)
}

/// 文本解码：识别并剥离 UTF-8 BOM / UTF-16 LE/BE BOM；无 BOM 按 UTF-8 lossy。
fn decode_text(data: &[u8]) -> String {
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(&data[3..]).into_owned()
    } else if data.starts_with(&[0xFF, 0xFE]) {
        utf16_to_string(&data[2..], true)
    } else if data.starts_with(&[0xFE, 0xFF]) {
        utf16_to_string(&data[2..], false)
    } else {
        String::from_utf8_lossy(data).into_owned()
    }
}

/// 递归复制 src → dst：文件直接复制；目录递归；源侧符号链接跳过（防逃出授权根）。
/// 目标侧每一层都不得是符号链接/重解析点：`std::fs::copy` 与 `create_dir_all`
/// 都会跟随已存在的目标链接，预先放置的链接会把内容写到授权目录之外。
/// 返回复制的总字节数。
fn copy_path_recursive(src: &Path, dst: &Path, depth: usize) -> Result<u64, String> {
    if depth > 24 {
        return Err("目录层级过深（>24），可能存在循环链接".into());
    }
    let meta = std::fs::symlink_metadata(src).map_err(|e| format!("读取源失败: {e}"))?;
    if meta.file_type().is_symlink() {
        return Ok(0); // 符号链接不跟随，跳过
    }
    if is_symlink(dst) {
        return Err(format!("目标是符号链接，已拒绝：{}", dst.display()));
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("创建目录失败: {e}"))?;
        let mut total = 0u64;
        for entry in std::fs::read_dir(src).map_err(|e| format!("读取目录失败: {e}"))? {
            let entry = entry.map_err(|e| e.to_string())?;
            total += copy_path_recursive(&entry.path(), &dst.join(entry.file_name()), depth + 1)?;
        }
        Ok(total)
    } else {
        std::fs::copy(src, dst).map_err(|e| format!("复制文件失败: {e}"))
    }
}

/// 扫描单个文件并按行匹配（跳过时原因写入 skipped）。
/// FIX：BOM/UTF-16 正确解码；二进制判定改为「无 BOM 且前 64KB 含 NUL」——
/// 此前整文件任一 NUL 即跳过，UTF-16 编码文件（Windows 编辑器常见）永远搜不到。
fn scan_one_file(
    path: &Path,
    query: &str,
    ignore_case: bool,
    max_results: usize,
    out: &mut Vec<Value>,
    skipped: &mut Vec<Value>,
    visited: &mut usize,
) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            push_skip(skipped, path, &format!("read_error: {e}"));
            return;
        }
    };
    let head = &data[..data.len().min(64 * 1024)];
    let has_bom = data.starts_with(&[0xEF, 0xBB, 0xBF])
        || data.starts_with(&[0xFF, 0xFE])
        || data.starts_with(&[0xFE, 0xFF]);
    if !has_bom && head.contains(&0) {
        push_skip(skipped, path, "binary_or_utf16_without_bom");
        return;
    }
    let text = decode_text(&data);
    *visited += 1;
    let needle = if ignore_case { query.to_lowercase() } else { String::new() };
    for (i, line) in text.lines().enumerate() {
        let hit = if ignore_case {
            line.to_lowercase().contains(&needle)
        } else {
            line.contains(query)
        };
        if hit {
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

fn search_walk(
    dir: &Path,
    query: &str,
    glob: &str,
    ignore_case: bool,
    max_results: usize,
    out: &mut Vec<Value>,
    skipped: &mut Vec<Value>,
    visited: &mut usize,
    depth: usize,
) {
    if out.len() >= max_results || depth > 12 {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            push_skip(skipped, dir, &format!("read_dir_error: {e}"));
            return;
        }
    };
    for e in rd.flatten() {
        if out.len() >= max_results {
            return;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let path = e.path();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if !skip_dir(&name) {
                search_walk(&path, query, glob, ignore_case, max_results, out, skipped, visited, depth + 1);
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
                push_skip(skipped, &path, "too_large(>4MiB)");
                continue;
            }
            scan_one_file(&path, query, ignore_case, max_results, out, skipped, visited);
        }
    }
}

fn run_exec(command: &str, dir: &Path, timeout: u64, max_chars: usize, stdin_data: Option<&str>) -> Result<Value, String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "sh" });
    cmd.current_dir(dir);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // FIX：raw_arg 原样拼接命令行。此前 std 的 arg() 按 MSVC 规则改写引号，
        // 而 cmd.exe 不遵循该规则，中文/嵌套引号/特殊字符经多层转义后几乎必坏。
        cmd.raw_arg("/C").raw_arg(command);
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    {
        cmd.arg("-c").arg(command);
    }
    let mut child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    crate::child_job::attach(&child);
    let pid = child.id();
    // FIX：stdin 参数——多行脚本从标准输入喂给解释器（python/node 等），
    // 彻底绕开 JSON→cmd→解释器 的命令行转义地狱。独立线程写入后随手柄关闭，
    // 子进程不读也不会阻塞主管线。
    if let Some(data) = stdin_data {
        if let Some(mut sin) = child.stdin.take() {
            let owned = data.as_bytes().to_vec();
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = sin.write_all(&owned);
            });
        }
    }
    let out = child.stdout.take();
    let err = child.stderr.take();
    // FIX-06：限额流式缓冲——超限后继续排空管道（不阻塞子进程），仅保留尾部 keep 字节
    fn drain<R: std::io::Read + Send + 'static>(
        pipe: Option<R>,
        keep: usize,
    ) -> std::thread::JoinHandle<(Vec<u8>, u64)> {
        std::thread::spawn(move || {
            let mut buf: Vec<u8> = Vec::new();
            let mut dropped: u64 = 0;
            let mut chunk = [0u8; 8192];
            if let Some(mut p) = pipe {
                loop {
                    match std::io::Read::read(&mut p, &mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.len() > keep {
                                let excess = buf.len() - keep;
                                buf.drain(..excess);
                                dropped += excess as u64;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
            (buf, dropped)
        })
    }
    let out_handle = drain(out, 512 * 1024);
    let err_handle = drain(err, 512 * 1024);
    // FIX-03：超时先终止进程树，再限时回收读取线程（防止后代持管道卡死）
    let status = wait_with_timeout(&mut child, timeout);
    let timed_out = status.is_none();
    if timed_out {
        let mut killer = std::process::Command::new("taskkill");
        killer.args(["/F", "/T", "/PID", &pid.to_string()]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            killer.creation_flags(0x0800_0000);
        }
        let _ = killer.output();
        let _ = child.wait();
    }
    let (stdout_raw, stdout_dropped) = out_handle.join().unwrap_or((Vec::new(), 0));
    let (stderr_raw, stderr_dropped) = err_handle.join().unwrap_or((Vec::new(), 0));
    let stdout_txt = String::from_utf8_lossy(&stdout_raw);
    let stderr_txt = String::from_utf8_lossy(&stderr_raw);
    let trunc_note = |dropped: u64| if dropped > 0 { format!("（另有 {dropped} 字节超限丢弃）") } else { String::new() };
    let cut = |s: &str| -> String {
        if s.chars().count() > max_chars {
            s.chars().take(max_chars).collect::<String>() + "\n…(截断)"
        } else {
            s.to_string()
        }
    };
    match status {
        Some(st) => Ok(json!({
            "exit_code": st.code().unwrap_or(-1),
            "stdout": cut(&stdout_txt),
            "stderr": cut(&stderr_txt),
            "workdir": dir.to_string_lossy(),
            "stdout_dropped_bytes": stdout_dropped,
            "stderr_dropped_bytes": stderr_dropped,
            "stdout_truncated_note": trunc_note(stdout_dropped),
            "stderr_truncated_note": trunc_note(stderr_dropped),
        })),
        None => {
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

#[cfg(test)]
mod boundary_tests {
    use super::*;

    fn cfg_at(root: std::path::PathBuf) -> Cfg {
        Cfg { root, token: String::new(), auth_user: String::new(), auth_pass: String::new(), exec_enabled: false }
    }

    /// Pre-planted symlinks inside the authorized directory must never let a write
    /// land outside it: neither via the copy target (dst is a directory, final name
    /// is a link), nor via nested targets of a recursive copy, nor via the atomic
    /// write's temporary file.
    #[cfg(unix)]
    #[test]
    fn preplanted_symlinks_cannot_redirect_copy_or_write_outside_root() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("shidrive-symlink-test-{}", uuid::Uuid::new_v4()));
        let root = base.join("project");
        let outside = base.join("outside.txt");
        std::fs::create_dir_all(root.join("dest")).unwrap();
        std::fs::create_dir_all(root.join("tree/sub")).unwrap();
        std::fs::write(&outside, "ORIGINAL").unwrap();
        std::fs::write(root.join("src.txt"), "payload").unwrap();
        std::fs::write(root.join("tree/sub/file.txt"), "payload").unwrap();
        let cfg = cfg_at(root.clone());

        // 1. copy file into a directory whose <srcname> entry is a link to the outside
        symlink(&outside, root.join("dest/src.txt")).unwrap();
        let r = call_tool(&cfg, "pc.fs.copy", &json!({ "src": "src.txt", "dst": "dest" }));
        assert!(r.is_err(), "copy onto symlinked target must be refused: {r:?}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "ORIGINAL");

        // 2. recursive copy of `tree` into existing dir `copy` (→ copy/tree/…) where the
        //    nested destination file is a pre-planted link to the outside
        std::fs::create_dir_all(root.join("copy/tree/sub")).unwrap();
        symlink(&outside, root.join("copy/tree/sub/file.txt")).unwrap();
        let r = call_tool(&cfg, "pc.fs.copy", &json!({ "src": "tree", "dst": "copy" }));
        assert!(r.is_err(), "nested symlinked target must be refused: {r:?}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "ORIGINAL");

        // 2b. nested destination *directory* is a link to an outside directory
        let outside_dir = base.join("outside-dir");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::create_dir_all(root.join("copy2/tree")).unwrap();
        symlink(&outside_dir, root.join("copy2/tree/sub")).unwrap();
        let r = call_tool(&cfg, "pc.fs.copy", &json!({ "src": "tree", "dst": "copy2" }));
        assert!(r.is_err(), "symlinked destination directory must be refused: {r:?}");
        assert!(!outside_dir.join("file.txt").exists());

        // 2c. an honest copy still works
        let r = call_tool(&cfg, "pc.fs.copy", &json!({ "src": "tree", "dst": "copy3" }));
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(std::fs::read_to_string(root.join("copy3/sub/file.txt")).unwrap(), "payload");

        // 3. atomic write: a planted link at the old fixed temp name must be irrelevant,
        //    and a link at the final path is refused
        symlink(&outside, root.join("out.tmp-shidrive")).unwrap();
        let r = call_tool(&cfg, "pc.fs.write", &json!({ "path": "out.txt", "content": "NEW" }));
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(std::fs::read_to_string(root.join("out.txt")).unwrap(), "NEW");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "ORIGINAL");
        symlink(&outside, root.join("linked.txt")).unwrap();
        let r = call_tool(&cfg, "pc.fs.write", &json!({ "path": "linked.txt", "content": "NEW" }));
        assert!(r.is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "ORIGINAL");
        // no temp files left behind
        let leftovers: Vec<_> = std::fs::read_dir(&root).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp-shidrive") && !e.path().is_symlink())
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn exists_and_read_cannot_probe_sibling_or_outside_root() {
        let base = std::env::temp_dir().join(format!("shidrive-boundary-test-{}", uuid::Uuid::new_v4()));
        let root = base.join("project");
        let sibling = base.join("project-backup");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let external_file = sibling.join("secret.txt");
        std::fs::write(&external_file, "private").unwrap();
        assert!(within_root(&root, &root));
        assert!(!within_root(&sibling, &root), "相同前缀不代表子目录");
        let cfg = Cfg { root, token: String::new(), auth_user: String::new(), auth_pass: String::new(), exec_enabled: false };
        let args = json!({ "path": external_file.to_string_lossy() });
        assert!(call_tool(&cfg, "pc.fs.read", &args).is_err());
        assert!(call_tool(&cfg, "pc.fs.exists", &args).is_err());
        #[cfg(unix)] {
            let link = cfg.root.join("link");
            std::os::unix::fs::symlink(&external_file, &link).unwrap();
            assert!(call_tool(&cfg, "pc.fs.exists", &json!({ "path": "link" })).is_err());
        }
        std::fs::remove_dir_all(base).unwrap();
    }
}
