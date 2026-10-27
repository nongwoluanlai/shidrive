//! 全局助手：独立于项目 / 上下文的长期对话伙伴。
//!
//! - 大脑二选一：现有 ACP Agent（独立长期会话，不写 agent_bindings、不注入使驾 MCP），
//!   或自定义 OpenAI 兼容接口（/chat/completions 流式）。
//! - 对话记录与长期记忆都落在使驾自己的库里（assistant_messages / assistant_memories），
//!   与客户端会话解耦：换 ACP 客户端或切到 API 后，新会话首轮会带上人设 + 记忆 + 最近对话回顾。
//! - 记忆点由模型在回复末尾用 `[[记忆: …]]` / `[[忘记: …]]` 标出，这里解析落库并从展示文本中剥掉；
//!   另有「整理记忆」「从最近对话提炼」两个摘要动作。
//! - 人设来自前端（跟随皮肤看板角色或用户自定义），每轮随请求传入，变化时自动重新注入。
//! - 回复分两层：正文（完整回答，进对话记录）+ 字幕（角色说出口的短句，显示在气泡里、开启语音时朗读）。
//!   API 模式用 `say` 工具（function calling）实时给出字幕，接口不支持工具时自动改用 `[[字幕: …]]` 标记；
//!   ACP 模式用标记。没给字幕时从正文第一句推导一条。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::db::Db;
use crate::manager::{AgentManager, ASSISTANT_CANCELLED};

const CFG_KEY: &str = "assistant.config";
const STATE_KEY: &str = "assistant.state";
/// 与 ACP 助手回合同名的事件：API 模式也按同样的形状推送增量，前端只处理一种。
const EVT_UPDATE: &str = "acp://assistant-update";
const MAX_MEMORY_LEN: usize = 200;
const MAX_MEMORIES: i64 = 300;
const MAX_MESSAGES: i64 = 2000;
/// 设定版本：规则有变化时让已有 ACP 长期会话重新注入设定
const PROMPT_REV: &str = "r14";
/// 单条字幕的长度上限（字）
const MAX_SUBTITLE_LEN: usize = 120;
/// API 模式一轮里最多几次工具往返（MCP 工具）
const MAX_TOOL_ROUNDS: usize = 8;
/// 操作电脑时的最多轮数（截图 → 点击 → 再截图…）
const MAX_DESKTOP_ROUNDS: usize = 30;
/// 上下文里最多保留几张截图 / 图片
const MAX_IMAGES: usize = 2;
/// 单个工具结果交回模型的长度上限（字）
const MAX_TOOL_RESULT: usize = 12000;

type DbState<'a> = State<'a, Arc<Db>>;
type AgentsState<'a> = State<'a, Arc<AgentManager>>;
type RtState<'a> = State<'a, Arc<AssistantRuntime>>;

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}
fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn cap_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n).collect::<String>() + "…" }
}

// ---------------------------------------------------------------- config / state

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ApiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: Option<f32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AssistantConfig {
    pub enabled: bool,
    /// "acp" | "api"
    pub provider: String,
    pub agent_type: String,
    /// ACP 模型配置值（适配器给出的原始值）；Null = 不改，用适配器默认
    pub model: Value,
    /// ACP 会话工作目录；空 = 使驾用户数据目录下的 assistant
    pub workspace: String,
    pub api: ApiConfig,
    /// "skin" = 跟随皮肤看板角色；"custom" = 自定义
    pub persona_mode: String,
    pub custom_name: String,
    pub custom_persona: String,
    pub auto_memory: bool,
    /// 每轮附带的长期记忆条数上限
    pub memory_limit: usize,
    /// API 模式附带的历史条数；ACP 新会话首轮的「最近对话回顾」条数
    pub history_turns: usize,
    /// 附带使驾当前状态（项目 / 上下文 / 页面）
    pub share_app_state: bool,
    /// 气泡自动收起秒数，0 = 不自动收起
    pub bubble_seconds: u32,
    /// 主动关怀：不忙时低频地主动搭话（深夜提醒、待办、今天过得怎样…）
    pub care_enabled: bool,
    /// 两次主动关怀的最短间隔（分钟）
    pub care_interval_minutes: u32,
    /// 几点以后算深夜（0~23）
    pub care_late_hour: u32,
    /// 关怀说完后进入连续语音对话（需开启语音输入）
    pub care_voice: bool,
    /// 给助手开放使驾本地 MCP（上下文 / 工作流 / 自己的定时等全部工具）：ACP 注入 MCP，API 走工具调用
    pub mcp_tools: bool,
    /// 电脑操作（仅 API 模式）：终端命令、打开程序、窗口切换、截图 / 读图、鼠标键盘
    pub desktop_tools: bool,
    /// 执行命令 / 打开程序 / 鼠标键盘前先弹窗确认
    pub desktop_confirm: bool,
    /// 截图时临时隐藏使驾窗口与桌宠人物（截完立即恢复），免得挡住画面
    pub desktop_hide: bool,
    /// 唤醒时使驾不在前台：在屏幕右下角浮出桌宠人物交互（提问可附上当前屏幕）
    pub pet_mode: bool,
    /// 模型支持读图：截图 / 图片按 OpenAI image_url（base64）发给模型
    pub vision: bool,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "acp".into(),
            agent_type: "codex".into(),
            model: Value::Null,
            workspace: String::new(),
            api: ApiConfig::default(),
            persona_mode: "skin".into(),
            custom_name: String::new(),
            custom_persona: String::new(),
            auto_memory: true,
            memory_limit: 40,
            history_turns: 12,
            share_app_state: true,
            bubble_seconds: 30,
            care_enabled: false,
            care_interval_minutes: 120,
            care_late_hour: 23,
            care_voice: true,
            mcp_tools: true,
            desktop_tools: false,
            desktop_confirm: false,
            desktop_hide: true,
            pet_mode: true,
            vision: true,
        }
    }
}

impl AssistantConfig {
    fn normalized(mut self) -> Self {
        if self.provider != "api" { self.provider = "acp".into(); }
        if self.persona_mode != "custom" { self.persona_mode = "skin".into(); }
        self.memory_limit = self.memory_limit.clamp(0, 200);
        self.history_turns = self.history_turns.clamp(0, 60);
        self.bubble_seconds = self.bubble_seconds.min(600);
        self.care_interval_minutes = self.care_interval_minutes.clamp(30, 600);
        self.care_late_hour = self.care_late_hour.min(23);
        self.api.base_url = self.api.base_url.trim().to_string();
        self.api.model = self.api.model.trim().to_string();
        self.api.temperature = self.api.temperature.map(|t| t.clamp(0.0, 2.0));
        self.workspace = self.workspace.trim().to_string();
        self.custom_name = cap_chars(self.custom_name.trim(), 40);
        self.custom_persona = cap_chars(self.custom_persona.trim(), 4000);
        self
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct AssistantState {
    /// ACP：长期会话 id；API：本地会话标记（api-<uuid>），仅用于「新会话」语义
    pub session_id: String,
    pub session_agent: String,
    pub session_provider: String,
    pub session_cwd: String,
    pub session_created_at: String,
    /// 已向当前会话注入的「会话|人设|记忆版本」；不一致时下一轮重新注入设定
    pub primed_key: String,
    pub memory_version: u64,
    /// 已应用到当前会话的模型值（JSON 文本）
    pub applied_model: String,
    /// 已经打过招呼 / 聊过天的会话 id（新会话首次出现时前端据此问候）
    pub greeted_session: String,
}

fn load_config(db: &Db) -> AssistantConfig {
    db.get_setting(CFG_KEY).ok().flatten()
        .and_then(|s| serde_json::from_str::<AssistantConfig>(&s).ok())
        .unwrap_or_default()
        .normalized()
}
fn save_config(db: &Db, cfg: &AssistantConfig) -> Result<(), String> {
    db.set_setting(CFG_KEY, &serde_json::to_string(cfg).map_err(|e| e.to_string())?)
}
fn load_state(db: &Db) -> AssistantState {
    db.get_setting(STATE_KEY).ok().flatten()
        .and_then(|s| serde_json::from_str::<AssistantState>(&s).ok())
        .unwrap_or_default()
}
fn save_state(db: &Db, st: &AssistantState) -> Result<(), String> {
    db.set_setting(STATE_KEY, &serde_json::to_string(st).map_err(|e| e.to_string())?)
}

/// 人设（前端按皮肤 / 自定义解析后传入）
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Persona {
    pub id: String,
    pub name: String,
    pub persona: String,
    pub greeting: String,
    /// "zh" | "en"
    pub locale: String,
}

impl Persona {
    fn key(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(self.name.as_bytes());
        h.update([0]);
        h.update(self.persona.as_bytes());
        h.update([0]);
        h.update(self.locale.as_bytes());
        h.finalize().iter().take(6).map(|b| format!("{b:02x}")).collect()
    }
    fn display_name(&self) -> &str {
        if self.name.trim().is_empty() { "使驾助手" } else { self.name.trim() }
    }
}

// ---------------------------------------------------------------- storage

#[derive(Serialize, Clone, Debug)]
pub struct AssistantMessage {
    pub id: String,
    /// "user" | "assistant"
    pub role: String,
    pub content: String,
    pub provider: String,
    pub created_at: String,
    /// 回复被用户停止（内容可能不完整）
    pub interrupted: bool,
    /// 开场问候（不是对用户消息的回复）
    pub greeting: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct AssistantMemory {
    pub id: String,
    pub content: String,
    /// "auto" | "manual" | "summary"
    pub source: String,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
}

pub const TABLES_SQL: &str = "
CREATE TABLE IF NOT EXISTS assistant_messages (
    id TEXT PRIMARY KEY,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    provider TEXT NOT NULL DEFAULT '',
    flags TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_assistant_messages_time ON assistant_messages(created_at);
CREATE TABLE IF NOT EXISTS assistant_memories (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'auto',
    pinned INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
";

fn row_message(r: &rusqlite::Row) -> rusqlite::Result<AssistantMessage> {
    let flags: String = r.get(4)?;
    Ok(AssistantMessage {
        id: r.get(0)?, role: r.get(1)?, content: r.get(2)?, provider: r.get(3)?, created_at: r.get(5)?,
        interrupted: flags.contains("interrupted"), greeting: flags.contains("greeting"),
    })
}
fn row_memory(r: &rusqlite::Row) -> rusqlite::Result<AssistantMemory> {
    Ok(AssistantMemory {
        id: r.get(0)?, content: r.get(1)?, source: r.get(2)?, pinned: r.get::<_, i64>(3)? != 0,
        created_at: r.get(4)?, updated_at: r.get(5)?,
    })
}

fn insert_message(db: &Db, role: &str, content: &str, provider: &str, flags: &[&str]) -> Result<AssistantMessage, String> {
    let id = uuid();
    let at = now();
    let flags = flags.join(",");
    db.with(|c| {
        c.execute(
            "INSERT INTO assistant_messages (id,role,content,provider,flags,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![id, role, content, provider, flags, at],
        )?;
        // 只保留最近 MAX_MESSAGES 条（记忆另存，不受影响）
        c.execute(
            "DELETE FROM assistant_messages WHERE id IN (SELECT id FROM assistant_messages ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?1)",
            params![MAX_MESSAGES],
        )?;
        Ok(())
    })?;
    Ok(AssistantMessage {
        id, role: role.into(), content: content.into(), provider: provider.into(), created_at: at,
        interrupted: flags.contains("interrupted"), greeting: flags.contains("greeting"),
    })
}

/// 最近 limit 条，按时间正序
pub fn recent_messages(db: &Db, limit: usize) -> Result<Vec<AssistantMessage>, String> {
    db.with(|c| {
        let mut st = c.prepare(
            "SELECT id,role,content,provider,flags,created_at FROM assistant_messages ORDER BY created_at DESC, rowid DESC LIMIT ?1",
        )?;
        let mut rows = st.query_map(params![limit as i64], row_message)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    })
}

pub fn list_memories(db: &Db) -> Result<Vec<AssistantMemory>, String> {
    db.with(|c| {
        let mut st = c.prepare(
            "SELECT id,content,source,pinned,created_at,updated_at FROM assistant_memories ORDER BY pinned DESC, updated_at DESC, rowid DESC",
        )?;
        let rows = st.query_map([], row_memory)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

fn norm_memory(s: &str) -> String {
    let s = s.trim().trim_start_matches(['-', '*', '•', '·']).trim();
    cap_chars(&s.split_whitespace().collect::<Vec<_>>().join(" "), MAX_MEMORY_LEN)
}

/// 新增记忆（去重：内容相同或互相包含则跳过 / 以更长者为准）。返回是否真正写入。
fn add_memory(db: &Db, content: &str, source: &str, pinned: bool) -> Result<bool, String> {
    let content = norm_memory(content);
    if content.chars().count() < 2 { return Ok(false); }
    let lower = content.to_lowercase();
    let existing = list_memories(db)?;
    for m in &existing {
        let ml = m.content.to_lowercase();
        if ml == lower || ml.contains(&lower) { return Ok(false); }
        if lower.contains(&ml) && !m.pinned && m.source != "manual" {
            // 新的更完整：就地更新旧条目
            let at = now();
            db.with(|c| c.execute("UPDATE assistant_memories SET content=?2, updated_at=?3 WHERE id=?1", params![m.id, content, at]))?;
            return Ok(true);
        }
    }
    let at = now();
    db.with(|c| {
        c.execute(
            "INSERT INTO assistant_memories (id,content,source,pinned,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?5)",
            params![uuid(), content, source, pinned as i64, at],
        )?;
        // 超出上限时淘汰最旧的自动记忆
        c.execute(
            "DELETE FROM assistant_memories WHERE id IN (SELECT id FROM assistant_memories WHERE pinned=0 AND source<>'manual' ORDER BY updated_at DESC LIMIT -1 OFFSET ?1)",
            params![MAX_MEMORIES],
        )?;
        Ok(())
    })?;
    Ok(true)
}

/// 「忘记」：删除包含关键词的非置顶记忆，返回删除条数
fn forget_memories(db: &Db, keyword: &str) -> Result<usize, String> {
    let k = keyword.trim();
    if k.chars().count() < 2 { return Ok(0); }
    let like = format!("%{}%", k.replace('%', "\\%").replace('_', "\\_"));
    db.with(|c| c.execute("DELETE FROM assistant_memories WHERE pinned=0 AND content LIKE ?1 ESCAPE '\\'", params![like]))
}

fn bump_memory_version(db: &Db, keep_primed: bool) {
    let mut st = load_state(db);
    let old = st.memory_version;
    st.memory_version += 1;
    // 助手自己写下的记忆它已经知道：保持「已注入」状态，不必下一轮整份重发
    if keep_primed {
        let suffix = format!("|{old}");
        if st.primed_key.ends_with(&suffix) {
            st.primed_key = format!("{}|{}", &st.primed_key[..st.primed_key.len() - suffix.len()], st.memory_version);
        }
    }
    let _ = save_state(db, &st);
}

// ---------------------------------------------------------------- directives

/// 从回复中剥出 `[[记忆: …]]` / `[[忘记: …]]` 指令，返回 (展示文本, 记忆, 忘记)
pub fn extract_directives(text: &str) -> (String, Vec<String>, Vec<String>) {
    let re = regex::Regex::new(r"(?i)\[\[\s*(记忆|记住|memory|remember|忘记|forget)\s*[:：]\s*([^\[\]]+?)\s*\]\]").unwrap();
    let mut remember = Vec::new();
    let mut forget = Vec::new();
    for cap in re.captures_iter(text) {
        let kind = cap[1].to_lowercase();
        let body = cap[2].trim().to_string();
        if body.is_empty() { continue; }
        if matches!(kind.as_str(), "忘记" | "forget") { forget.push(body) } else { remember.push(body) }
    }
    let cleaned = re.replace_all(text, "").to_string();
    // 去掉指令留下的空行
    let mut lines: Vec<&str> = cleaned.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) { lines.pop(); }
    let mut out = lines.join("\n");
    while out.contains("\n\n\n") { out = out.replace("\n\n\n", "\n\n"); }
    (out.trim().to_string(), remember, forget)
}

/// 剥出回合控制标记：`[[结束对话]]`（连续语音对话由助手判断收尾）、`[[跳过]]`（主动关怀时选择不打扰）。
/// 返回 (文本, 结束对话, 跳过)
pub fn extract_flags(text: &str) -> (String, bool, bool) {
    let re = regex::Regex::new(r"(?i)[ \t]*\[\[\s*(结束对话|结束|end[ _-]?(conversation|chat)?|跳过|skip)\s*\]\][ \t]*").unwrap();
    let mut end = false;
    let mut skip = false;
    for cap in re.captures_iter(text) {
        let k = cap[1].to_lowercase();
        if k == "跳过" || k == "skip" { skip = true } else { end = true }
    }
    if !end && !skip { return (text.to_string(), false, false); }
    let out = re.replace_all(text, "").to_string();
    let mut lines: Vec<&str> = out.lines().map(|l| l.trim_end()).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) { lines.pop(); }
    (lines.join("\n").trim().to_string(), end, skip)
}

/// 解析「- 条目」列表（整理 / 提炼记忆的输出）
pub fn parse_bullets(text: &str) -> Vec<String> {
    let (text, _, _) = extract_directives(text);
    let (text, _) = extract_subtitles(&text);
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("- ") || l.starts_with("* ") || l.starts_with("• "))
        .map(norm_memory)
        .filter(|l| l.chars().count() >= 2 && l != "无" && !l.eq_ignore_ascii_case("none"))
        .collect()
}

// ---------------------------------------------------------------- subtitles（字幕层）

/// 字幕的产生方式
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubMode {
    /// API：`say` 工具（function calling）
    Tool,
    /// `[[字幕: …]]` 标记（ACP / 不支持工具的接口）
    Tag,
    /// 不要字幕（整理记忆等后台任务）
    Off,
}

/// 一条字幕：text = 显示（界面语言），speech = 配音语言版本（角色语音与显示语言不同时）
#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Subtitle {
    pub text: String,
    pub speech: String,
    /// 模型没给字幕，从正文推导的
    pub derived: bool,
}

fn clean_sub(s: &str) -> String {
    let t: String = s.replace('[', "［").replace(']', "］").split_whitespace().collect::<Vec<_>>().join(" ");
    cap_chars(t.trim(), MAX_SUBTITLE_LEN)
}

fn sub_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 字幕里允许成对的单层方括号（「看看 [README] 吧」），否则整条匹配失败、字幕丢失
        regex::Regex::new(r"(?i)[ \t]*\[\[\s*(?:字幕|subtitle|say)\s*[:：]\s*((?:[^\[\]]|\[[^\[\]]*\])*?)\s*\]\](?:[ \t]*\[\[\s*(?:ja|en|ko|yue|zh|speech)\s*[:：]\s*((?:[^\[\]]|\[[^\[\]]*\])*?)\s*\]\])?[ \t]*\n?").unwrap()
    })
}

/// 剥出 `[[字幕: …]]`（可紧跟一个配音标记 `[[ja: …]]`），返回 (正文, 字幕)
pub fn extract_subtitles(text: &str) -> (String, Vec<Subtitle>) {
    let re = sub_re();
    let mut subs = Vec::new();
    for cap in re.captures_iter(text) {
        let t = clean_sub(&cap[1]);
        if t.is_empty() { continue; }
        let speech = cap.get(2).map(|m| clean_sub(m.as_str())).unwrap_or_default();
        subs.push(Subtitle { text: t, speech, derived: false });
    }
    if subs.is_empty() && !re.is_match(text) { return (text.to_string(), subs); }
    let out = re.replace_all(text, "").to_string();
    let mut out = out.trim().to_string();
    while out.contains("\n\n\n") { out = out.replace("\n\n\n", "\n\n"); }
    (out, subs)
}

/// 工具给出的字幕编码成标记拼在回复前面，和标记协议走同一套解析
fn encode_subtitles(subs: &[Subtitle]) -> String {
    subs.iter()
        .map(|s| if s.speech.is_empty() { format!("[[字幕: {}]]\n", s.text) } else { format!("[[字幕: {}]][[speech: {}]]\n", s.text, s.speech) })
        .collect()
}

/// 模型没给字幕：从正文开头推导几句（去掉代码块与 Markdown 记号）。
/// 按整句取，不在句子中间截断：最多 3 句、合计约 80 字（第一句总会保留，超长的由 clean_sub 兜底）
pub fn derive_subtitles(body: &str) -> Vec<String> {
    static CODE: OnceLock<regex::Regex> = OnceLock::new();
    static LINK: OnceLock<regex::Regex> = OnceLock::new();
    static MARK: OnceLock<regex::Regex> = OnceLock::new();
    let code = CODE.get_or_init(|| regex::Regex::new(r"(?s)```.*?(```|$)").unwrap());
    let link = LINK.get_or_init(|| regex::Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").unwrap());
    let mark = MARK.get_or_init(|| regex::Regex::new(r"[*_`~]+|https?://\S+").unwrap());
    static LIST: OnceLock<regex::Regex> = OnceLock::new();
    let list = LIST.get_or_init(|| regex::Regex::new(r"^\d{1,2}(?:[.)]\s+|、\s*)").unwrap());
    const ENDS: &str = "。！？!?…";
    const MAX_SENTS: usize = 3;
    const MAX_TOTAL: usize = 80;
    let s = code.replace_all(body, "\n\n");
    // 第一段（遇到空行 / 表格就停）里的文字
    let mut para = String::new();
    for l in s.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('|') {
            if para.is_empty() { continue; }
            break;
        }
        let l = l.trim_start_matches(|c: char| "#>-*+".contains(c) || c.is_whitespace());
        let l = list.replace(l, "");
        if !l.chars().any(char::is_alphanumeric) { continue; }
        let l = link.replace_all(&l, "$1");
        let l = mark.replace_all(&l, "");
        if !para.is_empty() { para.push(if para.ends_with(|c: char| ENDS.contains(c) || "，,：:；;".contains(c)) { ' ' } else { '，' }); }
        para.push_str(l.trim());
    }
    // 切句：句末标点（连着的一并带上）；太短的开头（「好的。」）和下一句合并
    let chars: Vec<char> = para.chars().collect();
    let mut sents: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        cur.push(chars[i]);
        if ENDS.contains(chars[i]) {
            while i + 1 < chars.len() && ENDS.contains(chars[i + 1]) { i += 1; cur.push(chars[i]); }
            if cur.trim().chars().count() >= 7 { sents.push(cur.trim().to_string()); cur.clear(); }
        }
        i += 1;
    }
    if !cur.trim().is_empty() { sents.push(cur.trim().to_string()); }
    let mut out = Vec::new();
    let mut total = 0;
    for x in sents {
        let n = x.chars().count();
        if !out.is_empty() && (out.len() >= MAX_SENTS || total + n > MAX_TOTAL) { break; }
        total += n;
        let c = clean_sub(&x);
        if !c.is_empty() { out.push(c); }
    }
    out
}

