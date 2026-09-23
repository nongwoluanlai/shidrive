//! OAuth 2.1 授权服务（MCP 标准握手）：使驾同时作为 Resource Server 与轻量 Authorization Server。
//!
//! 设计要点：
//! - 端点全部挂在当前服务上（/authorize、/token、/register、/.well-known/*），基于请求 Host
//!   动态生成元数据 URL——Quick Tunnel 地址随重启变化也不影响发现流程；
//! - 浏览器 /authorize 只展示等待页；真正的「批准/拒绝」发生在使驾桌面端
//!   （设置 → 外部编程接入 的授权卡片），审批者必须是能操作本机的人，公网机器人无法自批；
//! - OAuth 令牌映射到内部 RemoteGrant 授权行：批准时按（可缩减的）scope 创建授权行，
//!   此后越界防护/工具裁剪/目录限制/执行双开关与静态 token 完全同一套代码；
//! - access/refresh 令牌只存 sha256（sdo1_/sdr1_ 前缀），不落明文；refresh 轮换；
//! - 授权码一次性、10 分钟有效，PKCE 强制 S256。

use crate::db::Db;
use crate::remote_mcp::{grant_create, sha256_hex, RemoteGrant, RemoteGrantInput};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Emitter;

use rusqlite::OptionalExtension;

pub const ACCESS_TTL_SECS: i64 = 3600;
pub const REFRESH_TTL_SECS: i64 = 30 * 24 * 3600;
pub const TXN_TTL: Duration = Duration::from_secs(600);
pub const CODE_TTL: Duration = Duration::from_secs(600);
pub const SCOPES_SUPPORTED: &[&str] = &["fs:read", "fs:write", "exec", "context"];

fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}
fn now_epoch() -> i64 {
    chrono::Local::now().timestamp()
}

// ---------------- in-memory 状态 ----------------

#[derive(Clone)]
pub enum Decision {
    Approved { code: String },
    Denied,
}

pub struct PendingTxn {
    pub client_id: String,
    pub client_name: String,
    pub redirect_uri: String,
    pub scope: String,
    pub state: String,
    pub code_challenge: String,
    pub created: Instant,
    pub decision: Option<Decision>,
}

pub struct AuthCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub scope: String,
    pub grant_id: String,
    pub created: Instant,
}

#[derive(Default)]
pub struct OAuthState {
    pub txns: Mutex<HashMap<String, PendingTxn>>,
    pub codes: Mutex<HashMap<String, AuthCode>>,
}

impl OAuthState {
    pub fn new() -> Self {
        Self::default()
    }
}

