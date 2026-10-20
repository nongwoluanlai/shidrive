//! 全局助手：独立于项目 / 上下文的长期对话伙伴。
//!
//! - 大脑二选一：现有 ACP Agent（独立长期会话，不写 agent_bindings、不注入使驾 MCP），
//!   或自定义 OpenAI 兼容接口（/chat/completions 流式）。
//! - 对话记录与长期记忆都落在使驾自己的库里（assistant_messages / assistant_memories），
//!   与客户端会话解耦：换 ACP 客户端或切到 API 后，新会话首轮会带上人设 + 记忆 + 最近对话回顾。
//! - 记忆点由模型在回复末尾用 `[[记忆: …]]` / `[[忘记: …]]` 标出，这里解析落库并从展示文本中剥掉；
//!   另有「整理记忆」「从最近对话提炼」两个摘要动作。
//! - 人设来自前端（跟随皮肤看板角色或用户自定义），每轮随请求传入，变化时自动重新注入。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("- ") || l.starts_with("* ") || l.starts_with("• "))
        .map(norm_memory)
        .filter(|l| l.chars().count() >= 2 && l != "无" && !l.eq_ignore_ascii_case("none"))
        .collect()
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
pub fn system_prompt(p: &Persona, mems: &[AssistantMemory], cfg: &AssistantConfig) -> String {
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
- 回复会显示在桌面角色旁的对话气泡里：像聊天一样自然简短，一般 1~4 句；用户需要时再展开，可用 Markdown。\n\
- 始终保持人设口吻，但信息要准确；不确定就直说，不编造。\n\
- 这里是独立的助手会话，不是项目里的编程会话；除非用户明确要求，不要主动改动文件或执行命令。\n",
        lang_line(p)
    );
    if cfg.auto_memory {
        s.push_str("- 对话中出现值得长期记住的信息（用户的称呼、偏好、身份、长期目标、约定、重要事实）时，在回复最后单独一行写 [[记忆: 一句话摘要]]，可多条、每条一行；没有就不写。普通闲聊和一次性任务细节不要记。用户要求忘掉某事时写 [[忘记: 关键词]]。这些标记不会展示给用户。\n");
    }
    s.push_str("\n[长期记忆]（跨会话、跨客户端保存，按需参考，不必复述）\n");
    s.push_str(&memory_block(mems, cfg.memory_limit));
    s
}

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

pub fn speech_rule(h: Option<&SpeechHint>) -> Option<String> {
    let h = h?;
    let voice = voice_rule(h);
    if !h.conversation { return voice; }
    Some(match voice {
        Some(v) => format!("{v}\n{CONVERSATION_RULE}"),
        None => CONVERSATION_RULE.to_string(),
    })
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
    let re = regex::Regex::new(r"(?i)[ \t]*\[\[\s*(ja|en|ko|yue|zh)\s*[:：][^\[\]]*\]\]").unwrap();
    let out = re.replace_all(text, "").to_string();
    let mut lines: Vec<&str> = out.lines().map(|l| l.trim_end()).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) { lines.pop(); }
    let mut out = lines.join("\n");
    while out.contains("\n\n\n") { out = out.replace("\n\n\n", "\n\n"); }
    out.trim().to_string()
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
        match agents.assistant_session_ensure(&cfg.agent_type, &st.session_id, &cwd0).await {
            Ok(_) => {
                apply_model(db, agents, cfg, &mut st).await;
                return Ok((st.session_id.clone(), cwd0));
            }
            Err(e) => log::warn!("[assistant] 恢复长期会话失败，改为新建：{e}"),
        }
    }
    let (sid, _) = agents.assistant_session_new(&cfg.agent_type, &cwd).await?;
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