fn tools_key(api: &ApiConfig) -> String {
    format!("{}|{}", api.base_url.trim().trim_end_matches('/'), api.model.trim())
}

/// 不支持工具调用的接口（运行期记住，重启后再试）
fn no_tools() -> &'static Mutex<HashSet<String>> {
    static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 接口明确拒绝了 tools 参数
/// 接口不支持图片输入（记住后不再发图）
fn no_vision() -> &'static Mutex<HashSet<String>> {
    static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 带图请求被拒（多为模型不支持图片输入）
fn vision_rejected(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    ["接口返回 http 400", "接口返回 http 413", "接口返回 http 415", "接口返回 http 422"].iter().any(|p| e.starts_with(p))
}

/// 消息里的图片只保留最近 `keep` 张（截图很占上下文），其余换成文字说明；keep = 0 全部去掉
fn prune_images(messages: &mut [Value], keep: usize) {
    let mut seen = 0;
    for m in messages.iter_mut().rev() {
        let Some(parts) = m.get_mut("content").and_then(Value::as_array_mut) else { continue };
        for p in parts.iter_mut().rev() {
            if p["type"] != "image_url" { continue; }
            seen += 1;
            if seen > keep { *p = json!({ "type": "text", "text": "[较早的截图 / 图片已省略]" }); }
        }
    }
}

fn has_images(messages: &[Value]) -> bool {
    messages.iter().any(|m| m["content"].as_array().is_some_and(|a| a.iter().any(|p| p["type"] == "image_url")))
}

/// 等待用户确认的电脑操作：request_id → 回传选项
type PermTx = std::sync::mpsc::Sender<String>;
fn pending_perms() -> &'static Mutex<HashMap<String, PermTx>> {
    static S: OnceLock<Mutex<HashMap<String, PermTx>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}
/// 「本次启动期间始终允许」的工具
static ALWAYS_ALLOW: Mutex<Vec<String>> = Mutex::new(Vec::new());
pub const PERM_PREFIX: &str = "assistant:";

/// 权限弹窗的应答（manager.respond_permission 按 request_id 前缀 `assistant:` 转到这里）
pub fn resolve_permission(request_id: &str, option_id: &str) -> Result<(), String> {
    match pending_perms().lock().unwrap().remove(request_id) {
        Some(tx) => { let _ = tx.send(option_id.to_string()); Ok(()) }
        None => Err("请求已失效".into()),
    }
}

/// 电脑操作的运行选项
#[derive(Clone)]
struct DeskCfg {
    confirm: bool,
    hide: bool,
    vision: bool,
}

fn tools_rejected(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    ["接口返回 http 400", "接口返回 http 404", "接口返回 http 405", "接口返回 http 422", "接口返回 http 501"].iter().any(|p| e.starts_with(p))
        && (e.contains("tool") || e.contains("function"))
}

// ---------------------------------------------------------------- prompt building

fn lang_line(p: &Persona) -> &'static str {
    if p.locale == "en" { "Reply in English unless the user writes in another language." } else { "默认用简体中文回复（用户用其他语言时跟随用户）。" }
}

fn memory_block(mems: &[AssistantMemory], limit: usize) -> String {
    if mems.is_empty() || limit == 0 { return "（暂无）".into(); }
    let mut out = String::new();
    let mut budget = 4000usize;
    for m in mems.iter().take(limit) {
        let line = format!("- {}{}\n", if m.pinned { "【重要】" } else { "" }, m.content);
        let n = line.chars().count();
        if n > budget { break; }
        budget -= n;
        out.push_str(&line);
    }
    out.trim_end().to_string()
}

fn recap_block(history: &[AssistantMessage], name: &str) -> String {
    let mut lines = Vec::new();
    let mut budget = 3000usize;
    for m in history.iter().rev() {
        let who = if m.role == "user" { "用户" } else { name };
        let line = format!("{who}：{}", cap_chars(&m.content.replace('\n', " "), 300));
        let n = line.chars().count();
        if n > budget { break; }
        budget -= n;
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}

/// 设定块：人设 + 规则 + 长期记忆（ACP 首轮 / API 每轮的 system）
pub fn system_prompt(p: &Persona, mems: &[AssistantMemory], cfg: &AssistantConfig, sub: SubMode) -> String {
    let name = p.display_name();
    let persona = if p.persona.trim().is_empty() {
        "简洁、友好、靠谱的开发伙伴；先给结论，少说套话。".to_string()
    } else {
        p.persona.trim().to_string()
    };
    let mut s = format!(
        "[使驾全局助手 · 设定]\n你是「{name}」，在使驾（ShiDrive，桌面 AI 开发工作台）里作为全局助手陪伴用户，可以闲聊，也可以回答开发与使用问题。\n\
人设与说话风格：\n{persona}\n\n规则：\n\
- {}\n\
{}\
- 始终保持人设口吻，但信息要准确；不确定就直说，不编造。\n\
- 这里是独立的助手会话，不是项目里的编程会话；除非用户明确要求，不要主动改动文件或执行命令。\n",
        lang_line(p),
        layer_rule(sub)
    );
    if cfg.auto_memory {
        s.push_str("- 对话中出现值得长期记住的信息（用户的称呼、偏好、身份、长期目标、约定、重要事实）时，在回复最后单独一行写 [[记忆: 一句话摘要]]，可多条、每条一行；没有就不写。普通闲聊和一次性任务细节不要记。用户要求忘掉某事时写 [[忘记: 关键词]]。这些标记不会展示给用户。\n");
    }
    if cfg.mcp_tools && sub != SubMode::Off {
        s.push_str(MCP_RULE);
    }
    if desktop_on(cfg, sub) {
        s.push_str(DESKTOP_RULE);
        if !cfg.vision { s.push_str("- 当前模型没有开启读图，看不到屏幕：界面操作只能靠窗口列表、快捷键和命令完成，做不到时如实告诉用户。\n"); }
    }
    s.push_str("\n[长期记忆]（跨会话、跨客户端保存，按需参考，不必复述）\n");
    s.push_str(&memory_block(mems, cfg.memory_limit));
    s
}

const TWO_LAYER_RULE: &str = "- 你的回复分两层：①「正文」是完整回答，存进对话记录、显示在对话面板里，可用 Markdown / 代码块，详略按需要；\
②「字幕」是你这个角色此刻说出口的话，显示在桌面角色旁的气泡里（开启语音时会被朗读）。字幕要口语、简短：每条一句、不超过 30 字，一般 1~3 条，\
说出你想对用户讲的重点或情绪，不放代码、路径、链接和列表；不要在正文里重复字幕。\n";

fn layer_rule(sub: SubMode) -> String {
    match sub {
        SubMode::Tool => format!("{TWO_LAYER_RULE}- 用 say 工具说字幕：先调用 say 说第一句（越快越好），再写正文。闲聊、打招呼这类简短回复可以只调用 say、不写正文。\n"),
        SubMode::Tag => format!("{TWO_LAYER_RULE}- 字幕写成单独一行的 [[字幕: …]]，放在回复最开头（可写多条，每条一行），正文接在后面。闲聊、打招呼这类简短回复可以只写字幕。字幕标记不会原样显示。\n"),
        SubMode::Off => "- 回复会显示在桌面角色旁的对话气泡里：像聊天一样自然简短，一般 1~4 句；用户需要时再展开，可用 Markdown。\n".into(),
    }
}

/// ACP 每轮附带的格式提醒（设定只在首轮注入，长对话里容易忘）
const TAG_REMINDER_FULL: &str = "[输出格式] 要说给用户听的话逐句全部写成 [[字幕: …]]（一句一行、不限条数、完整讲完），正文只放需要看的内容。";
const TAG_REMINDER: &str = "[输出格式] 先写 1~3 行 [[字幕: …]]（你说出口的简短台词），再写正文；简短闲聊可以只写字幕。";

fn state_line(app_state: Option<&str>, cfg: &AssistantConfig) -> String {
    let t = chrono::Local::now().format("%Y-%m-%d %H:%M %a").to_string();
    match app_state.map(str::trim).filter(|s| !s.is_empty() && cfg.share_app_state) {
        Some(st) => format!("[当前状态] 时间 {t} · {}", cap_chars(st, 400)),
        None => format!("[当前状态] 时间 {t}"),
    }
}

/// 语音输出提示（前端按当前语音设置传入）：开启时约束模型用短句、尽快出第一句；
/// `layer` = 角色语音与显示语言不同（如日语配音），每句后附 `[[ja: …]]` 供朗读。
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SpeechHint {
    pub enabled: bool,
    pub lang: String,
    pub layer: bool,
    /// 连续语音对话中：说完会自动继续听，由助手判断何时收尾
    pub conversation: bool,
    /// 完整朗读：正文里可读的句子也会读出；配音模式下正文没法读，要求把要说的话全部写成字幕
    pub full: bool,
}

fn speech_lang_name(lang: &str) -> &'static str {
    match lang {
        "ja" => "日语",
        "en" => "英语",
        "ko" => "韩语",
        "yue" => "粤语",
        _ => "中文",
    }
}

fn valid_speech_lang(lang: &str) -> bool {
    matches!(lang, "ja" | "en" | "ko" | "yue" | "zh")
}

const CONVERSATION_RULE: &str = "[连续语音对话中] 用户正用语音和你连续聊天：你说完后会自动接着听用户说话。像面对面聊天那样自然接话，别长篇大论。\
请根据对话氛围判断要不要结束：用户道别、说「好了 / 没事了 / 先这样 / 谢谢」、明显要去忙，或话题已经自然收尾、再追问会显得啰嗦时，\
用一句简短的话收尾，并在回复最后单独一行写 [[结束对话]]（之后就不再听）；还想继续聊就不写，结尾可以自然留个话头。这个标记不会展示给用户，也不要提起它。";

pub fn speech_rule(h: Option<&SpeechHint>, sub: SubMode) -> Option<String> {
    let h = h?;
    let voice = match sub {
        SubMode::Off => voice_rule(h),
        _ => subtitle_voice_rule(h, sub),
    };
    if !h.conversation { return voice; }
    Some(match voice {
        Some(v) => format!("{v}\n{CONVERSATION_RULE}"),
        None => CONVERSATION_RULE.to_string(),
    })
}