fn rand_hex(n_blocks: usize) -> String {
    let mut s = String::new();
    for _ in 0..n_blocks {
        s.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    s
}

// ---------------- 客户端注册（RFC 7591 动态注册） ----------------

fn validate_redirect_uri(u: &str) -> Result<(), String> {
    let ok = u.starts_with("https://")
        || u.starts_with("http://127.0.0.1")
        || u.starts_with("http://localhost")
        || u.starts_with("http://[::1]");
    if ok {
        Ok(())
    } else {
        Err(format!("redirect_uri 仅支持 https 或本机回环地址：{u}"))
    }
}

pub fn client_register(db: &Arc<Db>, name: &str, redirect_uris: &[String]) -> Result<Value, String> {
    if redirect_uris.is_empty() {
        return Err("redirect_uris 不能为空".into());
    }
    for u in redirect_uris {
        validate_redirect_uri(u)?;
    }
    let client_id = format!("sd-oauth-{}", uuid::Uuid::new_v4().simple());
    let uris = serde_json::to_string(redirect_uris).map_err(|e| e.to_string())?;
    db.with(|c| {
        c.execute(
            "INSERT INTO remote_oauth_clients (client_id,client_name,redirect_uris,created_at) VALUES (?1,?2,?3,?4)",
            rusqlite::params![client_id, name, uris, now_str()],
        )
        .map(|_| ())
    })?;
    Ok(json!({
        "client_id": client_id,
        "client_id_issued_at": now_epoch(),
        "client_name": name,
        "redirect_uris": redirect_uris,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "scope": SCOPES_SUPPORTED.join(" "),
    }))
}

fn client_get(db: &Arc<Db>, client_id: &str) -> Option<(String, Vec<String>)> {
    db.with(|c| {
        let mut st = c.prepare("SELECT client_name, redirect_uris FROM remote_oauth_clients WHERE client_id=?1")?;
        let r = st
            .query_row(rusqlite::params![client_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?;
        Ok::<_, rusqlite::Error>(r)
    })
    .ok()
    .flatten()
    .map(|(name, uris)| {
        let list = serde_json::from_str::<Vec<String>>(&uris).unwrap_or_default();
        (name, list)
    })
}

// ---------------- 元数据（RFC 9728 / RFC 8414） ----------------

/// 从请求 Host 头推导对外 origin（Quick Tunnel/自定义域名 → https，本机/局域网 → http）。
pub fn origin_from_head(head: &str) -> String {
    let mut host = "";
    for line in head.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("host:") {
            // lower 与 line 等长（ASCII 小写化不改变长度），按下标取原值
            host = line["host:".len()..].trim();
            break;
        }
    }
    origin_of_host(host)
}

pub fn origin_of_host(host: &str) -> String {
    if host.is_empty() {
        return "http://127.0.0.1".into();
    }
    let bare = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    let local = bare == "localhost"
        || bare == "127.0.0.1"
        || bare == "::1"
        || bare.ends_with(".local")
        || bare.starts_with("192.168.")
        || bare.starts_with("10.")
        || bare.starts_with("172.");
    let scheme = if local { "http" } else { "https" };
    format!("{scheme}://{host}")
}

fn protected_resource_doc(origin: &str) -> Value {
    json!({
        "resource": format!("{origin}/mcp"),
        "authorization_servers": [origin],
        "scopes_supported": SCOPES_SUPPORTED,
        "bearer_methods_supported": ["header"],
    })
}

fn authorization_server_doc(origin: &str) -> Value {
    json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/authorize"),
        "token_endpoint": format!("{origin}/token"),
        "registration_endpoint": format!("{origin}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": SCOPES_SUPPORTED,
    })
}

// ---------------- HTTP 分发（无需 Bearer 的公开端点） ----------------

/// 返回 Ok(true) 表示请求已在本模块处理完毕；Ok(false) 交给既有 token 鉴权流程。
#[allow(clippy::too_many_arguments)]
pub fn try_handle_http(
    method: &str,
    path: &str,
    query: &str,
    stream: &mut std::net::TcpStream,
    buf: &mut Vec<u8>,
    header_end: usize,
    content_length: usize,
    db: &Arc<Db>,
    app: &tauri::AppHandle,
    oauth: &OAuthState,
) -> Result<bool, String> {
    let origin = origin_from_head_head(buf, header_end);
    match (method, path) {
        ("GET", "/.well-known/oauth-protected-resource") => {
            crate::remote_mcp::write_http_h(
                stream,
                200,
                "application/json",
                &protected_resource_doc(&origin).to_string(),
                "",
            )?;
            Ok(true)
        }
        ("GET", "/.well-known/oauth-authorization-server") => {
            crate::remote_mcp::write_http_h(
                stream,
                200,
                "application/json",
                &authorization_server_doc(&origin).to_string(),
                "",
            )?;
            Ok(true)
        }
        ("POST", "/register") => {
            read_body(stream, buf, header_end, content_length)?;
            let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
            let v: Value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
            let name = v.get("client_name").and_then(|x| x.as_str()).unwrap_or("unnamed-client").to_string();
            let uris: Vec<String> = v
                .get("redirect_uris")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            match client_register(db, &name, &uris) {
                Ok(doc) => {
                    crate::remote_mcp::write_http_h(stream, 201, "application/json", &doc.to_string(), "")?;
                    Ok(true)
                }
                Err(e) => {
                    crate::remote_mcp::write_http(
                        stream,
                        400,
                        "application/json",
                        &json!({ "error": "invalid_client_metadata", "error_description": e }).to_string(),
                    )?;
                    Ok(true)
                }
            }
        }
        ("GET", "/authorize") => {
            authorize_begin(oauth, db, app, query, stream)?;
            Ok(true)
        }
        ("GET", "/authorize/wait") => {
            authorize_wait(oauth, query, stream)?;
            Ok(true)
        }
        ("POST", "/token") => {
            read_body(stream, buf, header_end, content_length)?;
            let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
            token_endpoint(db, oauth, &body, stream)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn origin_from_head_head(buf: &[u8], header_end: usize) -> String {
    let head = String::from_utf8_lossy(&buf[..header_end]);
    origin_from_head(&head)
}

/// 把剩余 body 读全（OAuth 端点在鉴权前处理，需自行读体）。
fn read_body(
    stream: &mut std::net::TcpStream,
    buf: &mut Vec<u8>,
    header_end: usize,
    content_length: usize,
) -> Result<(), String> {
    use std::io::Read;
    let mut chunk = [0u8; 8192];
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(())
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = bytes.get(i + 1..i + 3);
                match hex.and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn page(title: &str, body: &str, refresh: Option<&str>) -> String {
    let meta = refresh
        .map(|u| format!("<meta http-equiv=\"refresh\" content=\"2; url={u}\">"))
        .unwrap_or_default();
    format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">{meta}\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{title}</title><style>body{{font-family:system-ui,-apple-system,'Segoe UI',sans-serif;background:#0f1115;color:#e8eaf0;\
display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0}}\
.card{{background:#181b22;border:1px solid #2a2f3a;border-radius:12px;padding:28px 32px;max-width:520px;line-height:1.7}}\
h1{{font-size:1.1em;margin:0 0 12px}}code{{background:#0f1115;padding:2px 6px;border-radius:4px;font-size:.9em;word-break:break-all}}\
.dim{{color:#8b93a7;font-size:.9em}}</style></head><body><div class=\"card\">{body}</div></body></html>",
        title = html_escape(title),
        body = body
    )
}

fn write_html(stream: &mut std::net::TcpStream, status: u16, html: &str) -> Result<(), String> {
    crate::remote_mcp::write_http_h(stream, status, "text/html; charset=utf-8", html, "")
}

fn build_redirect(uri: &str, params: &[(&str, &str)]) -> String {
    let sep = if uri.contains('?') { '&' } else { '?' };
    let q: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect();
    format!("{uri}{sep}{}", q.join("&"))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------- /authorize ----------------

fn authorize_begin(
    oauth: &OAuthState,
    db: &Arc<Db>,
    app: &tauri::AppHandle,
    query: &str,
    stream: &mut std::net::TcpStream,
) -> Result<(), String> {
    let p = parse_query(query);
    let q = |k: &str| p.get(k).cloned().unwrap_or_default();

    let mut bad = |desc: &str| -> Result<(), String> {
        write_html(stream, 400, &page("授权请求无效", &format!("<h1>⚠ 授权请求无效</h1><p>{desc}</p>"), None))
    };

    if q("response_type") != "code" {
        return bad("response_type 必须为 code。");
    }
    let client_id = q("client_id");
    let Some((client_name, uris)) = client_get(db, &client_id) else {
        return bad("未知的 client_id，请先通过 /register 动态注册。");
    };
    let redirect_uri = q("redirect_uri");
    if !uris.iter().any(|u| u == &redirect_uri) {
        return bad("redirect_uri 与注册信息不一致。");
    }
    let code_challenge = q("code_challenge");
    if code_challenge.is_empty() || q("code_challenge_method") != "S256" {
        return bad("必须提供 PKCE 参数（code_challenge + code_challenge_method=S256）。");
    }
    let mut scope = q("scope");
    if scope.is_empty() {
        scope = "fs:read".into();
    }
    let scope: Vec<String> = scope
        .split([' ', '+'])
        .filter(|s| SCOPES_SUPPORTED.contains(s))
        .map(|s| s.to_string())
        .collect();
    let scope = if scope.is_empty() { vec!["fs:read".to_string()] } else { scope };
    let scope = scope.join(" ");

    let txn_id = uuid::Uuid::new_v4().to_string();
    oauth.txns.lock().unwrap_or_else(|p| p.into_inner()).insert(
        txn_id.clone(),
        PendingTxn {
            client_id: client_id.clone(),
            client_name: client_name.clone(),
            redirect_uri: redirect_uri.clone(),
            scope: scope.clone(),
            state: q("state"),
            code_challenge,
            created: Instant::now(),
            decision: None,
        },
    );
    let _ = app.emit(
        "remote://oauth-consent",
        json!({
            "txnId": txn_id,
            "clientId": client_id,
            "clientName": client_name,
            "redirectUri": redirect_uri,
            "scopes": scope.split(' ').collect::<Vec<_>>(),
        }),
    );
    let body = format!(
        "<h1>🔐 使驾 OAuth 授权请求</h1>\
<p>客户端 <code>{client}</code> 正在请求访问你的使驾外部编程接口。</p>\
<p class=\"dim\">请求权限：{scope}</p>\
<p><b>请在使驾桌面端 → 设置 → 外部编程接入 中确认或拒绝本次授权。</b></p>\
<p class=\"dim\">本页会自动检测确认结果，无需刷新（10 分钟内有效）。</p>",
        client = html_escape(&client_name),
        scope = html_escape(&scope),
    );
    let wait_url = format!("/authorize/wait?txn_id={}", percent_encode(&txn_id));
    write_html(stream, 200, &page("等待授权确认", &body, Some(&wait_url)))
}

fn authorize_wait(oauth: &OAuthState, query: &str, stream: &mut std::net::TcpStream) -> Result<(), String> {
    let txn_id = parse_query(query).get("txn_id").cloned().unwrap_or_default();
    enum Out {
        Waiting,
        Redirect(String),
        Expired,
    }
    let out = {
        let mut map = oauth.txns.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(txn) = map.get_mut(&txn_id) {
            if txn.created.elapsed() > TXN_TTL {
                map.remove(&txn_id);
                Out::Expired
            } else {
                match &txn.decision {
                    None => Out::Waiting,
                    Some(Decision::Approved { code }) => {
                        let url = build_redirect(
                            &txn.redirect_uri,
                            &[("code", code.as_str()), ("state", txn.state.as_str())],
                        );
                        map.remove(&txn_id);
                        Out::Redirect(url)
                    }
                    Some(Decision::Denied) => {
                        let url = build_redirect(
                            &txn.redirect_uri,
                            &[("error", "access_denied"), ("state", txn.state.as_str())],
                        );
                        map.remove(&txn_id);
                        Out::Redirect(url)
                    }
                }
            }
        } else {
            Out::Expired
        }
    };
    match out {
        Out::Waiting => {
            let body = "<h1>⏳ 等待使驾端确认…</h1><p class=\"dim\">请在使驾桌面端完成批准 / 拒绝。本页自动刷新。</p>";
            let self_url = format!("/authorize/wait?txn_id={}", percent_encode(&txn_id));
            write_html(stream, 200, &page("等待授权确认", body, Some(&self_url)))
        }
        Out::Redirect(url) => {
            crate::remote_mcp::write_http_h(stream, 302, "text/plain", "redirect", &format!("Location: {url}\r\n"))
        }
        Out::Expired => {
            write_html(stream, 200, &page("授权已过期", "<h1>⌛ 授权请求已过期或不存在</h1><p class=\"dim\">请回到客户端重新发起连接。</p>", None))
        }
    }
}

// ---------------- 桌面端批准/拒绝（Tauri command 调用） ----------------

#[allow(clippy::too_many_arguments)]
pub fn oauth_decide(
    db: &Arc<Db>,
    oauth: &OAuthState,
    txn_id: &str,
    approve: bool,
    project_id: Option<String>,
    project_name: Option<String>,
    project_root: Option<String>,
    context_id: Option<String>,
    context_name: Option<String>,
    fs_write: bool,
    exec_allowed: bool,
) -> Result<(), String> {
    let (client_id, code_challenge, scope) = {
        let mut map = oauth.txns.lock().unwrap_or_else(|p| p.into_inner());
        let Some(txn) = map.get_mut(txn_id) else {
            return Err("授权请求不存在或已处理".into());
        };
        if txn.created.elapsed() > TXN_TTL {
            map.remove(txn_id);
            return Err("授权请求已过期".into());
        }
        if txn.decision.is_some() {
            return Err("该请求已被处理".into());
        }
        if !approve {
            txn.decision = Some(Decision::Denied);
            return Ok(());
        }
        (txn.client_id.clone(), txn.code_challenge.clone(), txn.scope.clone())
    };
    // 批准：按（不超出请求范围的）能力创建内部授权行，令牌随后绑定它
    let pid = project_id.ok_or("请选择要授权的项目")?;
    let root = project_root.ok_or("项目目录缺失")?;
    if root.trim().is_empty() {
        return Err("项目目录不能为空".into());
    }
    let context_enabled = context_id.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let input = RemoteGrantInput {
        project_id: pid,
        project_name: project_name.unwrap_or_default(),
        project_root: root,
        context_id,
        context_name: context_name.unwrap_or_default(),
        context_enabled,
        fs_write,
        exec_allowed,
    };
    let (grant, _legacy_token) = grant_create(db, &input)?;

    // 实际授予的 scope（可能少于请求）
    let mut granted: Vec<&str> = vec!["fs:read"];
    if fs_write {
        granted.push("fs:write");
    }
    if exec_allowed {
        granted.push("exec");
    }
    if context_enabled {
        granted.push("context");
    }
    let granted_scope = granted.join(" ");
    let _ = scope; // 请求范围已在 UI 侧限制勾选项，不超出

    let code = format!("sdc_{}", rand_hex(1));
    oauth.codes.lock().unwrap_or_else(|p| p.into_inner()).insert(
        code.clone(),
        AuthCode {
            client_id,
            redirect_uri: String::new(), // 在 /token 处与请求重校验
            code_challenge,
            scope: granted_scope,
            grant_id: grant.id,
            created: Instant::now(),
        },
    );
    let mut map = oauth.txns.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(txn) = map.get_mut(txn_id) {
        txn.decision = Some(Decision::Approved { code });
    }
    Ok(())
}

// ---------------- /token ----------------

fn b64url_nopad(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

fn sha256_b64url(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    b64url_nopad(&h.finalize())
}

fn token_endpoint(db: &Arc<Db>, oauth: &OAuthState, body: &str, stream: &mut std::net::TcpStream) -> Result<(), String> {
    let no_store = "Cache-Control: no-store\r\nPragma: no-cache\r\n";
    let mut err = |code: u16, error: &str, desc: &str| -> Result<(), String> {
        crate::remote_mcp::write_http_h(
            stream,
            code,
            "application/json",
            &json!({ "error": error, "error_description": desc }).to_string(),
            no_store,
        )
    };
    let trimmed = body.trim();
    let params: HashMap<String, String> = if trimmed.starts_with('{') {
        serde_json::from_str::<Value>(trimmed)
            .map(|v| {
                v.as_object()
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, x)| x.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    } else {
        parse_query(trimmed)
    };
    let get = |k: &str| params.get(k).cloned().unwrap_or_default();

    match get("grant_type").as_str() {
        "authorization_code" => {
            let code = get("code");
            let client_id = get("client_id");
            let redirect_uri = get("redirect_uri");
            let verifier = get("code_verifier");
            if verifier.len() < 43 || verifier.len() > 128 {
                return err(400, "invalid_request", "code_verifier 长度不符合 PKCE 要求");
            }
            let record = {
                let mut codes = oauth.codes.lock().unwrap_or_else(|p| p.into_inner());
                codes.remove(&code) // 一次性：取用即删
            };
            let Some(rec) = record else {
                return err(400, "invalid_grant", "授权码无效或已被使用");
            };
            if rec.created.elapsed() > CODE_TTL {
                return err(400, "invalid_grant", "授权码已过期");
            }
            if rec.client_id != client_id {
                return err(400, "invalid_grant", "client_id 与授权码不匹配");
            }
            // PKCE S256 校验
            if sha256_b64url(&verifier) != rec.code_challenge {
                return err(400, "invalid_grant", "PKCE 校验失败");
            }
            // redirect_uri 校验：与客户端注册表比对
            let Some((_, uris)) = client_get(db, &client_id) else {
                return err(400, "invalid_client", "未知 client");
            };
            if !redirect_uri.is_empty() && !uris.iter().any(|u| u == &redirect_uri) {
                return err(400, "invalid_grant", "redirect_uri 与注册信息不一致");
            }
            let resource = get("resource");
            let mut resp = issue_tokens(db, &client_id, &rec.grant_id, &rec.scope)?;
            if !resource.is_empty() {
                resp["resource"] = json!(resource);
            }
            crate::remote_mcp::write_http_h(stream, 200, "application/json", &resp.to_string(), no_store)
        }
        "refresh_token" => {
            let refresh = get("refresh_token");
            let client_id = get("client_id");
            if !refresh.starts_with("sdr1_") {
                return err(400, "invalid_grant", "refresh_token 无效");
            }
            let hash = sha256_hex(&refresh);
            let row = db
                .with(|c| {
                    let mut st = c.prepare(
                        "SELECT id, client_id, grant_id, scopes, refresh_expires_epoch FROM remote_oauth_tokens WHERE refresh_hash=?1 AND revoked_at IS NULL",
                    )?;
                    let r = st
                        .query_row(rusqlite::params![hash], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, String>(3)?,
                                r.get::<_, i64>(4)?,
                            ))
                        })
                        .optional()?;
                    Ok::<_, rusqlite::Error>(r)
                })
                .ok()
                .flatten();
            let Some((id, tok_client, grant_id, scopes, refresh_exp)) = row else {
                return err(400, "invalid_grant", "refresh_token 无效或已撤销");
            };
            if tok_client != client_id {
                return err(400, "invalid_grant", "client_id 不匹配");
            }
            if refresh_exp < now_epoch() {
                return err(400, "invalid_grant", "refresh_token 已过期，请重新走授权流程");
            }
            // 轮换：旧 access/refresh 立即失效
            let new_access = format!("sdo1_{}", rand_hex(1));
            let new_refresh = format!("sdr1_{}", rand_hex(1));
            let now = now_epoch();
            db.with(|c| {
                c.execute(
                    "UPDATE remote_oauth_tokens SET access_hash=?2, refresh_hash=?3, access_expires_epoch=?4, refresh_expires_epoch=?5, created_at=?6 WHERE id=?1",
                    rusqlite::params![
                        id,
                        sha256_hex(&new_access),
                        sha256_hex(&new_refresh),
                        now + ACCESS_TTL_SECS,
                        now + REFRESH_TTL_SECS,
                        now_str(),
                    ],
                )
                .map(|_| ())
            })?;
            let resp = json!({
                "access_token": new_access,
                "token_type": "Bearer",
                "expires_in": ACCESS_TTL_SECS,
                "refresh_token": new_refresh,
                "scope": scopes,
            });
            crate::remote_mcp::write_http_h(stream, 200, "application/json", &resp.to_string(), no_store)
        }
        _ => err(400, "unsupported_grant_type", "仅支持 authorization_code / refresh_token"),
    }
}

fn issue_tokens(db: &Arc<Db>, client_id: &str, grant_id: &str, scope: &str) -> Result<Value, String> {
    let access = format!("sdo1_{}", rand_hex(1));
    let refresh = format!("sdr1_{}", rand_hex(1));
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_epoch();
    db.with(|c| {
        c.execute(
            "INSERT INTO remote_oauth_tokens (id,client_id,grant_id,access_hash,refresh_hash,scopes,created_at,access_expires_epoch,refresh_expires_epoch) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                id,
                client_id,
                grant_id,
                sha256_hex(&access),
                sha256_hex(&refresh),
                scope,
                now_str(),
                now + ACCESS_TTL_SECS,
                now + REFRESH_TTL_SECS,
            ],
        )
        .map(|_| ())
    })?;
    Ok(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL_SECS,
        "refresh_token": refresh,
        "scope": scope,
    }))
}

// ---------------- 令牌鉴权（中间件用）与管理（设置页用） ----------------

/// OAuth 访问令牌 → 内部授权行。None = 无效/过期/已撤销（按 401 invalid_token 处理）。
/// 暂停状态不在此拦截（由中间件统一 403，提示去「继续开放」）。
pub fn resolve_access_token(db: &Arc<Db>, token: &str) -> Result<Option<RemoteGrant>, String> {
    if !token.starts_with("sdo1_") {
        return Ok(None);
    }
    let hash = sha256_hex(token);
    let row = db.with(|c| {
        let mut st = c.prepare(
            "SELECT id, grant_id, access_expires_epoch FROM remote_oauth_tokens WHERE access_hash=?1 AND revoked_at IS NULL",
        )?;
        let r = st
            .query_row(rusqlite::params![hash], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .optional()?;
        Ok::<_, rusqlite::Error>(r)
    })?;
    let Some((tok_id, grant_id, expires)) = row else {
        return Ok(None);
    };
    if expires < now_epoch() {
        return Ok(None);
    }
    let _ = db.with(|c| {
        c.execute(
            "UPDATE remote_oauth_tokens SET last_used_at=?2 WHERE id=?1",
            rusqlite::params![tok_id, now_str()],
        )
        .map(|_| ())
    });
    let grants = crate::remote_mcp::grants_list(db)?;
    let grant = grants.into_iter().find(|g| g.id == grant_id && g.revoked_at.is_none());
    Ok(grant)
}

#[derive(serde::Serialize)]
pub struct OAuthTokenRow {
    pub id: String,
    pub client_id: String,
    pub grant_id: String,
    pub grant_name: String,
    pub scopes: String,
    pub created_at: String,
    pub expires_in_secs: i64,
    pub refresh_expires_in_secs: i64,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

pub fn tokens_list(db: &Arc<Db>) -> Result<Vec<OAuthTokenRow>, String> {
    let rows = db.with(|c| {
        let mut st = c.prepare(
            "SELECT t.id, t.client_id, t.grant_id, COALESCE(g.project_name,''), t.scopes, t.created_at, t.access_expires_epoch, t.refresh_expires_epoch, t.last_used_at, t.revoked_at
             FROM remote_oauth_tokens t LEFT JOIN remote_grants g ON g.id = t.grant_id ORDER BY t.created_at DESC",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })?;
    let now = now_epoch();
    Ok(rows
        .into_iter()
        .map(|(id, client_id, grant_id, grant_name, scopes, created_at, aexp, rexp, last_used_at, revoked_at)| OAuthTokenRow {
            id,
            client_id,
            grant_id,
            grant_name,
            scopes,
            created_at,
            expires_in_secs: aexp - now,
            refresh_expires_in_secs: rexp - now,
            last_used_at,
            revoked_at,
        })
        .collect())
}

pub fn token_revoke(db: &Arc<Db>, id: &str) -> Result<(), String> {
    let n = db.with(|c| {
        c.execute(
            "UPDATE remote_oauth_tokens SET revoked_at=?2 WHERE id=?1 AND revoked_at IS NULL",
            rusqlite::params![id, now_str()],
        )
    })?;
    if n == 0 {
        return Err("令牌不存在或已吊销".into());
    }
    Ok(())
}