/// OpenAI 兼容 /chat/completions（优先流式；服务端不支持流式时按普通 JSON 解析）
fn api_chat(app: &AppHandle, api: &ApiConfig, messages: Vec<Value>, session: &str, turn: &str, cancel: &AtomicBool) -> Result<String, String> {
    let url = chat_url(&api.base_url)?;
    if api.model.is_empty() { return Err("请先在设置中填写模型名称".into()); }
    let mut body = json!({ "model": api.model, "messages": messages, "stream": true });
    if let Some(t) = api.temperature { body["temperature"] = json!(t); }
    let mut req = http_agent().post(&url).set("Content-Type", "application/json").set("Accept", "text/event-stream, application/json");
    if !api.api_key.is_empty() { req = req.set("Authorization", &format!("Bearer {}", api.api_key)); }
    let resp = req.send_string(&body.to_string()).map_err(http_err)?;
    let ct = resp.header("content-type").unwrap_or("").to_ascii_lowercase();
    let mut out = String::new();
    if !ct.contains("event-stream") {
        let v: Value = serde_json::from_reader(resp.into_reader()).map_err(|e| format!("接口返回无法解析：{e}"))?;
        if let Some(e) = v.get("error") { return Err(format!("接口错误：{}", cap_chars(&e.to_string(), 400))); }
        out = v.pointer("/choices/0/message/content").and_then(Value::as_str).unwrap_or_default().to_string();
        if !out.is_empty() { emit_chunk(app, session, turn, &out); }
        return Ok(out);
    }
    let reader = BufReader::new(resp.into_reader());
    for line in reader.lines() {
        if cancel.load(Ordering::SeqCst) { return Err(format!("{ASSISTANT_CANCELLED}{out}")); }
        let line = line.map_err(|e| format!("读取接口流失败：{e}"))?;
        let Some(data) = line.trim().strip_prefix("data:") else { continue };
        let data = data.trim();
        if data == "[DONE]" { break; }
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(e) = v.get("error") { return Err(format!("接口错误：{}", cap_chars(&e.to_string(), 400))); }
        if let Some(d) = v.pointer("/choices/0/delta/content").and_then(Value::as_str) {
            if !d.is_empty() {
                out.push_str(d);
                emit_chunk(app, session, turn, d);
            }
        }
    }
    if cancel.load(Ordering::SeqCst) { return Err(format!("{ASSISTANT_CANCELLED}{out}")); }
    Ok(out)
}

/// 执行一轮：组装提示词 → 发给 ACP / API → 返回原始回复文本（含指令标记）。
/// `Err` 以 ASSISTANT_CANCELLED 开头表示被停止（其后为部分文本）。
async fn run_turn(
    app: &AppHandle, db: &Arc<Db>, agents: &AgentManager, rt: &AssistantRuntime,
    cfg: &AssistantConfig, persona: &Persona, app_state: Option<&str>, turn: Turn<'_>,
    turn_id: &str, cancel: Arc<AtomicBool>, speech: Option<&SpeechHint>,
) -> Result<String, String> {
    let speech_line = speech_rule(speech);
    let mems = list_memories(db)?;
    let name = persona.display_name().to_string();
    let body = match &turn {
        Turn::User(t) => t.to_string(),
        Turn::Event(e) => e.clone(),
    };
    if cfg.provider == "api" {
        let st = ensure_api_session(db)?;
        let mut sys = format!("{}\n\n{}", system_prompt(persona, &mems, cfg), state_line(app_state, cfg));
        if let Some(l) = &speech_line { sys.push_str("\n"); sys.push_str(l); }
        let mut messages = vec![json!({ "role": "system", "content": sys })];
        // 本轮用户消息已先入库：历史里排除最后一条（就是它）
        let mut hist = recent_messages(db, cfg.history_turns + 1)?;
        if matches!(turn, Turn::User(_)) && hist.last().is_some_and(|m| m.role == "user") { hist.pop(); }
        for m in hist.iter().rev().take(cfg.history_turns).collect::<Vec<_>>().into_iter().rev() {
            let role = if m.role == "user" { "user" } else { "assistant" };
            messages.push(json!({ "role": role, "content": m.content }));
        }
        messages.push(json!({ "role": "user", "content": body }));
        let api = cfg.api.clone();
        let app2 = app.clone();
        let sid = st.session_id.clone();
        let tid = turn_id.to_string();
        return tokio::task::spawn_blocking(move || api_chat(&app2, &api, messages, &sid, &tid, &cancel))
            .await
            .map_err(|e| format!("接口任务异常：{e}"))?;
    }
    let (sid, cwd) = ensure_acp_session(db, agents, rt, cfg).await?;
    let st = load_state(db);
    let primed_key = format!("{sid}|{}|{}", persona.key(), st.memory_version);
    let mut text = String::new();
    if st.primed_key != primed_key {
        let same_session_and_persona = st.primed_key.starts_with(&format!("{sid}|{}|", persona.key()));
        if same_session_and_persona {
            // 只是记忆有变化（用户在面板里改了）：补发最新记忆
            text.push_str("[长期记忆已更新]\n");
            text.push_str(&memory_block(&mems, cfg.memory_limit));
        } else {
            text.push_str(&system_prompt(persona, &mems, cfg));
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
    if let Some(l) = &speech_line { text.push('\n'); text.push_str(l); }
    text.push_str("\n\n");
    match &turn {
        Turn::User(t) => { text.push_str("[用户消息]\n"); text.push_str(t); }
        Turn::Event(e) => text.push_str(e),
    }
    let r = agents.assistant_prompt(&cfg.agent_type, &sid, &cwd, &text, turn_id).await;
    // 只要请求已送达（成功或被停止）就视为设定已注入
    if r.is_ok() || r.as_ref().err().is_some_and(|e| e.starts_with(ASSISTANT_CANCELLED)) {
        let mut st = load_state(db);
        st.primed_key = format!("{sid}|{}|{}", persona.key(), st.memory_version);
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
        return Ok(TurnResult { message: None, memories_added: 0, memories_forgotten: 0, end_conversation: false, skipped: true });
    }
    let (clean, remember, forget) = extract_directives(&raw);
    let clean = strip_speech_tags(&clean);
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
    Ok(TurnResult { message, memories_added: added, memories_forgotten: forgotten, end_conversation, skipped: false })
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
) -> Result<TurnResult, String> {
    let cfg = load_config(&db);
    check_enabled(&cfg)?;
    let text = text.trim().to_string();
    if text.is_empty() { return Err("消息为空".into()); }
    let (_g, cancel) = acquire(&rt, &turn_id)?;
    let db = db.inner().clone();
    insert_message(&db, "user", &text, &cfg.provider, &[])?;
    let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, app_state.as_deref(), Turn::User(&text), &turn_id, cancel, speech.as_ref()).await;
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
        Some("resume") => "[系统事件] 用户刚打开使驾，请用人设简短地打个招呼（1~2 句），可自然地接上你记得的事情；不要自我介绍功能清单。".to_string(),
        _ => "[系统事件] 这是一个新的长期会话：用户刚启用或新建了你。请用人设向用户打招呼并简单自我介绍（1~3 句），如果长期记忆里有相关内容可以自然提起。".to_string(),
    };
    if !persona.greeting.trim().is_empty() {
        ev.push_str(&format!("\n参考开场白风格（不必照抄）：{}", persona.greeting.trim()));
    }
    let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, app_state.as_deref(), Turn::Event(ev), &turn_id, cancel, speech.as_ref()).await;
    mark_greeted(&db, &raw);
    finish_reply(&db, &cfg, raw, true)
}