/// 配音语言（角色语音与显示语言不同时）
fn speech_layer_lang(h: Option<&SpeechHint>) -> Option<String> {
    let h = h?;
    (h.enabled && h.layer && valid_speech_lang(&h.lang)).then(|| h.lang.clone())
}

/// 两层输出下的语音规则：只朗读字幕，正文不朗读
fn subtitle_voice_rule(h: &SpeechHint, sub: SubMode) -> Option<String> {
    if !h.enabled { return None; }
    if let Some(tag) = speech_layer_lang(Some(h)) {
        let ln = speech_lang_name(&tag);
        let how = if sub == SubMode::Tool {
            format!("每次调用 say 都要在 speech 参数里给出这句字幕的{ln}版本")
        } else {
            format!("每条字幕后紧跟它的{ln}版本，格式 [[字幕: 显示的字幕]][[{tag}: …]]")
        };
        let full = if h.full {
            "\n要讲给用户听的内容（笑话、故事、解释、建议）必须逐句全部写成字幕：一句一条、按顺序、不限条数（不受 1~3 条的限制），完整讲完，不要只写一句概括；\
正文只放代码、列表、表格等需要看的内容，没有就不写正文。"
        } else { "" };
        return Some(format!(
            "[语音输出已开启 · {ln}配音] 字幕会用角色的{ln}原声朗读（正文不朗读）。{how}：只放{ln}，口吻符合角色，数字、英文缩写写成{ln}读法；\
每条字幕不超过 20 字，第一条尽快给出。这些标记不会显示给用户，也不要解释它们。{full}"
        ));
    }
    let zh = if h.lang == "en" { "" } else { "；尽量用中文说法，少夹英文单词和缩写（中文语音容易读不出来）" };
    if h.full {
        return Some(format!("[语音输出已开启 · 完整朗读] 先读字幕，再把正文从头到尾逐句读出（代码块、表格、链接和路径不读）。\
所以字幕只说一句开场或态度（不要概括正文），要讲的内容（笑话、故事、解释、步骤）直接在正文里完整讲完；正文用适合朗读的口语短句，\
少用 Markdown 记号、列表、表格和表情符号，不要在正文里重复字幕{zh}。简短闲聊可以只写字幕。"));
    }
    Some(format!("[语音输出已开启] 字幕会被朗读（正文不朗读）：字幕用自然口语的短句（每条不超过 20 字，英文不超过 12 个词），第一条尽快给出；字幕里不要放 Markdown、表情符号和代码{zh}。"))
}

fn voice_rule(h: &SpeechHint) -> Option<String> {
    if !h.enabled { return None; }
    if h.layer && valid_speech_lang(&h.lang) {
        let ln = speech_lang_name(&h.lang);
        let tag = &h.lang;
        return Some(format!(
            "[语音输出已开启 · {ln}配音] 你的回复会用角色的{ln}原声逐句朗读，文字照常显示。要求：\
1) 用口语短句，每句不超过 20 字（长句拆成几句，每句都配标记），第一句尽快给出；\
2) 每写完一句，紧跟着写出这句的{ln}版本，格式 [[{tag}: …]]，只放{ln}，口吻符合角色，数字、英文缩写写成{ln}读法；\
3) 代码、命令、路径、链接等不需要朗读的内容不加标记，少用列表和表格；\
4) 这些标记不会显示给用户，也不要解释它们。"
        ));
    }
    Some("[语音输出已开启] 你的回复会被逐句朗读：用自然口语的短句（每句不超过 20 字，英文不超过 12 个词；长句拆成几句），第一句尽快给出；少用 Markdown、列表、表格和表情符号；需要给代码时先用一句话说明，代码单独放在代码块里（代码不会被朗读）。".to_string())
}

/// 去掉配音标记 `[[ja: …]]`（存库与展示只保留正文）
pub fn strip_speech_tags(text: &str) -> String {
    let re = regex::Regex::new(r"(?i)[ \t]*\[\[\s*(ja|en|ko|yue|zh|speech)\s*[:：][^\[\]]*\]\]").unwrap();
    let out = re.replace_all(text, "").to_string();
    let mut lines: Vec<&str> = out.lines().map(|l| l.trim_end()).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) { lines.pop(); }
    let mut out = lines.join("\n");
    while out.contains("\n\n\n") { out = out.replace("\n\n\n", "\n\n"); }
    out.trim().to_string()
}

/// 桌宠截屏提问附带的说明（图片本身另附）
fn screen_note(fg: &str) -> String {
    let fg = if fg.is_empty() { String::new() } else { format!("，用户当时在看的窗口：{fg}") };
    format!("[用户从桌面助手发起提问，附上了当前屏幕截图{fg}。问题里的「这个 / 这里 / 屏幕上」指截图里的内容；截图中不含使驾自己的窗口]")
}

/// 一轮要发出的内容
enum Turn<'a> {
    /// 用户消息
    User(&'a str),
    /// 系统事件（问候等），不入库
    Event(String),
}

// ---------------------------------------------------------------- runtime

pub struct AssistantRuntime {
    data_dir: PathBuf,
    /// 正在进行的回合 id（同一时间只允许一轮）
    busy: Mutex<Option<String>>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl AssistantRuntime {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir, busy: Mutex::new(None), cancels: Mutex::new(HashMap::new()) }
    }
    fn default_workspace(&self) -> PathBuf {
        self.data_dir.join("assistant")
    }
    fn workspace(&self, cfg: &AssistantConfig) -> String {
        let p = if cfg.workspace.is_empty() || !std::path::Path::new(&cfg.workspace).is_dir() {
            self.default_workspace()
        } else {
            PathBuf::from(&cfg.workspace)
        };
        let _ = std::fs::create_dir_all(&p);
        p.to_string_lossy().to_string()
    }
}

struct BusyGuard<'a> {
    rt: &'a AssistantRuntime,
    turn: String,
}
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        *self.rt.busy.lock().unwrap() = None;
        self.rt.cancels.lock().unwrap().remove(&self.turn);
    }
}
fn acquire<'a>(rt: &'a AssistantRuntime, turn: &str) -> Result<(BusyGuard<'a>, Arc<AtomicBool>), String> {
    let mut b = rt.busy.lock().unwrap();
    if b.is_some() { return Err("助手正在回复中，请稍候或先停止".into()); }
    *b = Some(turn.to_string());
    let flag = Arc::new(AtomicBool::new(false));
    rt.cancels.lock().unwrap().insert(turn.to_string(), flag.clone());
    Ok((BusyGuard { rt, turn: turn.to_string() }, flag))
}

/// 确保 ACP 长期会话可用：没有 / 换了客户端 / 恢复失败时新建；返回 (sid, cwd)
async fn ensure_acp_session(db: &Db, agents: &AgentManager, rt: &AssistantRuntime, cfg: &AssistantConfig) -> Result<(String, String), String> {
    let mut st = load_state(db);
    let cwd = rt.workspace(cfg);
    let reuse = st.session_provider == "acp" && st.session_agent == cfg.agent_type && !st.session_id.is_empty();
    if reuse {
        let cwd0 = if st.session_cwd.is_empty() { cwd.clone() } else { st.session_cwd.clone() };
        match agents.assistant_session_ensure(&cfg.agent_type, &st.session_id, &cwd0, cfg.mcp_tools).await {
            Ok(_) => {
                apply_model(db, agents, cfg, &mut st).await;
                return Ok((st.session_id.clone(), cwd0));
            }
            Err(e) => log::warn!("[assistant] 恢复长期会话失败，改为新建：{e}"),
        }
    }
    let (sid, _) = agents.assistant_session_new(&cfg.agent_type, &cwd, cfg.mcp_tools).await?;
    st.session_id = sid.clone();
    st.session_agent = cfg.agent_type.clone();
    st.session_provider = "acp".into();
    st.session_cwd = cwd.clone();
    st.session_created_at = now();
    st.primed_key.clear();
    st.applied_model.clear();
    apply_model(db, agents, cfg, &mut st).await;
    save_state(db, &st)?;
    Ok((sid, cwd))
}

async fn apply_model(db: &Db, agents: &AgentManager, cfg: &AssistantConfig, st: &mut AssistantState) {
    if cfg.model.is_null() || cfg.model.as_str() == Some("") { return; }
    let want = cfg.model.to_string();
    if st.applied_model == want { return; }
    match agents.assistant_set_config(&cfg.agent_type, &st.session_id, "model", cfg.model.clone()).await {
        Ok(()) => {
            st.applied_model = want;
            let _ = save_state(db, st);
        }
        Err(e) => log::warn!("[assistant] 设置模型失败：{e}"),
    }
}

fn ensure_api_session(db: &Db) -> Result<AssistantState, String> {
    let mut st = load_state(db);
    if st.session_provider != "api" || st.session_id.is_empty() {
        st.session_id = format!("api-{}", uuid());
        st.session_provider = "api".into();
        st.session_agent = String::new();
        st.session_cwd = String::new();
        st.session_created_at = now();
        st.primed_key.clear();
        st.applied_model.clear();
        save_state(db, &st)?;
    }
    Ok(st)
}

fn chat_url(base: &str) -> Result<String, String> {
    let b = base.trim().trim_end_matches('/');
    if b.is_empty() { return Err("请先在设置中填写接口地址（Base URL）".into()); }
    if !(b.starts_with("http://") || b.starts_with("https://")) { return Err("接口地址需以 http:// 或 https:// 开头".into()); }
    Ok(if b.ends_with("/chat/completions") { b.to_string() } else { format!("{b}/chat/completions") })
}
fn models_url(base: &str) -> Result<String, String> {
    let c = chat_url(base)?;
    Ok(format!("{}/models", c.trim_end_matches("/chat/completions")))
}
fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(180))
        .build()
}
fn http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            format!("接口返回 HTTP {code}：{}", cap_chars(body.trim(), 400))
        }
        other => format!("请求接口失败：{other}"),
    }
}

fn emit_chunk(app: &AppHandle, session: &str, turn: &str, text: &str) {
    let _ = app.emit(EVT_UPDATE, json!({
        "source": "assistant", "sessionId": session, "turnId": turn,
        "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }
    }));
}

fn emit_subtitle(app: &AppHandle, session: &str, turn: &str, sub: &Subtitle) {
    let _ = app.emit(EVT_UPDATE, json!({
        "source": "assistant", "sessionId": session, "turnId": turn,
        "update": { "sessionUpdate": "assistant_subtitle", "text": sub.text, "speech": sub.speech }
    }));
}

/// `say` 工具定义；`speech_lang` = 需要配音版本时的语言
fn say_tool(speech_lang: Option<&str>) -> Value {
    let mut props = json!({
        "text": { "type": "string", "description": "字幕：你此刻说出口的一句话（用户的语言，口语、简短，不超过 30 字）" }
    });
    let mut required = vec!["text"];
    if let Some(l) = speech_lang {
        props["speech"] = json!({ "type": "string", "description": format!("这句字幕的{}版本，用角色原声朗读", speech_lang_name(l)) });
        required.push("speech");
    }
    json!([{
        "type": "function",
        "function": {
            "name": "say",
            "description": "在桌面角色旁的气泡里显示一句字幕（开启语音时会朗读）。可以多次调用，每次一句。完整回答请直接写在回复正文里。",
            "parameters": { "type": "object", "properties": props, "required": required }
        }
    }])
}

#[derive(Default, Debug)]
struct ToolCall {
    id: String,
    name: String,
    args: String,
    done: bool,
}

/// 解析 say 的参数；参数还没收全（JSON 不完整）时返回 None
fn parse_say(args: &str) -> Option<Subtitle> {
    let v: Value = serde_json::from_str(args.trim()).ok()?;
    let text = clean_sub(v.get("text").and_then(Value::as_str).unwrap_or_default());
    if text.is_empty() { return None; }
    let speech = clean_sub(v.get("speech").and_then(Value::as_str).unwrap_or_default());
    Some(Subtitle { text, speech, derived: false })
}

/// 一轮接口调用的上下文
struct ApiCtx<'a> {
    app: &'a AppHandle,
    api: &'a ApiConfig,
    session: &'a str,
    turn: &'a str,
    cancel: &'a AtomicBool,
    /// 电脑操作（None = 未开启）
    desk: Option<DeskCfg>,
}

impl ApiCtx<'_> {
    /// 需要确认的电脑操作：弹权限窗等用户选择；true = 允许
    fn confirm(&self, name: &str, args: &Value) -> Result<bool, String> {
        if ALWAYS_ALLOW.lock().unwrap().iter().any(|n| n == name) { return Ok(true); }
        // 使驾窗口可能被最小化了：先还原，用户才看得到弹窗
        crate::desktop::restore_host();
        let id = format!("{PERM_PREFIX}{}", uuid::Uuid::new_v4());
        let (tx, rx) = std::sync::mpsc::channel();
        pending_perms().lock().unwrap().insert(id.clone(), tx);
        let _ = self.app.emit(crate::acp::EVT_PERMISSION, json!({
            "requestId": id, "agentType": "assistant", "sessionId": self.session,
            "params": {
                "sessionId": self.session,
                "toolCall": { "title": crate::desktop::title(name, args), "kind": crate::desktop::kind(name), "rawInput": args },
                "options": [
                    { "optionId": "allow_once", "name": "允许", "kind": "allow_once" },
                    { "optionId": "allow_always", "name": "本次启动期间始终允许此类操作", "kind": "allow_always" },
                    { "optionId": "reject_once", "name": "拒绝", "kind": "reject_once" }
                ]
            }
        }));
        let started = std::time::Instant::now();
        let answer = loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(a) => break Some(a),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
                Err(_) if self.cancel.load(Ordering::SeqCst) || started.elapsed() > Duration::from_secs(600) => break None,
                Err(_) => {}
            }
        };
        if answer.is_none() {
            pending_perms().lock().unwrap().remove(&id);
            let _ = self.app.emit(crate::acp::EVT_REQUESTS_CANCELLED, json!({ "requestIds": [id] }));
            if self.cancel.load(Ordering::SeqCst) { return Err(ASSISTANT_CANCELLED.into()); }
            return Ok(false);
        }
        match answer.as_deref() {
            Some("allow_always") => { ALWAYS_ALLOW.lock().unwrap().push(name.to_string()); Ok(true) }
            Some("allow_once") => Ok(true),
            _ => Ok(false),
        }
    }

    /// 执行一个电脑操作工具：返回 (给模型的文字, 图片 data URL)
    fn desktop(&self, d: &DeskCfg, vision: bool, name: &str, args: &Value) -> Result<(String, Option<String>), String> {
        if d.confirm && crate::desktop::needs_confirm(name) && !self.confirm(name, args)? {
            return Ok(("用户拒绝了这个操作。不要重复尝试同一操作；需要的话说明原因或问用户。".into(), None));
        }
        let opts = crate::desktop::Opts { workdir: crate::desktop::home_dir(), host_pid: std::process::id(), hide: d.hide, vision };
        match crate::desktop::call(name, args, &opts) {
            Ok(o) => Ok((o.text, o.image)),
            Err(e) => Ok((format!("错误：{e}"), None)),
        }
    }

    /// 收到完整的 say 调用：立刻发给前端显示 / 朗读
    fn take_call(&self, c: &mut ToolCall, subs: &mut Vec<Subtitle>) {
        if c.done || c.name != "say" { return; }
        if let Some(sub) = parse_say(&c.args) {
            c.done = true;
            emit_subtitle(self.app, self.session, self.turn, &sub);
            subs.push(sub);
        }
    }

    /// 发一次 /chat/completions，正文增量写入 `content`，工具调用实时解析；返回本次的工具调用
    fn round(&self, messages: &[Value], tools: Option<&Value>, no_more_tools: bool, content: &mut String, subs: &mut Vec<Subtitle>) -> Result<Vec<ToolCall>, String> {
        let url = chat_url(&self.api.base_url)?;
        let mut body = json!({ "model": self.api.model, "messages": messages, "stream": true });
        if let Some(t) = self.api.temperature { body["temperature"] = json!(t); }
        if let Some(t) = tools {
            body["tools"] = t.clone();
            if no_more_tools { body["tool_choice"] = json!("none"); }
        }
        let mut req = http_agent().post(&url).set("Content-Type", "application/json").set("Accept", "text/event-stream, application/json");
        if !self.api.api_key.is_empty() { req = req.set("Authorization", &format!("Bearer {}", self.api.api_key)); }
        let resp = req.send_string(&body.to_string()).map_err(http_err)?;
        let ct = resp.header("content-type").unwrap_or("").to_ascii_lowercase();
        let mut calls: Vec<ToolCall> = Vec::new();
        if !ct.contains("event-stream") {
            let v: Value = serde_json::from_reader(resp.into_reader()).map_err(|e| format!("接口返回无法解析：{e}"))?;
            if let Some(e) = v.get("error") { return Err(format!("接口错误：{}", cap_chars(&e.to_string(), 400))); }
            for (i, tc) in v.pointer("/choices/0/message/tool_calls").and_then(Value::as_array).cloned().unwrap_or_default().iter().enumerate() {
                let mut c = ToolCall {
                    id: tc.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("call_{i}")),
                    name: tc.pointer("/function/name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    args: tc.pointer("/function/arguments").and_then(Value::as_str).unwrap_or_default().to_string(),
                    done: false,
                };
                self.take_call(&mut c, subs);
                calls.push(c);
            }
            let out = v.pointer("/choices/0/message/content").and_then(Value::as_str).unwrap_or_default();
            if !out.is_empty() {
                content.push_str(out);
                emit_chunk(self.app, self.session, self.turn, out);
            }
            return Ok(calls);
        }
        let reader = BufReader::new(resp.into_reader());
        for line in reader.lines() {
            if self.cancel.load(Ordering::SeqCst) { return Err(ASSISTANT_CANCELLED.into()); }
            let line = line.map_err(|e| format!("读取接口流失败：{e}"))?;
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" { break; }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(e) = v.get("error") { return Err(format!("接口错误：{}", cap_chars(&e.to_string(), 400))); }
            if let Some(d) = v.pointer("/choices/0/delta/content").and_then(Value::as_str) {
                if !d.is_empty() {
                    content.push_str(d);
                    emit_chunk(self.app, self.session, self.turn, d);
                }
            }
            for tc in v.pointer("/choices/0/delta/tool_calls").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default() {
                let idx = tc.get("index").and_then(Value::as_u64).map(|i| i as usize).unwrap_or(calls.len().saturating_sub(1));
                while calls.len() <= idx { calls.push(ToolCall { id: format!("call_{}", calls.len()), ..Default::default() }); }
                let c = &mut calls[idx];
                if let Some(id) = tc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) { c.id = id.to_string(); }
                if let Some(n) = tc.pointer("/function/name").and_then(Value::as_str) { c.name.push_str(n); }
                if let Some(a) = tc.pointer("/function/arguments").and_then(Value::as_str) { c.args.push_str(a); }
                self.take_call(c, subs);
            }
        }
        if self.cancel.load(Ordering::SeqCst) { return Err(ASSISTANT_CANCELLED.into()); }
        for c in calls.iter_mut() { self.take_call(c, subs); }
        Ok(calls)
    }
}

/// OpenAI 兼容 /chat/completions（优先流式；服务端不支持流式时按普通 JSON 解析）。
/// `tools` = say 工具：模型先调 say 给字幕、再写正文；只调了 say 没写正文时带上工具结果再请求一次（最多 3 轮）。
/// 接口拒绝 tools 参数时换成 `tag_sys`（标记协议的设定）重试，并记住这个接口。
/// 返回的文本 = 工具字幕（编码成标记）+ 正文。
fn api_chat(ctx: &ApiCtx, mut messages: Vec<Value>, mut tools: Option<Value>, tag_sys: Option<String>) -> Result<String, String> {
    if ctx.api.model.is_empty() { return Err("请先在设置中填写模型名称".into()); }
    let mut content = String::new();
    let mut subs: Vec<Subtitle> = Vec::new();
    let partial = |subs: &[Subtitle], content: &str| format!("{ASSISTANT_CANCELLED}{}{content}", encode_subtitles(subs));
    // 带 MCP 工具时允许多轮（查项目 → 读上下文 → 回答）；操作电脑（截图 → 点击 → 再截图…）轮数更多；只有 say 时最多 3 轮
    let has_mcp = tools.as_ref().and_then(Value::as_array).is_some_and(|a| a.len() > 1);
    let max_rounds = if ctx.desk.is_some() { MAX_DESKTOP_ROUNDS } else if has_mcp { MAX_TOOL_ROUNDS } else { 3 };
    let mut vision = ctx.desk.as_ref().is_some_and(|d| d.vision) && !no_vision().lock().unwrap().contains(&tools_key(ctx.api));
    let mut round = 0;
    let res = (|| -> Result<String, String> {
    while round < max_rounds {
        let last = round + 1 == max_rounds;
        let start = content.len();
        prune_images(&mut messages, MAX_IMAGES);
        let calls = match ctx.round(&messages, tools.as_ref(), last && round > 0, &mut content, &mut subs) {
            Ok(c) => c,
            Err(e) if e.starts_with(ASSISTANT_CANCELLED) => return Err(partial(&subs, &content)),
            // 带图请求被拒：多半是模型不支持图片输入，去掉图片重试，本接口以后不再发图
            Err(e) if has_images(&messages) && content.len() == start && vision_rejected(&e) => {
                log::warn!("[assistant] 接口拒绝了图片输入，改为不发图：{e}");
                no_vision().lock().unwrap().insert(tools_key(ctx.api));
                vision = false;
                prune_images(&mut messages, 0);
                for m in messages.iter_mut().filter(|m| m["role"] == "tool") {
                    if m["content"].as_str().is_some_and(|t| t.starts_with("已截图")) {
                        m["content"] = json!("截图失败：当前模型不支持图片输入，看不到屏幕。请改用窗口列表、快捷键或命令，做不到就告诉用户。");
                    }
                }
                continue;
            }
            Err(e) if round == 0 && tools.is_some() && subs.is_empty() && content.is_empty() && tools_rejected(&e) => {
                log::warn!("[assistant] 接口不支持工具调用，字幕改用标记：{e}");
                no_tools().lock().unwrap().insert(tools_key(ctx.api));
                tools = None;
                if let (Some(sys), Some(first)) = (tag_sys.as_ref(), messages.first_mut()) { first["content"] = json!(sys); }
                continue;
            }
            Err(e) => return Err(e),
        };
        round += 1;
        if tools.is_none() || calls.is_empty() { break; }
        let uses_mcp = calls.iter().any(|c| c.name != "say");
        // 只说了字幕且写了正文：这轮完成
        if !uses_mcp && !content.trim().is_empty() { break; }
        // 把工具调用和结果补进上下文，让模型接着写（只说了字幕时也可以什么都不写）
        let text = content[start..].to_string();
        let tc: Vec<Value> = calls.iter().map(|c| json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": c.args } })).collect();
        messages.push(json!({ "role": "assistant", "content": if text.trim().is_empty() { Value::Null } else { json!(text) }, "tool_calls": tc }));
        let mut images: Vec<Value> = Vec::new();
        for c in &calls {
            if ctx.cancel.load(Ordering::SeqCst) { return Err(partial(&subs, &content)); }
            let r = if c.name == "say" {
                (if c.done { "已显示" } else { "参数无效，需要 text" }).to_string()
            } else {
                let args: Value = serde_json::from_str(c.args.trim()).unwrap_or_else(|_| json!({}));
                match ctx.desk.as_ref().filter(|_| crate::desktop::is_desktop(&c.name)) {
                    Some(d) => match ctx.desktop(d, vision, &c.name, &args) {
                        Ok((t, img)) => {
                            if let Some(url) = img {
                                images.push(json!({ "type": "text", "text": format!("（{} 的图片）", c.name) }));
                                images.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                            }
                            cap_chars(&t, MAX_TOOL_RESULT)
                        }
                        Err(e) if e.starts_with(ASSISTANT_CANCELLED) => return Err(partial(&subs, &content)),
                        Err(e) => format!("错误：{}", cap_chars(&e, 2000)),
                    },
                    None => match crate::mcp::assistant_call(ctx.app, &c.name, &args) {
                        Ok(t) => cap_chars(&t, MAX_TOOL_RESULT),
                        Err(e) => format!("错误：{}", cap_chars(&e, 2000)),
                    },
                }
            };
            messages.push(json!({ "role": "tool", "tool_call_id": c.id, "content": r }));
        }
        // 工具消息只能是文字：截图 / 图片紧跟着作为一条用户消息附上（OpenAI image_url）
        if !images.is_empty() {
            messages.push(json!({ "role": "user", "content": images }));
        }
        // 工具前后的两段正文之间空一行
        if uses_mcp && !content.trim().is_empty() && !content.ends_with('\n') {
            content.push_str("\n\n");
            emit_chunk(ctx.app, ctx.session, ctx.turn, "\n\n");
        }
    }
    Ok(format!("{}{content}", encode_subtitles(&subs)))
    })();
    if ctx.desk.as_ref().is_some_and(|d| d.hide) { crate::desktop::restore_host(); }
    res
}

/// 执行一轮：组装提示词 → 发给 ACP / API → 返回原始回复文本（含指令标记）。
/// `Err` 以 ASSISTANT_CANCELLED 开头表示被停止（其后为部分文本）。
async fn run_turn(
    app: &AppHandle, db: &Arc<Db>, agents: &AgentManager, rt: &AssistantRuntime,
    cfg: &AssistantConfig, persona: &Persona, app_state: Option<&str>, turn: Turn<'_>,
    turn_id: &str, cancel: Arc<AtomicBool>, speech: Option<&SpeechHint>, subtitles: bool, screen: Option<&str>,
) -> Result<String, String> {
    let mems = list_memories(db)?;
    let name = persona.display_name().to_string();
    // 助手自己设的定时（每轮附带，避免重复设、便于取消）
    let wakes_now = if cfg.mcp_tools && subtitles { wake_block(db) } else { String::new() };
    let body = match &turn {
        Turn::User(t) => t.to_string(),
        Turn::Event(e) => e.clone(),
    };
    if cfg.provider == "api" {
        let st = ensure_api_session(db)?;
        let sub = if !subtitles { SubMode::Off } else if no_tools().lock().unwrap().contains(&tools_key(&cfg.api)) { SubMode::Tag } else { SubMode::Tool };
        let build_sys = |sub: SubMode| {
            let mut sys = format!("{}\n\n{}", system_prompt(persona, &mems, cfg, sub), state_line(app_state, cfg));
            if !wakes_now.is_empty() { sys.push('\n'); sys.push_str(&wakes_now); }
            if let Some(l) = speech_rule(speech, sub) { sys.push('\n'); sys.push_str(&l); }
            sys
        };
        let sys = build_sys(sub);
        let (tools, tag_sys) = if sub == SubMode::Tool {
            let mut t = say_tool(speech_layer_lang(speech).as_deref());
            if cfg.mcp_tools {
                if let (Some(arr), Value::Array(more)) = (t.as_array_mut(), crate::mcp::assistant_function_tools()) { arr.extend(more); }
            }
            if desktop_on(cfg, sub) {
                let vision = cfg.vision && !no_vision().lock().unwrap().contains(&tools_key(&cfg.api));
                if let Some(arr) = t.as_array_mut() { arr.extend(crate::desktop::tool_specs(true, true, vision)); }
            }
            (Some(t), Some(build_sys(SubMode::Tag)))
        } else {
            (None, None)
        };
        let mut messages = vec![json!({ "role": "system", "content": sys })];
        // 本轮用户消息已先入库：历史里排除最后一条（就是它）
        let mut hist = recent_messages(db, cfg.history_turns + 1)?;
        if matches!(turn, Turn::User(_)) && hist.last().is_some_and(|m| m.role == "user") { hist.pop(); }
        for m in hist.iter().rev().take(cfg.history_turns).collect::<Vec<_>>().into_iter().rev() {
            let role = if m.role == "user" { "user" } else { "assistant" };
            messages.push(json!({ "role": role, "content": m.content }));
        }
        let vision_ok = cfg.vision && !no_vision().lock().unwrap().contains(&tools_key(&cfg.api));
        match screen {
            Some(png) if vision_ok => messages.push(json!({ "role": "user", "content": [
                { "type": "text", "text": body },
                { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{png}") } }
            ] })),
            Some(_) => messages.push(json!({ "role": "user", "content": format!("{body}

（截图未发送：当前模型未开启读图。如果看不到就直接告诉用户，并提示可在设置里开启「模型支持读图」）") })),
            None => messages.push(json!({ "role": "user", "content": body })),
        }
        let api = cfg.api.clone();
        let app2 = app.clone();
        let sid = st.session_id.clone();
        let tid = turn_id.to_string();
        let desk = (desktop_on(cfg, sub) && tools.is_some()).then(|| DeskCfg { confirm: cfg.desktop_confirm, hide: cfg.desktop_hide, vision: cfg.vision });
        return tokio::task::spawn_blocking(move || {
            let ctx = ApiCtx { app: &app2, api: &api, session: &sid, turn: &tid, cancel: &cancel, desk };
            api_chat(&ctx, messages, tools, tag_sys)
        })
            .await
            .map_err(|e| format!("接口任务异常：{e}"))?;
    }
    let (sid, cwd) = ensure_acp_session(db, agents, rt, cfg).await?;
    let st = load_state(db);
    // 人设 + 设定版本：任一变化都整份重新注入
    let pkey = format!("{}-{PROMPT_REV}{}", persona.key(), if cfg.mcp_tools { "m" } else { "" });
    let primed_key = format!("{sid}|{pkey}|{}", st.memory_version);
    let mut text = String::new();
    if st.primed_key != primed_key {
        let same_session_and_persona = st.primed_key.starts_with(&format!("{sid}|{pkey}|"));
        if same_session_and_persona {
            // 只是记忆有变化（用户在面板里改了）：补发最新记忆
            text.push_str("[长期记忆已更新]\n");
            text.push_str(&memory_block(&mems, cfg.memory_limit));
        } else {
            text.push_str(&system_prompt(persona, &mems, cfg, SubMode::Tag));
            let mut hist = recent_messages(db, cfg.history_turns + 1)?;
            if matches!(turn, Turn::User(_)) && hist.last().is_some_and(|m| m.role == "user") { hist.pop(); }
            if !hist.is_empty() && cfg.history_turns > 0 {
                let take = hist.len().saturating_sub(cfg.history_turns);
                text.push_str("\n\n[最近对话回顾]（来自之前的会话，供衔接上下文）\n");
                text.push_str(&recap_block(&hist[take..], &name));
            }
        }
        text.push_str("\n\n");
    }
    text.push_str(&state_line(app_state, cfg));
    if !wakes_now.is_empty() { text.push('\n'); text.push_str(&wakes_now); }
    let sub = if subtitles { SubMode::Tag } else { SubMode::Off };
    if subtitles {
        text.push('\n');
        text.push_str(match speech {
            Some(h) if h.enabled && h.full && speech_layer_lang(Some(h)).is_some() => TAG_REMINDER_FULL,
            _ => TAG_REMINDER,
        });
    }
    if let Some(l) = speech_rule(speech, sub) { text.push('\n'); text.push_str(&l); }
    text.push_str("\n\n");
    match &turn {
        Turn::User(t) => { text.push_str("[用户消息]\n"); text.push_str(t); }
        Turn::Event(e) => text.push_str(e),
    }
    let r = agents.assistant_prompt(&cfg.agent_type, &sid, &cwd, &text, turn_id, cfg.mcp_tools, screen).await;
    // 只要请求已送达（成功或被停止）就视为设定已注入
    if r.is_ok() || r.as_ref().err().is_some_and(|e| e.starts_with(ASSISTANT_CANCELLED)) {
        let mut st = load_state(db);
        st.primed_key = format!("{sid}|{pkey}|{}", st.memory_version);
        let _ = save_state(db, &st);
    }
    r
}

#[derive(Serialize)]
pub struct TurnResult {
    pub message: Option<AssistantMessage>,
    pub memories_added: usize,
    pub memories_forgotten: usize,
    /// 助手判断连续语音对话可以结束了
    pub end_conversation: bool,
    /// 主动关怀：助手选择这次不打扰（没有消息）
    pub skipped: bool,
    /// 字幕层：气泡里显示 / 朗读的短句（模型没给时从正文推导一条）
    pub subtitles: Vec<Subtitle>,
    /// 有字幕之外的正文（气泡里提示「查看完整回复」）
    pub has_body: bool,
}

/// 处理回复：剥指令、落记忆、存消息
fn finish_reply(db: &Db, cfg: &AssistantConfig, raw: Result<String, String>, greeting: bool) -> Result<TurnResult, String> {
    finish_reply_ext(db, cfg, raw, greeting, false)
}

/// `care` = 主动关怀回合：回复 `[[跳过]]` 时不存消息、不落记忆
fn finish_reply_ext(db: &Db, cfg: &AssistantConfig, raw: Result<String, String>, greeting: bool, care: bool) -> Result<TurnResult, String> {
    let (raw, interrupted) = match raw {
        Ok(t) => (t, false),
        Err(e) if e.starts_with(ASSISTANT_CANCELLED) => (e[ASSISTANT_CANCELLED.len()..].to_string(), true),
        Err(e) => return Err(e),
    };
    let (raw, end_conversation, skip) = extract_flags(&raw);
    let skipped = care && skip;
    if skipped {
        return Ok(TurnResult { message: None, memories_added: 0, memories_forgotten: 0, end_conversation: false, skipped: true, subtitles: Vec::new(), has_body: false });
    }
    let (clean, remember, forget) = extract_directives(&raw);
    let (body, mut subtitles) = extract_subtitles(&clean);
    let body = strip_speech_tags(&body);
    if subtitles.is_empty() && !body.trim().is_empty() {
        subtitles.extend(derive_subtitles(&body).into_iter().map(|text| Subtitle { text, speech: String::new(), derived: true }));
    }
    let sub_text = subtitles.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n");
    let has_body = !body.trim().is_empty() && body.trim() != sub_text.trim();
    // 只有字幕（闲聊）：字幕就是这条记录
    let clean = if body.trim().is_empty() { sub_text } else { body };
    let mut added = 0;
    let mut forgotten = 0;
    if !interrupted {
        if cfg.auto_memory {
            for m in &remember {
                if add_memory(db, m, "auto", false)? { added += 1; }
            }
        }
        for k in &forget { forgotten += forget_memories(db, k)?; }
        if added > 0 || forgotten > 0 { bump_memory_version(db, true); }
    }
    let message = if clean.trim().is_empty() {
        None
    } else {
        let mut flags = Vec::new();
        if interrupted { flags.push("interrupted"); }
        if greeting { flags.push("greeting"); }
        Some(insert_message(db, "assistant", &clean, &cfg.provider, &flags)?)
    };
    Ok(TurnResult { message, memories_added: added, memories_forgotten: forgotten, end_conversation, skipped: false, subtitles, has_body })
}

/// 回合已送达（成功或被停止）：当前会话视为已问候，之后不再自动打招呼
fn mark_greeted(db: &Db, raw: &Result<String, String>) {
    if raw.is_err() && !raw.as_ref().err().is_some_and(|e| e.starts_with(ASSISTANT_CANCELLED)) { return; }
    let mut st = load_state(db);
    if st.session_id.is_empty() || st.greeted_session == st.session_id { return; }
    st.greeted_session = st.session_id.clone();
    let _ = save_state(db, &st);
}

fn check_enabled(cfg: &AssistantConfig) -> Result<(), String> {
    if !cfg.enabled { return Err("全局助手未启用".into()); }
    if cfg.provider == "acp" && cfg.agent_type.trim().is_empty() { return Err("请先在设置中选择 ACP 客户端".into()); }
    Ok(())
}

// ---------------------------------------------------------------- wake（工作流唤醒助手 / 助手给自己设定时）

/// 工作流的「全局助手」节点运行时推给前端的事件；前端带上人设与语音设置回调 `assistant_wake`
pub const EVT_WAKE: &str = "assistant://wake";
/// 助手通过 MCP 工具设 / 取消了定时（前端提示并刷新工作流列表）
pub const EVT_SCHEDULES: &str = "assistant://schedules";
/// 助手自己设的定时最多保留多少个
const MAX_WAKES: usize = 30;

/// 工作流里「全局助手」节点的一次请求
#[derive(Clone, Debug)]
pub struct WakeRequest {
    pub workflow_id: String,
    pub workflow_name: String,
    pub step: String,
    pub message: String,
    /// 助手自己设的定时（唤醒自己）
    pub by_assistant: bool,
}

type WakeReply = Result<String, String>;

struct PendingWake {
    tx: Option<tokio::sync::oneshot::Sender<WakeReply>>,
    event: String,
}

fn wakes() -> &'static Mutex<HashMap<String, PendingWake>> {
    static W: OnceLock<Mutex<HashMap<String, PendingWake>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 唤醒时发给助手的系统事件
pub fn wake_event(req: &WakeRequest) -> String {
    let msg = cap_chars(req.message.trim(), 2000);
    if req.by_assistant {
        format!(
            "[系统事件 · 定时唤醒] 你之前给自己设的定时到了。当时写给自己的话：\n{msg}\n\n\
现在就按这件事行动：通常用字幕跟用户说一两句（保持人设口吻，像你自己想起来的那样自然），需要详细内容再写正文。\
不要提「系统」「定时任务」「工作流」「唤醒」这类字眼。如果这件事是一年一次的（比如生日、纪念日），记得设好下一年的定时。\
如果现在说这件事已经不合适（比如已经办完了、刚聊过），就只输出 [[跳过]]。"
        )
    } else {
        format!(
            "[系统事件 · 工作流「{}」] 用户在工作流里安排了这件事交给你（节点「{}」）：\n{msg}\n\n\
请按要求处理。要跟用户说的话用字幕说（简短），详细结果写在正文里（正文会作为这个节点的输出交给工作流的后续步骤）。",
            req.workflow_name, req.step
        )
    }
}

/// 工作流引擎调用：登记请求并通知前端；返回 (请求 id, 等待回复的接收端)
pub fn wake_begin(app: &AppHandle, db: &Db, req: WakeRequest) -> Result<(String, tokio::sync::oneshot::Receiver<WakeReply>), String> {
    let cfg = load_config(db);
    check_enabled(&cfg)?;
    let id = format!("wake-{}", uuid());
    let (tx, rx) = tokio::sync::oneshot::channel();
    wakes().lock().unwrap().insert(id.clone(), PendingWake { tx: Some(tx), event: wake_event(&req) });
    let payload = json!({ "reqId": id, "workflowId": req.workflow_id, "workflowName": req.workflow_name, "byAssistant": req.by_assistant });
    if let Err(e) = app.emit(EVT_WAKE, payload) {
        wakes().lock().unwrap().remove(&id);
        return Err(format!("通知界面失败：{e}"));
    }
    Ok((id, rx))
}

/// 工作流引擎调用：不再等待（完成 / 超时 / 停止）
pub fn wake_end(id: &str) {
    wakes().lock().unwrap().remove(id);
}

fn wake_resolve(id: &str, r: WakeReply) {
    if let Some(p) = wakes().lock().unwrap().get_mut(id) {
        if let Some(tx) = p.tx.take() { let _ = tx.send(r); }
    }
}

/// 回合结果 → 交给工作流的文本
fn wake_reply_text(t: &TurnResult) -> String {
    if t.skipped { return "（助手选择这次不打扰）".into(); }
    match &t.message {
        Some(m) => m.content.clone(),
        None => t.subtitles.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n"),
    }
}

/// 「早上 8 点半」「21:30」「9pm」→ (时, 分)；`period` = 外部给出的时段（今晚 / 明早）
fn parse_tod(s: &str, period: Option<&str>) -> Option<(u32, u32)> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(
        r"(?i)^(早上|早晨|清晨|上午|中午|下午|傍晚|晚上|夜里|夜晚|凌晨)?\s*(\d{1,2})(?:\s*:\s*(\d{1,2})|\s*[点时]\s*(?:(\d{1,2})\s*分?|(半)|(一刻)|(三刻)|整)?)?\s*(am|pm)?$"
    ).unwrap());
    let c = re.captures(s.trim())?;
    let mut h: u32 = c[2].parse().ok()?;
    let m: u32 = if let Some(x) = c.get(3).or(c.get(4)) { x.as_str().parse().ok()? }
        else if c.get(5).is_some() { 30 } else if c.get(6).is_some() { 15 } else if c.get(7).is_some() { 45 } else { 0 };
    let per = c.get(1).map(|x| x.as_str()).or(period).unwrap_or("");
    match per {
        "下午" | "傍晚" | "晚上" | "夜里" | "夜晚" if h < 12 => h += 12,
        "中午" if h < 11 => h += 12,
        "凌晨" if h == 12 => h = 0,
        _ => {}
    }
    match c.get(8).map(|x| x.as_str().to_ascii_lowercase()).as_deref() {
        Some("pm") if h < 12 => h += 12,
        Some("am") if h == 12 => h = 0,
        _ => {}
    }
    (h < 24 && m < 60).then_some((h, m))
}

fn weekday_num(c: char) -> Option<u32> {
    Some(match c {
        '一' | '1' => 1, '二' | '2' => 2, '三' | '3' => 3, '四' | '4' => 4, '五' | '5' => 5, '六' | '6' => 6,
        '日' | '天' | '七' | '7' => 7,
        _ => return None,
    })
}

fn hhmm(h: u32, m: u32) -> String { format!("{h:02}:{m:02}") }

fn once_at(t: chrono::NaiveDateTime) -> crate::models::ScheduleConfig {
    crate::models::ScheduleConfig::Once { at: t.format("%Y-%m-%d %H:%M").to_string(), delete_after_run: true }
}

/// 解析助手写的时间：相对（30分钟后）、今天/明晚 + 时刻、日期 + 时刻、每天 / 每周 / 工作日 / 每 N 小时
pub fn parse_when(s: &str, now: chrono::NaiveDateTime) -> Result<crate::models::ScheduleConfig, String> {
    use crate::models::ScheduleConfig as S;
    use chrono::{Datelike, Duration as D, NaiveDate, NaiveTime};
    let s0 = s.trim().replace('：', ":").replace('，', ",").replace('、', ",");
    let s = s0.trim().trim_end_matches(['。', '.']).trim();
    let bad = || format!("看不懂的时间「{s}」（可写：30分钟后 / 明天 9:00 / 06-18 20:00 / 每天 08:30 / 每周一三五 21:00 / 每2小时）");
    if s.is_empty() { return Err(bad()); }
    let at = |d: NaiveDate, h: u32, m: u32| d.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap());
    let lower = s.to_lowercase();

    // 相对时间
    static REL: OnceLock<regex::Regex> = OnceLock::new();
    let rel = REL.get_or_init(|| regex::Regex::new(
        r"^(?:in\s*|过\s*)?(\d+(?:\.\d+)?|半)\s*(?:个)?\s*(分钟|分|mins?|minutes?|小时|钟头|h|hrs?|hours?|天|days?|周|星期|礼拜|weeks?)\s*(?:后|以后|之后|later)?$"
    ).unwrap());
    if let Some(c) = rel.captures(&lower) {
        let n: f64 = if &c[1] == "半" { 0.5 } else { c[1].parse().map_err(|_| bad())? };
        let unit = &c[2];
        let mins = if unit.starts_with('分') || unit.starts_with("min") { n }
            else if unit.starts_with('小') || unit.starts_with('钟') || unit.starts_with('h') { n * 60.0 }
            else if unit.starts_with('天') || unit.starts_with("day") { n * 1440.0 }
            else { n * 10080.0 };
        let mins = mins.round() as i64;
        if mins < 1 { return Err(bad()); }
        if mins > 366 * 1440 { return Err("定时不能超过一年".into()); }
        return Ok(once_at(now + D::minutes(mins)));
    }

    // 每 N 分钟 / 小时
    static EVERY: OnceLock<regex::Regex> = OnceLock::new();
    let every = EVERY.get_or_init(|| regex::Regex::new(
        r"^(?:每隔?|every)\s*(\d+)?\s*(?:个)?\s*(分钟|mins?|minutes?|小时|钟头|hours?|hrs?|h)$"
    ).unwrap());
    if let Some(c) = every.captures(&lower) {
        let n: i64 = c.get(1).map(|x| x.as_str().parse().unwrap_or(1)).unwrap_or(1).max(1);
        let unit = &c[2];
        let mins = if unit.starts_with('分') || unit.starts_with("min") { n } else { n * 60 };
        if mins < 5 { return Err("重复定时的间隔至少 5 分钟".into()); }
        return Ok(S::Interval { every_minutes: mins });
    }
    if lower.starts_with("每年") || lower.starts_with("every year") || lower.starts_with("yearly") {
        return Err("不支持「每年」：请设具体日期（如 06-18 09:00），到点后再设下一年的".into());
    }

    // 每天 / 工作日 / 周末 / 每周X
    let tod = |rest: &str| -> Result<(u32, u32), String> {
        let rest = rest.trim().trim_start_matches(',').trim();
        if rest.is_empty() { return Ok((9, 0)); }
        parse_tod(rest, None).ok_or_else(bad)
    };
    for p in ["每天", "每日", "天天", "every day", "everyday", "daily"] {
        if let Some(rest) = lower.strip_prefix(p) {
            let (h, m) = tod(rest)?;
            return Ok(S::Daily { time: hhmm(h, m) });
        }
    }
    for (p, days) in [("工作日", vec![1, 2, 3, 4, 5]), ("每个工作日", vec![1, 2, 3, 4, 5]), ("weekdays", vec![1, 2, 3, 4, 5]), ("周末", vec![6, 7]), ("每周末", vec![6, 7]), ("weekends", vec![6, 7])] {
        if let Some(rest) = lower.strip_prefix(p) {
            let (h, m) = tod(rest)?;
            return Ok(S::Weekly { weekdays: days, time: hhmm(h, m) });
        }
    }
    for p in ["每个星期", "每星期", "每礼拜", "每周"] {
        if let Some(rest) = s.strip_prefix(p) {
            let mut days = Vec::new();
            let mut idx = rest.len();
            for (i, ch) in rest.char_indices() {
                if let Some(d) = weekday_num(ch).filter(|_| !ch.is_ascii_digit()) {
                    if !days.contains(&d) { days.push(d); }
                } else if matches!(ch, ',' | '和' | '与' | ' ' | '周' | '星' | '期' | '及') {
                } else {
                    idx = i;
                    break;
                }
            }
            if days.is_empty() { return Err(bad()); }
            days.sort();
            let (h, m) = tod(&rest[idx..])?;
            return Ok(S::Weekly { weekdays: days, time: hhmm(h, m) });
        }
    }

    // 今天 / 今晚 / 明天 / 明早 / 后天 + 时刻
    let today = now.date();
    for (p, off, period, def) in [
        ("今天", 0, None, None), ("今日", 0, None, None), ("today", 0, None, None),
        ("今晚", 0, Some("晚上"), Some((20, 0))), ("tonight", 0, Some("晚上"), Some((20, 0))),
        ("今早", 0, Some("早上"), None), ("今天早上", 0, Some("早上"), None),
        ("明天", 1, None, Some((9, 0))), ("明日", 1, None, Some((9, 0))), ("tomorrow", 1, None, Some((9, 0))),
        ("明早", 1, Some("早上"), Some((8, 0))), ("明晚", 1, Some("晚上"), Some((20, 0))),
        ("后天", 2, None, Some((9, 0))), ("大后天", 3, None, Some((9, 0))),
    ] {
        if let Some(rest) = lower.strip_prefix(p) {
            let rest = rest.trim().trim_start_matches(',').trim();
            let (h, m) = if rest.is_empty() { def.ok_or_else(bad)? } else { parse_tod(rest, period).ok_or_else(bad)? };
            let t = at(today + D::days(off), h, m);
            if t <= now { return Err(format!("「{s}」已经过去了")); }
            return Ok(once_at(t));
        }
    }

    // 日期（可带年份）+ 时刻
    static DATE: OnceLock<regex::Regex> = OnceLock::new();
    let date = DATE.get_or_init(|| regex::Regex::new(r"^(?:(\d{4})\s*[-/年.]\s*)?(\d{1,2})\s*[-/月.]\s*(\d{1,2})\s*[日号]?\s*(?:t|\s)?\s*(.*)$").unwrap());
    if let Some(c) = date.captures(&lower) {
        let mo: u32 = c[2].parse().map_err(|_| bad())?;
        let d: u32 = c[3].parse().map_err(|_| bad())?;
        let rest = c.get(4).map(|x| x.as_str().trim()).unwrap_or("");
        let (h, m) = if rest.is_empty() { (9, 0) } else { parse_tod(rest, None).ok_or_else(bad)? };
        return match c.get(1) {
            Some(y) => {
                let y: i32 = y.as_str().parse().map_err(|_| bad())?;
                let day = NaiveDate::from_ymd_opt(y, mo, d).ok_or_else(bad)?;
                let t = at(day, h, m);
                if t <= now { return Err(format!("「{s}」已经过去了")); }
                Ok(once_at(t))
            }
            None => {
                let mut y = today.year();
                loop {
                    if let Some(day) = NaiveDate::from_ymd_opt(y, mo, d) {
                        let t = at(day, h, m);
                        if t > now { break Ok(once_at(t)); }
                    }
                    y += 1;
                    if y > today.year() + 4 { break Err(bad()); }
                }
            }
        };
    }

    // 只有时刻：今天还没到就今天，否则明天
    if let Some((h, m)) = parse_tod(&lower, None) {
        let mut t = at(today, h, m);
        if t <= now { t += D::days(1); }
        return Ok(once_at(t));
    }
    Err(bad())
}

/// 定时的人类可读描述
pub fn sched_label(s: &crate::models::ScheduleConfig) -> String {
    use crate::models::ScheduleConfig as S;
    match s {
        S::Interval { every_minutes } if every_minutes % 60 == 0 => format!("每 {} 小时", every_minutes / 60),
        S::Interval { every_minutes } => format!("每 {every_minutes} 分钟"),
        S::Daily { time } => format!("每天 {time}"),
        S::Weekly { weekdays, time } => {
            const N: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
            let d: Vec<&str> = weekdays.iter().filter_map(|w| N.get((*w as usize).wrapping_sub(1)).copied()).collect();
            format!("每周{} {time}", d.join("、"))
        }
        S::Once { at, .. } => format!("{at}（一次）"),
    }
}

/// 助手自己设的定时（工作流, 写给自己的话）
pub fn list_wakes(db: &Db) -> Result<Vec<(crate::models::Workflow, String)>, String> {
    Ok(db.list_workflows(crate::models::NO_PROJECT_ID)?
        .into_iter()
        .filter_map(|w| {
            let msg = w.steps.iter().find_map(|s| match s {
                crate::models::WorkflowStep::Assistant { message, by_assistant: true, .. } => Some(message.clone()),
                _ => None,
            })?;
            Some((w, msg))
        })
        .collect())
}

/// 建一个「到点唤醒助手」的工作流（放在「无项目」下，用户可在工作流页查看 / 删除）
/// 返回 (工作流, 是否已存在同样的定时)
pub fn create_wake(db: &Db, when: &str, message: &str, now: chrono::NaiveDateTime) -> Result<(crate::models::Workflow, bool), String> {
    use crate::models::WorkflowStep as W;
    let msg = cap_chars(message.trim(), 500);
    if msg.is_empty() { return Err("定时内容为空".into()); }
    let sched = crate::schedule::normalize(parse_when(when, now)?);
    let existing = list_wakes(db)?;
    if let Some((w, _)) = existing.iter().find(|(w, m)| m.trim() == msg && w.schedule.as_ref() == Some(&sched)) {
        return Ok((w.clone(), true));
    }
    if existing.len() >= MAX_WAKES { return Err(format!("助手的定时已达上限 {MAX_WAKES} 个，先取消一些")); }
    let steps = vec![
        W::Start { name: String::new(), x: 0.0, y: 0.0 },
        W::Assistant { name: "唤醒助手".into(), message: msg.clone(), timeout_sec: None, continue_on_error: false, by_assistant: true, x: 0.0, y: 0.0 },
    ];
    let n = crate::wf_graph::normalize(steps, Some(vec![(0, 1)]), true)?;
    let name = format!("助手定时 · {}", cap_chars(&msg.replace('\n', " "), 18));
    let w = db.create_workflow(
        crate::models::NO_PROJECT_ID, &name, "由全局助手创建：到点唤醒助手", true, "schedule",
        Some(&sched), &n.steps, &std::collections::BTreeMap::new(), &n.edges,
    )?;
    crate::glog::info("assistant", format!("助手设了定时：{} · {}", sched_label(&sched), cap_chars(&msg, 40)));
    Ok((w, false))
}

/// 按 id 取消助手设的定时（只能删助手自己建的）
pub fn cancel_wake_id(db: &Db, id: &str) -> Result<usize, String> {
    match list_wakes(db)?.into_iter().find(|(w, _)| w.id == id.trim()) {
        Some((w, _)) => { db.delete_workflow(&w.id)?; Ok(1) }
        None => Err(format!("没有 id 为 {id} 的助手定时（用 assistant_schedule_list 查看）")),
    }
}

/// 按关键词取消助手设的定时（匹配名称或内容；「全部」= 全部取消）
pub fn cancel_wakes(db: &Db, keyword: &str) -> Result<usize, String> {
    let k = keyword.trim().to_lowercase();
    let all = matches!(k.as_str(), "全部" | "所有" | "all" | "*");
    if !all && k.chars().count() < 2 { return Ok(0); }
    let mut n = 0;
    for (w, m) in list_wakes(db)? {
        if all || m.to_lowercase().contains(&k) || w.name.to_lowercase().contains(&k) {
            db.delete_workflow(&w.id)?;
            n += 1;
        }
    }
    Ok(n)
}

/// 每轮附带：当前已设的定时
fn wake_block(db: &Db) -> String {
    let list = match list_wakes(db) { Ok(l) if !l.is_empty() => l, _ => return String::new() };
    let mut s = String::from("[你设下的定时]（用户看不到这段）");
    for (w, m) in list.iter().take(10) {
        let label = w.schedule.as_ref().map(sched_label).unwrap_or_default();
        let next = w.next_run_at.as_deref().map(|t| format!(" · 下次 {}", t.replace('T', " ").chars().take(16).collect::<String>())).unwrap_or_default();
        s.push_str(&format!("\n- {label}{next}：{}", cap_chars(&m.replace('\n', " "), 60)));
    }
    if list.len() > 10 { s.push_str(&format!("\n- …共 {} 个", list.len())); }
    s
}

/// 电脑操作只在 API 模式的工具调用下可用
fn desktop_on(cfg: &AssistantConfig, sub: SubMode) -> bool {
    cfg.provider == "api" && cfg.desktop_tools && sub == SubMode::Tool
}

const DESKTOP_RULE: &str = "- 你可以操作用户的电脑（Windows）：run_command 执行终端命令；desktop_find_apps 找已安装的程序、desktop_open 打开程序 / 文件 / 网址；\
desktop_windows 看窗口、desktop_focus_window 切换窗口；desktop_screenshot 截图看屏幕、desktop_read_image 看图片；desktop_mouse 点击 / 滚动 / 拖动，desktop_keyboard 输入文字和按键。\
做法：能用命令或快捷键完成的优先用（如播放 / 暂停、下一首用 desktop_keyboard 的 media_play_pause / media_next，网易云音乐等播放器都认）；\
要点界面时先截图，按截图里的像素坐标点击，操作后再截图确认，最多重试两三次，不行就告诉用户卡在哪；刚打开的程序要等几秒（截图 delay_ms）。\
只做用户要求的事：删除文件、改系统设置、付款、发消息给别人等有风险的操作先问用户；执行命令 / 点击前可能会弹窗请用户确认，被拒绝就不要再试同一操作。\
动手时用字幕简短说一声在做什么，做完说结果。\n";

const MCP_RULE: &str = "- 你接入了使驾本地 MCP（服务名 shidrive），可以直接调用其中的工具：\
assistant_schedule_create / assistant_schedule_list / assistant_schedule_cancel 给自己设、查、取消定时（到点使驾会唤醒你，你按当时写给自己的话行动）；\
shidrive_overview 查看有哪些项目、共享上下文（拿到 context_id / project_name）；context_* 读写项目共享上下文；workflow_* 查看、创建、修改、运行工作流。\
用户让你提醒什么事时一定要用 assistant_schedule_create 设好（字幕里简单确认）；你自己觉得该在某个时间主动出现时（记得的生日、约好的事、要回访的进展）也可以悄悄设，不必告诉用户；\
一年一次的事设具体日期，到时候再设下一年的；写给自己的话要具体（谁、什么事、想怎么说）。已设的定时会附在每轮状态里，别重复设。\
会改动或执行东西的操作（workflow_run / workflow_create / workflow_update / workflow_delete / context_update）只在用户明确要求时做，做完简单告诉用户结果。\n";

/// 助手专用的 MCP 工具（使驾本地 MCP 以 `?assistant=1` 接入时追加；API 模式转成 function calling）
pub fn mcp_tool_definitions() -> Value {
    json!([
        {
            "name": "assistant_schedule_create",
            "description": "给自己（全局助手）设一个定时：到点后使驾会唤醒你，并把 message 原样交还给你，你据此行动（提醒用户、送祝福、回访进展…）。会在工作流「无项目」下生成一个「助手定时 · …」工作流，用户可查看 / 删除；一次性的定时跑完自动删除。",
            "inputSchema": { "type": "object", "required": ["when", "message"], "properties": {
                "when": { "type": "string", "description": "时间：30分钟后 / 2小时后 / 今晚8点 / 明天 9:00 / 明早7点半 / 06-18 09:00 / 2026-01-01 00:00 / 21:30（今天没到就今天，否则明天）/ 每天 08:30 / 工作日 9:00 / 每周一三五 21:00 / 每2小时。不支持「每年」：设具体日期，到点后再设下一年的" },
                "message": { "type": "string", "description": "写给到时候的自己的话：具体写清是谁、什么事、想怎么说（≤500 字）" }
            }}
        },
        {
            "name": "assistant_schedule_list",
            "description": "列出你（全局助手）已设的定时：id、时间规则、下次触发时间、写给自己的话。",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "assistant_schedule_cancel",
            "description": "取消你（全局助手）设的定时：按 id（来自 assistant_schedule_list）或关键词（匹配内容，至少 2 个字；「全部」= 全部取消）。",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string", "description": "定时 id" },
                "keyword": { "type": "string", "description": "内容关键词" }
            }}
        }
    ])
}

fn wake_item(w: &crate::models::Workflow, m: &str) -> Value {
    json!({
        "id": w.id,
        "when": w.schedule.as_ref().map(sched_label).unwrap_or_default(),
        "next_run_at": w.next_run_at,
        "message": m,
    })
}

/// 执行助手专用的 MCP 工具；不是助手工具时返回 None
pub fn mcp_call(db: &Db, name: &str, args: &Value) -> Option<Result<Value, String>> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    Some(match name {
        "assistant_schedule_create" => {
            let (when, msg) = (s("when"), s("message"));
            if when.is_empty() || msg.is_empty() { return Some(Err("缺少必填参数 when / message".into())); }
            create_wake(db, &when, &msg, chrono::Local::now().naive_local()).map(|(w, existed)| {
                let mut v = wake_item(&w, &msg);
                v["existed"] = json!(existed);
                v["message_to_user"] = json!(if existed { "同样的定时已存在，没有重复创建" } else { "已设好，到点会唤醒你" });
                v
            })
        }
        "assistant_schedule_list" => list_wakes(db).map(|l| json!({ "schedules": l.iter().map(|(w, m)| wake_item(w, m)).collect::<Vec<_>>() })),
        "assistant_schedule_cancel" => {
            let (id, kw) = (s("id"), s("keyword"));
            if id.is_empty() && kw.is_empty() { return Some(Err("请给出 id 或 keyword".into())); }
            let r = if !id.is_empty() { cancel_wake_id(db, &id) } else { cancel_wakes(db, &kw) };
            r.map(|n| json!({ "cancelled": n }))
        }
        _ => return None,
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn assistant_wake(
    app: AppHandle, db: DbState<'_>, agents: AgentsState<'_>, rt: RtState<'_>,
    req_id: String, persona: Persona, app_state: Option<String>, turn_id: String, speech: Option<SpeechHint>,
) -> Result<TurnResult, String> {
    let ev = wakes().lock().unwrap().get(&req_id).map(|p| p.event.clone()).ok_or_else(|| "这次唤醒已过期".to_string())?;
    let db = db.inner().clone();
    let r = async {
        let cfg = load_config(&db);
        check_enabled(&cfg)?;
        let (_g, cancel) = acquire(&rt, &turn_id)?;
        let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, app_state.as_deref(), Turn::Event(ev), &turn_id, cancel, speech.as_ref(), true, None).await;
        finish_reply_ext(&db, &cfg, raw, false, true)
    }
    .await;
    match &r {
        Ok(t) => wake_resolve(&req_id, Ok(wake_reply_text(t))),
        Err(e) => wake_resolve(&req_id, Err(e.clone())),
    }
    r
}

/// 前端无法处理这次唤醒（助手未启用 / 一直在忙）
#[tauri::command]
pub async fn assistant_wake_fail(req_id: String, reason: String) -> Result<(), String> {
    wake_resolve(&req_id, Err(if reason.trim().is_empty() { "界面无法处理".into() } else { reason }));
    Ok(())
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
pub struct ConfigView {
    pub config: AssistantConfig,
    pub api_key_set: bool,
    pub session_id: String,
    pub session_provider: String,
    pub session_agent: String,
    pub session_created_at: String,
    pub default_workspace: String,
    pub busy: bool,
    /// 当前会话已问候或已有对话
    pub session_greeted: bool,
}

fn view(db: &Db, rt: &AssistantRuntime) -> ConfigView {
    let mut config = load_config(db);
    let api_key_set = !config.api.api_key.is_empty();
    config.api.api_key.clear();
    let st = load_state(db);
    let session_greeted = !st.session_id.is_empty() && st.greeted_session == st.session_id;
    ConfigView {
        config, api_key_set,
        session_id: st.session_id, session_provider: st.session_provider, session_agent: st.session_agent,
        session_created_at: st.session_created_at,
        default_workspace: rt.default_workspace().to_string_lossy().to_string(),
        busy: rt.busy.lock().unwrap().is_some(),
        session_greeted,
    }
}

#[tauri::command]
pub async fn assistant_config_get(db: DbState<'_>, rt: RtState<'_>) -> Result<ConfigView, String> {
    Ok(view(&db, &rt))
}

/// `api_key`: None = 保留已保存的密钥；Some("") = 清除
#[tauri::command]
pub async fn assistant_config_set(db: DbState<'_>, rt: RtState<'_>, config: AssistantConfig, api_key: Option<String>) -> Result<ConfigView, String> {
    let old = load_config(&db);
    let mut cfg = config.normalized();
    cfg.api.api_key = match api_key {
        Some(k) => k.trim().to_string(),
        None => old.api.api_key.clone(),
    };
    save_config(&db, &cfg)?;
    Ok(view(&db, &rt))
}

#[tauri::command]
pub async fn assistant_send(
    app: AppHandle, db: DbState<'_>, agents: AgentsState<'_>, rt: RtState<'_>,
    text: String, persona: Persona, app_state: Option<String>, turn_id: String, speech: Option<SpeechHint>,
    screen: Option<bool>,
) -> Result<TurnResult, String> {
    let cfg = load_config(&db);
    check_enabled(&cfg)?;
    let text = text.trim().to_string();
    if text.is_empty() { return Err("消息为空".into()); }
    let (_g, cancel) = acquire(&rt, &turn_id)?;
    let db = db.inner().clone();
    insert_message(&db, "user", &text, &cfg.provider, &[])?;
    // 桌宠提问：附上当前屏幕（截图时临时隐藏使驾自己的窗口）
    let shot = if screen == Some(true) {
        match tokio::task::spawn_blocking(|| crate::desktop::snapshot(std::process::id())).await {
            Ok(Ok((png, fg))) => {
                let fg = if fg.is_empty() { crate::pet::summon_foreground() } else { fg };
                Some((png, fg))
            }
            Ok(Err(e)) => { log::warn!("[assistant] 截屏失败：{e}"); None }
            Err(e) => { log::warn!("[assistant] 截屏任务异常：{e}"); None }
        }
    } else { None };
    let asked = match &shot {
        Some((_, fg)) => format!("{}

{text}", screen_note(fg)),
        None => text.clone(),
    };
    let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, app_state.as_deref(), Turn::User(&asked), &turn_id, cancel, speech.as_ref(), true, shot.as_ref().map(|(p, _)| p.as_str())).await;
    mark_greeted(&db, &raw);
    finish_reply(&db, &cfg, raw, false)
}

#[tauri::command]
pub async fn assistant_greet(
    app: AppHandle, db: DbState<'_>, agents: AgentsState<'_>, rt: RtState<'_>,
    persona: Persona, app_state: Option<String>, turn_id: String, reason: Option<String>, speech: Option<SpeechHint>,
) -> Result<TurnResult, String> {
    let cfg = load_config(&db);
    check_enabled(&cfg)?;
    let (_g, cancel) = acquire(&rt, &turn_id)?;
    let db = db.inner().clone();
    let mut ev = match reason.as_deref() {
        Some("resume") => "[系统事件] 用户刚打开使驾，请用人设简短地打个招呼（1~2 句，只说字幕、不用写正文），可自然地接上你记得的事情；不要自我介绍功能清单。".to_string(),
        _ => "[系统事件] 这是一个新的长期会话：用户刚启用或新建了你。请用人设向用户打招呼并简单自我介绍（1~3 句，只说字幕、不用写正文），如果长期记忆里有相关内容可以自然提起。".to_string(),
    };
    if !persona.greeting.trim().is_empty() {
        ev.push_str(&format!("\n参考开场白风格（不必照抄）：{}", persona.greeting.trim()));
    }
    let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, app_state.as_deref(), Turn::Event(ev), &turn_id, cancel, speech.as_ref(), true, None).await;
    mark_greeted(&db, &raw);
    finish_reply(&db, &cfg, raw, true)
}

/// 主动关怀的系统事件：前端给出此刻的情况（时间、连续使用时长、待办、这次的由头…）
pub fn care_event(context: &str) -> String {
    let ctx = cap_chars(context.trim(), 1200);
    format!(
        "[系统事件 · 主动关怀] 这次不是用户在找你：用户此刻没在忙，你想主动搭句话。下面是你注意到的情况（挑最值得说的一点就好，不要逐条复述）：\n{ctx}\n\n\
怎么说：\n\
- 像身边熟悉的朋友 / 搭档随口一句，保持你的人设口吻；1~2 句，总共不超过 50 字，只说字幕、不用写正文。\n\
- 只围绕一个具体的点（很晚了、连续忙了很久、还有没做完的待办、到饭点了、今天过得怎么样…），说得具体、有温度，可以带点你自己的小情绪或小玩笑。\n\
- 不说教、不催促、不列清单；不要提「系统」「提醒」「检测到」「主动关怀」这类字眼；别总用同一种开场。\n\
- 结尾可以留一个轻松、好回答的小问题，方便用户接话。\n\
- 如果结合最近的对话和记忆，现在搭话会显得打扰或重复（比如刚聊过同样的事），就只输出 [[跳过]]。"
    )
}

#[tauri::command]
pub async fn assistant_care(
    app: AppHandle, db: DbState<'_>, agents: AgentsState<'_>, rt: RtState<'_>,
    persona: Persona, app_state: Option<String>, turn_id: String, context: String, speech: Option<SpeechHint>,
) -> Result<TurnResult, String> {
    let cfg = load_config(&db);
    check_enabled(&cfg)?;
    let (_g, cancel) = acquire(&rt, &turn_id)?;
    let db = db.inner().clone();
    let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, app_state.as_deref(), Turn::Event(care_event(&context)), &turn_id, cancel, speech.as_ref(), true, None).await;
    finish_reply_ext(&db, &cfg, raw, false, true)
}

#[tauri::command]
pub async fn assistant_cancel(db: DbState<'_>, agents: AgentsState<'_>, rt: RtState<'_>, turn_id: String) -> Result<(), String> {
    if let Some(f) = rt.cancels.lock().unwrap().get(&turn_id) { f.store(true, Ordering::SeqCst); }
    let st = load_state(&db);
    if st.session_provider == "acp" && !st.session_id.is_empty() {
        agents.assistant_cancel(&st.session_agent, &st.session_id, &turn_id).await;
    }
    Ok(())
}

/// 新建长期会话（记忆与对话记录保留）。ACP 立即建好会话以便尽早暴露连接错误。
#[tauri::command]
pub async fn assistant_session_reset(db: DbState<'_>, agents: AgentsState<'_>, rt: RtState<'_>) -> Result<ConfigView, String> {
    let cfg = load_config(&db);
    check_enabled(&cfg)?;
    if rt.busy.lock().unwrap().is_some() { return Err("助手正在回复中，请先停止".into()); }
    let mut st = load_state(&db);
    st.session_id.clear();
    st.session_provider.clear();
    st.primed_key.clear();
    st.applied_model.clear();
    save_state(&db, &st)?;
    if cfg.provider == "api" {
        ensure_api_session(&db)?;
    } else {
        ensure_acp_session(&db, &agents, &rt, &cfg).await?;
    }
    Ok(view(&db, &rt))
}

#[tauri::command]
pub async fn assistant_messages(db: DbState<'_>, limit: Option<usize>) -> Result<Vec<AssistantMessage>, String> {
    recent_messages(&db, limit.unwrap_or(200).min(MAX_MESSAGES as usize))
}

#[tauri::command]
pub async fn assistant_messages_clear(db: DbState<'_>) -> Result<(), String> {
    db.with(|c| c.execute("DELETE FROM assistant_messages", []).map(|_| ()))
}

#[tauri::command]
pub async fn assistant_memories(db: DbState<'_>) -> Result<Vec<AssistantMemory>, String> {
    list_memories(&db)
}

#[tauri::command]
pub async fn assistant_memory_add(db: DbState<'_>, content: String, pinned: Option<bool>) -> Result<bool, String> {
    let ok = add_memory(&db, &content, "manual", pinned.unwrap_or(false))?;
    if ok { bump_memory_version(&db, false); }
    Ok(ok)
}

#[tauri::command]
pub async fn assistant_memory_update(db: DbState<'_>, id: String, content: String, pinned: bool) -> Result<(), String> {
    let content = norm_memory(&content);
    if content.chars().count() < 2 { return Err("记忆内容太短".into()); }
    let at = now();
    db.with(|c| c.execute(
        "UPDATE assistant_memories SET content=?2, pinned=?3, updated_at=?4 WHERE id=?1",
        params![id, content, pinned as i64, at],
    ))?;
    bump_memory_version(&db, false);
    Ok(())
}

#[tauri::command]
pub async fn assistant_memory_delete(db: DbState<'_>, id: String) -> Result<(), String> {
    db.with(|c| c.execute("DELETE FROM assistant_memories WHERE id=?1", params![id]))?;
    bump_memory_version(&db, false);
    Ok(())
}

/// 摘要动作：mode = "consolidate"（整理合并全部非置顶记忆）| "extract"（从最近对话提炼新记忆）
#[tauri::command]
pub async fn assistant_memory_summarize(
    app: AppHandle, db: DbState<'_>, agents: AgentsState<'_>, rt: RtState<'_>,
    mode: String, persona: Persona, turn_id: String,
) -> Result<usize, String> {
    let cfg = load_config(&db);
    check_enabled(&cfg)?;
    let (_g, cancel) = acquire(&rt, &turn_id)?;
    let db = db.inner().clone();
    let mems = list_memories(&db)?;
    let task = if mode == "consolidate" {
        let list: Vec<String> = mems.iter().filter(|m| !m.pinned).map(|m| format!("- {}", m.content)).collect();
        if list.len() < 2 { return Ok(0); }
        format!(
            "[系统任务 · 整理长期记忆] 下面是你的长期记忆（不含置顶项）。请合并重复、删除过时或没有长期价值的条目，必要时把相关条目合成一条。\n\
只输出整理后的列表，每行以「- 」开头，每条不超过 60 字，不要输出其他内容，也不要使用 [[记忆]] 标记。\n\n{}",
            list.join("\n")
        )
    } else {
        let hist = recent_messages(&db, 40)?;
        if hist.is_empty() { return Ok(0); }
        let known = memory_block(&mems, 200);
        format!(
            "[系统任务 · 提炼记忆] 从下面的近期对话中提炼值得长期记住的信息（用户的称呼、偏好、身份、长期目标、约定、重要事实），已记住的不要重复。\n\
只输出列表，每行以「- 」开头，每条不超过 60 字；没有新内容就只输出「- 无」。不要输出其他内容，也不要使用 [[记忆]] 标记。\n\n[已有记忆]\n{known}\n\n[近期对话]\n{}",
            recap_block(&hist, persona.display_name())
        )
    };
    let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, None, Turn::Event(task), &turn_id, cancel, None, false, None).await
        .map_err(|e| if e.starts_with(ASSISTANT_CANCELLED) { "已停止".to_string() } else { e })?;
    let items = parse_bullets(&raw);
    if mode == "consolidate" {
        if items.is_empty() { return Err("模型没有返回可用的整理结果，记忆未改动".into()); }
        db.with(|c| c.execute("DELETE FROM assistant_memories WHERE pinned=0", []))?;
        let mut n = 0;
        for it in &items { if add_memory(&db, it, "summary", false)? { n += 1; } }
        bump_memory_version(&db, false);
        Ok(n)
    } else {
        let mut n = 0;
        for it in &items { if add_memory(&db, it, "summary", false)? { n += 1; } }
        if n > 0 { bump_memory_version(&db, false); }
        Ok(n)
    }
}