/// 主动关怀的系统事件：前端给出此刻的情况（时间、连续使用时长、待办、这次的由头…）
pub fn care_event(context: &str) -> String {
    let ctx = cap_chars(context.trim(), 1200);
    format!(
        "[系统事件 · 主动关怀] 这次不是用户在找你：用户此刻没在忙，你想主动搭句话。下面是你注意到的情况（挑最值得说的一点就好，不要逐条复述）：\n{ctx}\n\n\
怎么说：\n\
- 像身边熟悉的朋友 / 搭档随口一句，保持你的人设口吻；1~2 句，总共不超过 50 字。\n\
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
    let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, app_state.as_deref(), Turn::Event(care_event(&context)), &turn_id, cancel, speech.as_ref()).await;
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
    let raw = run_turn(&app, &db, &agents, &rt, &cfg, &persona, None, Turn::Event(task), &turn_id, cancel, None).await
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
    agents.assistant_session_ensure(&cfg.agent_type, &sid, &rt.workspace(&cfg)).await
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
        assert!(speech_rule(None).is_none());
        assert!(speech_rule(Some(&SpeechHint { enabled: false, ..Default::default() })).is_none());
        let ja = speech_rule(Some(&SpeechHint { enabled: true, lang: "ja".into(), layer: true, ..Default::default() })).unwrap();
        assert!(ja.contains("[[ja: …]]") && ja.contains("日语"));
        let plain = speech_rule(Some(&SpeechHint { enabled: true, lang: "zh".into(), layer: false, ..Default::default() })).unwrap();
        assert!(plain.contains("短句") && !plain.contains("[["));
    }

    #[test]
    fn conversation_and_care_flags() {
        let conv = speech_rule(Some(&SpeechHint { conversation: true, ..Default::default() })).unwrap();
        assert!(conv.contains("[[结束对话]]") && !conv.contains("语音输出已开启"));
        let both = speech_rule(Some(&SpeechHint { enabled: true, lang: "zh".into(), conversation: true, ..Default::default() })).unwrap();
        assert!(both.contains("语音输出已开启") && both.contains("[[结束对话]]"));
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
        let s = system_prompt(&p, &list_memories(&db).unwrap(), &cfg);
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
}