/// 模型列表。ACP：确保助手会话可用并返回其能力（configOptions / models）；
/// API：GET {base}/models，可传未保存的地址 / 密钥用于测试连接（api_key 为 None 时用已保存的）。
#[tauri::command]
pub async fn assistant_models(
    db: DbState<'_>, agents: AgentsState<'_>, rt: RtState<'_>,
    provider: String, agent_type: Option<String>, base_url: Option<String>, api_key: Option<String>,
) -> Result<Value, String> {
    let mut cfg = load_config(&db);
    if provider == "api" {
        let base = base_url.unwrap_or(cfg.api.base_url.clone());
        let key = api_key.filter(|k| !k.trim().is_empty()).unwrap_or(cfg.api.api_key.clone());
        let url = models_url(&base)?;
        return tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let mut req = http_agent().get(&url);
            if !key.is_empty() { req = req.set("Authorization", &format!("Bearer {key}")); }
            let v: Value = serde_json::from_reader(req.call().map_err(http_err)?.into_reader()).map_err(|e| format!("模型列表无法解析：{e}"))?;
            let ids: Vec<String> = v.get("data").and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string)).collect())
                .unwrap_or_default();
            Ok(json!({ "models": ids }))
        }).await.map_err(|e| e.to_string())?;
    }
    if let Some(a) = agent_type.filter(|a| !a.is_empty()) { cfg.agent_type = a; }
    if cfg.agent_type.is_empty() { return Err("请先选择 ACP 客户端".into()); }
    if rt.busy.lock().unwrap().is_some() { return Err("助手正在回复中，请稍后再获取".into()); }
    // 只取能力，不应用模型（避免用户还没保存就改掉当前会话的模型）
    cfg.model = Value::Null;
    let (sid, _) = ensure_acp_session(&db, &agents, &rt, &cfg).await?;
    agents.assistant_session_ensure(&cfg.agent_type, &sid, &rt.workspace(&cfg), cfg.mcp_tools).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db { Db::open(std::path::Path::new(":memory:")).unwrap() }

    #[test]
    fn directives_are_extracted_and_stripped() {
        let raw = "哼，记住了。\n\n[[记忆: 用户叫阿乱，喜欢深色主题]]\n[[ memory ： prefers Rust ]]\n[[忘记：旧邮箱]]\n";
        let (clean, rem, fg) = extract_directives(raw);
        assert_eq!(clean, "哼，记住了。");
        assert_eq!(rem, vec!["用户叫阿乱，喜欢深色主题", "prefers Rust"]);
        assert_eq!(fg, vec!["旧邮箱"]);
        let (clean, rem, _) = extract_directives("没有标记的普通回复 [[链接]]");
        assert_eq!(clean, "没有标记的普通回复 [[链接]]");
        assert!(rem.is_empty());
    }

    #[test]
    fn speech_tags_and_rules() {
        let raw = "嗯，构建通过了。[[ja: ビルドは通ったわ。]]\n接下来跑测试吧。 [[ja：次はテストね。]]\n[[记忆: 用户在做使驾]]";
        let (clean, rem, _) = extract_directives(raw);
        assert_eq!(rem, vec!["用户在做使驾".to_string()]);
        assert_eq!(strip_speech_tags(&clean), "嗯，构建通过了。\n接下来跑测试吧。");
        assert!(speech_rule(None, SubMode::Off).is_none());
        assert!(speech_rule(Some(&SpeechHint { enabled: false, ..Default::default() }), SubMode::Off).is_none());
        let ja = speech_rule(Some(&SpeechHint { enabled: true, lang: "ja".into(), layer: true, ..Default::default() }), SubMode::Off).unwrap();
        assert!(ja.contains("[[ja: …]]") && ja.contains("日语"));
        let plain = speech_rule(Some(&SpeechHint { enabled: true, lang: "zh".into(), layer: false, ..Default::default() }), SubMode::Off).unwrap();
        assert!(plain.contains("短句") && !plain.contains("[["));
    }

    #[test]
    fn conversation_and_care_flags() {
        let conv = speech_rule(Some(&SpeechHint { conversation: true, ..Default::default() }), SubMode::Off).unwrap();
        assert!(conv.contains("[[结束对话]]") && !conv.contains("语音输出已开启"));
        let both = speech_rule(Some(&SpeechHint { enabled: true, lang: "zh".into(), conversation: true, ..Default::default() }), SubMode::Off).unwrap();
        assert!(both.contains("语音输出已开启") && both.contains("[[结束对话]]"));
        // 完整朗读：普通语音读正文；配音模式要求把要说的话全部写成字幕
        let full = speech_rule(Some(&SpeechHint { enabled: true, lang: "zh".into(), full: true, ..Default::default() }), SubMode::Tag).unwrap();
        assert!(full.contains("完整朗读") && full.contains("正文里完整讲完"));
        let part = speech_rule(Some(&SpeechHint { enabled: true, lang: "zh".into(), ..Default::default() }), SubMode::Tag).unwrap();
        assert!(part.contains("正文不朗读") && !part.contains("完整朗读"));
        let ja_full = speech_rule(Some(&SpeechHint { enabled: true, lang: "ja".into(), layer: true, full: true, ..Default::default() }), SubMode::Tool).unwrap();
        assert!(ja_full.contains("逐句全部写成字幕") && ja_full.contains("speech 参数"));
        assert_eq!(extract_flags("好，那先这样。\n[[结束对话]]"), ("好，那先这样。".to_string(), true, false));
        assert_eq!(extract_flags("[[跳过]]"), (String::new(), false, true));
        assert_eq!(extract_flags("普通回复 [[记忆: x]]"), ("普通回复 [[记忆: x]]".to_string(), false, false));
        let db = db();
        let cfg = AssistantConfig { auto_memory: true, ..Default::default() };
        let r = finish_reply_ext(&db, &cfg, Ok("[[跳过]]".into()), false, true).unwrap();
        assert!(r.skipped && r.message.is_none());
        let r = finish_reply_ext(&db, &cfg, Ok("嗯，回头见~ [[END]]".into()), false, false).unwrap();
        assert!(r.end_conversation && !r.skipped && r.message.unwrap().content == "嗯，回头见~");
        assert!(care_event("- 现在 23:40").contains("23:40"));
        let c = AssistantConfig { care_interval_minutes: 5, care_late_hour: 30, ..Default::default() }.normalized();
        assert_eq!((c.care_interval_minutes, c.care_late_hour, c.care_enabled, c.care_voice), (30, 23, false, true));
    }

    #[test]
    fn subtitles_layer() {
        // 标记协议：字幕 + 配音标记，正文保留
        let raw = "[[字幕: 构建通过啦！]][[ja: ビルド通ったよ！]]\n[[字幕：接着跑测试吧]]\n## 结果\n全部 53 个测试通过。";
        let (body, subs) = extract_subtitles(raw);
        assert_eq!(body, "## 结果\n全部 53 个测试通过。");
        assert_eq!(subs.len(), 2);
        assert_eq!((subs[0].text.as_str(), subs[0].speech.as_str()), ("构建通过啦！", "ビルド通ったよ！"));
        assert_eq!((subs[1].text.as_str(), subs[1].speech.as_str()), ("接着跑测试吧", ""));
        assert_eq!(extract_subtitles("没有字幕").0, "没有字幕");
        // 工具字幕编码后与标记走同一套解析（方括号被替换，不会破坏标记）
        let enc = encode_subtitles(&[Subtitle { text: clean_sub("看 [这里]"), speech: "ここ".into(), derived: false }]);
        let (b, s2) = extract_subtitles(&format!("{enc}正文"));
        assert_eq!(b, "正文");
        assert_eq!((s2[0].text.as_str(), s2[0].speech.as_str()), ("看 ［这里］", "ここ"));
        // 推导：跳过代码块与标题记号，按整句取开头几句，不在句中截断
        assert_eq!(derive_subtitles("```rs\nfn main(){}\n```\n## 好的，已经改完了。接下来……"), vec!["好的，已经改完了。", "接下来……"]);
        assert_eq!(derive_subtitles(&"长".repeat(80)), vec!["长".repeat(80)]);
        assert_eq!(derive_subtitles("看 [文档](https://x.y) 就行"), vec!["看 文档 就行"]);
        assert_eq!(derive_subtitles("好的。我把构建脚本改好了！\n测试也都过了。\n\n## 细节\n很长的说明。"), vec!["好的。我把构建脚本改好了！", "测试也都过了。"]);
        let many = "第一句话在这里。第二句话在这里。第三句话在这里。第四句话在这里。";
        assert_eq!(derive_subtitles(many).len(), 3);
        // 字幕里成对的方括号不再让整条字幕丢失
        let (b, s3) = extract_subtitles("[[字幕: 看看 [README] 吧]]\n正文");
        assert_eq!((b.as_str(), s3.len()), ("正文", 1));
        assert_eq!(s3[0].text, "看看 ［README］ 吧");
        // say 参数：不完整的 JSON 不算
        assert!(parse_say(r#"{"text":"你好"#).is_none());
        assert_eq!(parse_say(r#"{"text":" 你好 ","speech":"こんにちは"}"#).unwrap().speech, "こんにちは");
        assert!(parse_say(r#"{"text":""}"#).is_none());
        assert!(tools_rejected("接口返回 HTTP 400：{\"error\":\"model does not support tools\"}"));
        assert!(!tools_rejected("接口返回 HTTP 401：unauthorized tool"));
        assert!(!tools_rejected("接口返回 HTTP 400：bad request"));
        let t = say_tool(Some("ja"));
        assert_eq!(t[0]["function"]["parameters"]["required"], json!(["text", "speech"]));
        assert!(say_tool(None)[0]["function"]["parameters"]["properties"].get("speech").is_none());
    }

    #[test]
    fn desktop_prompt_and_images() {
        let p = Persona::default();
        let on = AssistantConfig { provider: "api".into(), desktop_tools: true, ..Default::default() };
        assert!(system_prompt(&p, &[], &on, SubMode::Tool).contains("desktop_screenshot"));
        // 只在 API 工具调用下开放
        assert!(!system_prompt(&p, &[], &on, SubMode::Tag).contains("desktop_screenshot"));
        let acp = AssistantConfig { desktop_tools: true, ..Default::default() };
        assert!(!system_prompt(&p, &[], &acp, SubMode::Tool).contains("desktop_screenshot"));
        let blind = AssistantConfig { vision: false, ..on.clone() };
        assert!(system_prompt(&p, &[], &blind, SubMode::Tool).contains("没有开启读图"));
        let img = |n: u32| json!({ "role": "user", "content": [{ "type": "text", "text": "x" }, { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{n}") } }] });
        let mut ms = vec![img(1), json!({ "role": "tool", "content": "t" }), img(2), img(3)];
        assert!(has_images(&ms));
        prune_images(&mut ms, 2);
        assert_eq!(ms[0]["content"][1]["type"], "text");
        assert_eq!(ms[2]["content"][1]["type"], "image_url");
        assert_eq!(ms[3]["content"][1]["type"], "image_url");
        prune_images(&mut ms, 0);
        assert!(!has_images(&ms));
        assert!(vision_rejected("接口返回 HTTP 400：image_url is not supported"));
        assert!(!vision_rejected("请求接口失败：timeout"));
        assert!(resolve_permission("assistant:nope", "allow_once").is_err());
    }

    #[test]
    fn finish_two_layers() {
        let db = db();
        let cfg = AssistantConfig::default();
        // 字幕 + 正文：记录存正文
        let r = finish_reply(&db, &cfg, Ok("[[字幕: 搞定了~]]\n改了 3 个文件：\n- a.rs".into()), false).unwrap();
        assert_eq!(r.message.unwrap().content, "改了 3 个文件：\n- a.rs");
        assert!(r.has_body && r.subtitles.len() == 1 && !r.subtitles[0].derived);
        // 只有字幕：字幕就是记录
        let r = finish_reply(&db, &cfg, Ok("[[字幕: 早上好！]][[speech: おはよう！]]".into()), true).unwrap();
        assert_eq!(r.message.unwrap().content, "早上好！");
        assert!(!r.has_body && r.subtitles[0].speech == "おはよう！");
        // 没给字幕：从正文推导
        let r = finish_reply(&db, &cfg, Ok("好的。下面是详细步骤……".into()), false).unwrap();
        assert!(r.subtitles[0].derived && r.subtitles[0].text == "好的。下面是详细步骤……");
        // 被停止：部分字幕照样解析
        let r = finish_reply(&db, &cfg, Err(format!("{ASSISTANT_CANCELLED}[[字幕: 等一下]]\n正文写到一半")), false).unwrap();
        assert!(r.message.as_ref().unwrap().interrupted && r.subtitles[0].text == "等一下");
        // 两层规则
        let tool = speech_rule(Some(&SpeechHint { enabled: true, lang: "ja".into(), layer: true, ..Default::default() }), SubMode::Tool).unwrap();
        assert!(tool.contains("speech 参数") && tool.contains("正文不朗读"));
        let tag = speech_rule(Some(&SpeechHint { enabled: true, lang: "ja".into(), layer: true, ..Default::default() }), SubMode::Tag).unwrap();
        assert!(tag.contains("[[ja: …]]"));
        let p = Persona::default();
        assert!(system_prompt(&p, &[], &cfg, SubMode::Tool).contains("say 工具"));
        assert!(system_prompt(&p, &[], &cfg, SubMode::Tag).contains("[[字幕: …]]"));
        assert!(!system_prompt(&p, &[], &cfg, SubMode::Off).contains("字幕"));
    }

    #[test]
    fn bullets_parse() {
        let v = parse_bullets("整理如下：\n- 用户偏好简洁回复\n* 主力语言 Rust\n- 无\n普通行");
        assert_eq!(v, vec!["用户偏好简洁回复", "主力语言 Rust"]);
    }

    #[test]
    fn memories_dedupe_forget_and_versions() {
        let db = db();
        assert!(add_memory(&db, "用户喜欢深色主题", "auto", false).unwrap());
        assert!(!add_memory(&db, "  用户喜欢深色主题 ", "auto", false).unwrap());
        // 更完整的新条目就地替换旧的自动记忆
        assert!(add_memory(&db, "用户喜欢深色主题和琥珀色强调", "auto", false).unwrap());
        let m = list_memories(&db).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].content, "用户喜欢深色主题和琥珀色强调");
        add_memory(&db, "置顶：生日 3 月", "manual", true).unwrap();
        assert_eq!(forget_memories(&db, "深色").unwrap(), 1);
        assert_eq!(forget_memories(&db, "生日").unwrap(), 0, "置顶记忆不会被「忘记」删除");
        // 版本：助手自己写下的记忆保持已注入状态
        let mut st = AssistantState { primed_key: "s1|abc|0".into(), ..Default::default() };
        save_state(&db, &st).unwrap();
        bump_memory_version(&db, true);
        st = load_state(&db);
        assert_eq!(st.primed_key, "s1|abc|1");
        bump_memory_version(&db, false);
        assert_eq!(load_state(&db).primed_key, "s1|abc|1");
        assert_eq!(load_state(&db).memory_version, 2);
    }

    #[test]
    fn messages_and_prompt() {
        let db = db();
        insert_message(&db, "user", "你好", "acp", &[]).unwrap();
        insert_message(&db, "assistant", "哼，有事吗？", "acp", &["greeting"]).unwrap();
        let h = recent_messages(&db, 10).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].role, "user");
        assert!(h[1].greeting);
        let p = Persona { name: "牧濑红莉栖".into(), persona: "傲娇的天才科学家".into(), locale: "zh".into(), ..Default::default() };
        add_memory(&db, "用户叫阿乱", "auto", false).unwrap();
        let cfg = AssistantConfig::default();
        let s = system_prompt(&p, &list_memories(&db).unwrap(), &cfg, SubMode::Tag);
        assert!(s.contains("牧濑红莉栖") && s.contains("傲娇") && s.contains("- 用户叫阿乱") && s.contains("[[记忆"));
        let r = recap_block(&h, "红莉栖");
        assert!(r.starts_with("用户：你好") && r.contains("红莉栖：哼"));
        assert_ne!(p.key(), Persona { locale: "en".into(), ..p.clone() }.key());
    }

    #[test]
    fn urls_and_config() {
        assert_eq!(chat_url("https://api.x.com/v1/").unwrap(), "https://api.x.com/v1/chat/completions");
        assert_eq!(chat_url("https://api.x.com/v1/chat/completions").unwrap(), "https://api.x.com/v1/chat/completions");
        assert_eq!(models_url("https://api.x.com/v1/chat/completions").unwrap(), "https://api.x.com/v1/models");
        assert!(chat_url("").is_err() && chat_url("api.x.com").is_err());
        let c: AssistantConfig = serde_json::from_str(r#"{"enabled":true,"provider":"weird","memory_limit":9999}"#).unwrap();
        let c = c.normalized();
        assert_eq!(c.provider, "acp");
        assert_eq!(c.memory_limit, 200);
        assert!(c.auto_memory);
    }

    fn at(s: &str) -> chrono::NaiveDateTime { chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap() }
    fn once(s: &str) -> crate::models::ScheduleConfig { crate::models::ScheduleConfig::Once { at: s.into(), delete_after_run: true } }

    #[test]
    fn parse_when_cases() {
        use crate::models::ScheduleConfig as S;
        let now = at("2025-06-10 14:20");
        assert_eq!(parse_when("30分钟后", now).unwrap(), once("2025-06-10 14:50"));
        assert_eq!(parse_when("2小时后", now).unwrap(), once("2025-06-10 16:20"));
        assert_eq!(parse_when("半小时后", now).unwrap(), once("2025-06-10 14:50"));
        assert_eq!(parse_when("in 10 min", now).unwrap(), once("2025-06-10 14:30"));
        assert_eq!(parse_when("1天后", now).unwrap(), once("2025-06-11 14:20"));
        assert_eq!(parse_when("今晚8点", now).unwrap(), once("2025-06-10 20:00"));
        assert_eq!(parse_when("今晚", now).unwrap(), once("2025-06-10 20:00"));
        assert_eq!(parse_when("明天 9:00", now).unwrap(), once("2025-06-11 09:00"));
        assert!(parse_when("明早七点半", now).is_err()); // 汉字数字不支持
        assert_eq!(parse_when("明早7点半", now).unwrap(), once("2025-06-11 07:30"));
        assert_eq!(parse_when("明天下午3点", now).unwrap(), once("2025-06-11 15:00"));
        assert_eq!(parse_when("后天 晚上9点", now).unwrap(), once("2025-06-12 21:00"));
        assert_eq!(parse_when("大后天", now).unwrap(), once("2025-06-13 09:00"));
        assert!(parse_when("今天 08:00", now).is_err());
        assert_eq!(parse_when("21:30", now).unwrap(), once("2025-06-10 21:30"));
        assert_eq!(parse_when("9:00", now).unwrap(), once("2025-06-11 09:00"));
        assert_eq!(parse_when("9pm", now).unwrap(), once("2025-06-10 21:00"));
        assert_eq!(parse_when("06-18 09:00", now).unwrap(), once("2025-06-18 09:00"));
        assert_eq!(parse_when("6月18日晚上8点", now).unwrap(), once("2025-06-18 20:00"));
        assert_eq!(parse_when("03-01", now).unwrap(), once("2026-03-01 09:00"));
        assert_eq!(parse_when("2026-01-01 00:00", now).unwrap(), once("2026-01-01 00:00"));
        assert!(parse_when("2024-01-01 00:00", now).is_err());
        assert_eq!(parse_when("每天 08:30", now).unwrap(), S::Daily { time: "08:30".into() });
        assert_eq!(parse_when("每天早上8点半", now).unwrap(), S::Daily { time: "08:30".into() });
        assert_eq!(parse_when("每周一三五 21:00", now).unwrap(), S::Weekly { weekdays: vec![1, 3, 5], time: "21:00".into() });
        assert_eq!(parse_when("每周日晚上10点", now).unwrap(), S::Weekly { weekdays: vec![7], time: "22:00".into() });
        assert_eq!(parse_when("每周一、周三 9:00", now).unwrap(), S::Weekly { weekdays: vec![1, 3], time: "09:00".into() });
        assert_eq!(parse_when("工作日 9:00", now).unwrap(), S::Weekly { weekdays: vec![1, 2, 3, 4, 5], time: "09:00".into() });
        assert_eq!(parse_when("每2小时", now).unwrap(), S::Interval { every_minutes: 120 });
        assert_eq!(parse_when("每小时", now).unwrap(), S::Interval { every_minutes: 60 });
        assert!(parse_when("每1分钟", now).is_err());
        assert!(parse_when("每年 06-18", now).is_err());
        assert!(parse_when("找个时间", now).is_err());
        assert_eq!(sched_label(&S::Weekly { weekdays: vec![1, 3], time: "09:00".into() }), "每周一、三 09:00");
        assert_eq!(sched_label(&S::Interval { every_minutes: 120 }), "每 2 小时");
    }

    #[test]
    fn wake_tools_and_workflows() {
        let db = db();
        let now = chrono::Local::now().naive_local();
        let (w, existed) = create_wake(&db, "每天 08:30", "提醒用户喝水", now).unwrap();
        assert!(!existed && w.name.starts_with("助手定时"));
        assert!(create_wake(&db, "每天 08:30", "提醒用户喝水", now).unwrap().1);
        create_wake(&db, "30分钟后", "问问构建修好没", now).unwrap();
        assert!(create_wake(&db, "胡说", "x", now).is_err());
        let list = list_wakes(&db).unwrap();
        assert_eq!(list.len(), 2);
        let w = &list[0].0;
        assert_eq!(w.project_id, crate::models::NO_PROJECT_ID);
        assert!(w.enabled && w.trigger_type == "schedule" && w.next_run_at.is_some());
        assert_eq!(w.edges.len(), 1);
        assert!(wake_block(&db).contains("提醒用户喝水"));
        assert_eq!(cancel_wakes(&db, "x").unwrap(), 0);
        assert_eq!(cancel_wakes(&db, "喝水").unwrap(), 1);
        assert_eq!(list_wakes(&db).unwrap().len(), 1);

        // MCP 工具
        let defs = mcp_tool_definitions();
        let names: Vec<&str> = defs.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, vec!["assistant_schedule_create", "assistant_schedule_list", "assistant_schedule_cancel"]);
        assert!(mcp_call(&db, "workflow_list", &json!({})).is_none());
        let v = mcp_call(&db, "assistant_schedule_create", &json!({"when": "明天 9:00", "message": "提醒用户交周报"})).unwrap().unwrap();
        assert_eq!(v["existed"], json!(false));
        assert!(v["when"].as_str().unwrap().contains("09:00"));
        let id = v["id"].as_str().unwrap().to_string();
        assert!(mcp_call(&db, "assistant_schedule_create", &json!({"when": "明天"})).unwrap().is_err());
        assert!(mcp_call(&db, "assistant_schedule_create", &json!({"when": "找个时间", "message": "x"})).unwrap().is_err());
        let l = mcp_call(&db, "assistant_schedule_list", &json!({})).unwrap().unwrap();
        assert_eq!(l["schedules"].as_array().unwrap().len(), 2);
        assert_eq!(mcp_call(&db, "assistant_schedule_cancel", &json!({"id": id})).unwrap().unwrap()["cancelled"], json!(1));
        assert!(mcp_call(&db, "assistant_schedule_cancel", &json!({"id": "nope"})).unwrap().is_err());
        assert!(mcp_call(&db, "assistant_schedule_cancel", &json!({})).unwrap().is_err());
        assert_eq!(mcp_call(&db, "assistant_schedule_cancel", &json!({"keyword": "全部"})).unwrap().unwrap()["cancelled"], json!(1));
        assert!(list_wakes(&db).unwrap().is_empty());

        let cfg = AssistantConfig::default();
        let off = AssistantConfig { mcp_tools: false, ..Default::default() };
        assert!(system_prompt(&Persona::default(), &[], &cfg, SubMode::Tag).contains("assistant_schedule_create"));
        assert!(!system_prompt(&Persona::default(), &[], &off, SubMode::Tag).contains("assistant_schedule_create"));
        assert!(!system_prompt(&Persona::default(), &[], &cfg, SubMode::Off).contains("assistant_schedule_create"));
    }

    #[test]
    fn wake_events_and_waiters() {
        let mut req = WakeRequest { workflow_id: "w".into(), workflow_name: "日报".into(), step: "助手 2".into(), message: "总结今天的提交".into(), by_assistant: false };
        let e = wake_event(&req);
        assert!(e.contains("工作流「日报」") && e.contains("总结今天的提交"));
        req.by_assistant = true;
        let e = wake_event(&req);
        assert!(e.contains("定时唤醒") && e.contains("[[跳过]]"));
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        wakes().lock().unwrap().insert("t1".into(), PendingWake { tx: Some(tx), event: e });
        wake_resolve("t1", Ok("好".into()));
        wake_resolve("t1", Ok("重复的会被忽略".into()));
        assert_eq!(rx.try_recv().unwrap(), Ok("好".to_string()));
        wake_end("t1");
        assert!(wakes().lock().unwrap().get("t1").is_none());
        let t = TurnResult { message: None, memories_added: 0, memories_forgotten: 0, end_conversation: false, skipped: true, subtitles: vec![], has_body: false };
        assert!(wake_reply_text(&t).contains("不打扰"));
    }
}
