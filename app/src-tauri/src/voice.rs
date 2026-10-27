//! 全局助手语音：语音输入（本地 SenseVoice / 百度短语音识别极速版）与语音输出（本地 TTS / 角色语音包）。
//!
//! - 「语音运行环境」按需安装：Python 3.12 嵌入版 + pip 装 sherpa-onnx / onnxruntime / pyopenjtalk 等，
//!   全部落在使驾数据目录 tools/voice 下，不碰系统 Python。
//! - 常驻语音服务（voice_server.py）经 stdin/stdout JSON 行通信，模型只加载一次；父进程退出即结束。
//! - 模型：SenseVoice（识别）、Melo / Kokoro（通用中英 TTS）从 sherpa-onnx 官方发布下载；
//!   角色语音包（如 牧濑红莉栖 · Amadeus 日语）以 zip 导入（model.onnx + tokens + voice.json）。
//! - 唤醒词（如 Christina）：sherpa-onnx KeywordSpotter（GigaSpeech 英文小模型），前端持续送 16k 音频片段，
//!   由单独一个单线程语音服务实例检测，不和识别 / 合成排队。
//! - 系统语音（Web Speech）由前端直接调用，不经过这里。

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::db::Db;

const CFG_KEY: &str = "assistant.voice";
const EVT_PROGRESS: &str = "voice://progress";
const SERVER_PY: &str = include_str!("voice_server.py");

const PY_VERSION: &str = "3.12.10";
const PIP_WHEEL: &str = "packages/f3/6e/1736e5b4ae2b778ef2f81c47d797de9f891d4d8acb047a24ca37a60294dd/pip-26.2.1-py3-none-any.whl";
/// 第一步按依赖解析安装；pyopenjtalk-plus 单独 --no-deps（不需要 200MB 的 sudachidict）
const PIP_PACKAGES: &[&str] = &["numpy>=1.26,<3", "onnxruntime>=1.18,<2", "sherpa-onnx>=1.12,<2", "pydantic>=2,<3", "sudachipy>=0.6,<1", "jaconv>=0.3"];
const PIP_NODEPS: &[&str] = &["pyopenjtalk-plus>=0.4,<0.5"];
const CN_PYPI: &str = "https://pypi.tuna.tsinghua.edu.cn/simple";
const MAX_PCM_B64: usize = 3_400_000;

type DbState<'a> = State<'a, Arc<Db>>;
type VState<'a> = State<'a, Arc<VoiceRuntime>>;

// ---------------------------------------------------------------- config

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct VoiceConfig {
    pub input_enabled: bool,
    /// "local" | "baidu"
    pub input_engine: String,
    pub baidu_api_key: String,
    pub baidu_secret_key: String,
    /// 识别完直接发送（否则填进输入框）
    pub input_auto_send: bool,
    pub input_max_seconds: u32,
    pub output_enabled: bool,
    /// 通用语音："system" | "melo" | "kokoro"
    pub default_voice: String,
    pub kokoro_speaker: u32,
    /// 系统语音名（空 = 按语言自动）
    pub system_voice: String,
    /// 皮肤角色有专属语音包时用角色语音
    pub character_voice: bool,
    /// 完整朗读：字幕之外，正文里可读的句子也逐句读出（代码块、表格、链接不读）
    pub read_full: bool,
    /// 皮肤 id → 语音包 id（"none" = 该皮肤不用角色语音；缺省 = 按语音包声明自动匹配）
    pub bindings: BTreeMap<String, String>,
    pub rate: f32,
    pub volume: f32,
    /// 下载源："official" | "cn"
    pub source: String,
    /// GitHub 下载加速前缀（如 https://ghfast.top/），空 = 直连
    pub github_prefix: String,
    /// 自定义 PyPI 索引（空 = 按下载源）
    pub pypi_index: String,
    /// 仅用于下载的 HTTP 代理（如 http://127.0.0.1:7890）
    pub proxy: String,
    /// 唤醒词：常开麦克风，听到唤醒词后唤起助手
    pub wake_enabled: bool,
    /// 唤醒词（英文，逗号分隔，最多 4 个）
    pub wake_keywords: String,
    /// 触发阈值 0.05~0.6：越小越灵敏（也越容易误触发）
    pub wake_threshold: f32,
    /// 唤醒后："listen" = 直接开始语音输入；"open" = 只打开输入框
    pub wake_action: String,
    /// 窗口最小化 / 在后台时也监听（唤醒时把窗口拉到前台）
    pub wake_background: bool,
    /// 唤醒时播放提示音
    pub wake_chime: bool,
    /// 唤醒后进入连续语音对话（助手说完自动继续听，由她判断何时结束）
    pub wake_multiturn: bool,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            input_enabled: false,
            input_engine: "local".into(),
            baidu_api_key: String::new(),
            baidu_secret_key: String::new(),
            input_auto_send: true,
            input_max_seconds: 30,
            output_enabled: false,
            default_voice: "system".into(),
            kokoro_speaker: 3,
            system_voice: String::new(),
            character_voice: true,
            read_full: false,
            bindings: BTreeMap::new(),
            rate: 1.0,
            volume: 1.0,
            source: "official".into(),
            github_prefix: String::new(),
            pypi_index: String::new(),
            proxy: String::new(),
            wake_enabled: false,
            wake_keywords: DEFAULT_WAKE.into(),
            wake_threshold: 0.25,
            wake_action: "listen".into(),
            wake_background: false,
            wake_chime: true,
            wake_multiturn: true,
        }
    }
}

impl VoiceConfig {
    fn normalized(mut self) -> Self {
        if self.input_engine != "baidu" { self.input_engine = "local".into(); }
        if !matches!(self.default_voice.as_str(), "system" | "melo" | "kokoro") { self.default_voice = "system".into(); }
        if self.source != "cn" { self.source = "official".into(); }
        self.input_max_seconds = self.input_max_seconds.clamp(5, 60);
        self.kokoro_speaker = self.kokoro_speaker.min(102);
        self.rate = if self.rate.is_finite() { self.rate.clamp(0.5, 2.0) } else { 1.0 };
        self.volume = if self.volume.is_finite() { self.volume.clamp(0.0, 1.0) } else { 1.0 };
        self.baidu_api_key = self.baidu_api_key.trim().to_string();
        self.github_prefix = self.github_prefix.trim().to_string();
        if !self.github_prefix.is_empty() && !self.github_prefix.ends_with('/') { self.github_prefix.push('/'); }
        self.pypi_index = self.pypi_index.trim().trim_end_matches('/').to_string();
        self.proxy = self.proxy.trim().to_string();
        self.system_voice = self.system_voice.trim().chars().take(200).collect();
        self.bindings.retain(|k, v| !k.trim().is_empty() && k.len() <= 80 && v.len() <= 80);
        self.wake_keywords = wake_words(&self.wake_keywords).join(", ");
        if self.wake_keywords.is_empty() { self.wake_keywords = DEFAULT_WAKE.into(); }
        self.wake_threshold = if self.wake_threshold.is_finite() { self.wake_threshold.clamp(0.05, 0.6) } else { 0.25 };
        if self.wake_action != "open" { self.wake_action = "listen".into(); }
        self
    }
}

const DEFAULT_WAKE: &str = "Christina";
/// 唤醒词的加权（keywords_score）：越大越容易命中
const WAKE_BOOST: f32 = 1.5;

/// 唤醒词列表：按逗号 / 分号 / 换行切开，只保留英文字母、空格、撇号（模型是英文 BPE），去重，最多 4 个
fn wake_words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in s.split([',', '，', ';', '；', '、', '\n', '/']) {
        let w: String = part.chars().filter(|c| c.is_ascii_alphabetic() || *c == ' ' || *c == '\'').collect();
        let w = w.split_whitespace().collect::<Vec<_>>().join(" ");
        if w.is_empty() || w.len() > 40 || out.iter().any(|x| x.eq_ignore_ascii_case(&w)) { continue; }
        out.push(w);
        if out.len() >= 4 { break; }
    }
    out
}

fn load_config(db: &Db) -> VoiceConfig {
    let mut cfg = db
        .get_setting(CFG_KEY).ok().flatten()
        .and_then(|s| serde_json::from_str::<VoiceConfig>(&s).ok())
        .unwrap_or_default()
        .normalized();
    // 「完整朗读正文」从默认开改为默认关（读出来太长；字幕才是主要交互）。
    // 一次性把存量配置里的 read_full 压下去；想要的话在语音设置里重新勾选。
    if db.get_setting("voice.read_full_off_migrated").ok().flatten().as_deref() != Some("1") {
        cfg.read_full = false;
        let _ = db.set_setting("voice.read_full_off_migrated", "1");
    }
    cfg
}

// ---------------------------------------------------------------- catalog

struct ModelSpec {
    id: &'static str,
    kind: &'static str,
    name: &'static str,
    url: &'static str,
    size_mb: u32,
    check: &'static str,
}

const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "asr-sensevoice", kind: "asr", name: "SenseVoice（中/英/日/韩/粤 识别，int8）",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2",
        size_mb: 156, check: "model.int8.onnx",
    },
    ModelSpec {
        id: "tts-melo", kind: "tts", name: "MeloTTS 中英（单音色，快速）",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-melo-tts-zh_en.tar.bz2",
        size_mb: 160, check: "model.onnx",
    },
    ModelSpec {
        id: "tts-kokoro", kind: "tts", name: "Kokoro v1.1 中英（多音色，较慢）",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-int8-multi-lang-v1_1.tar.bz2",
        size_mb: 141, check: "voices.bin",
    },
    ModelSpec {
        id: "kws-gigaspeech", kind: "kws", name: "唤醒词模型（英文 GigaSpeech zipformer 3.3M）",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2",
        size_mb: 18, check: "bpe.model",
    },
];

fn model_spec(id: &str) -> Option<&'static ModelSpec> { MODELS.iter().find(|m| m.id == id) }

// ---------------------------------------------------------------- runtime state

#[derive(Serialize, Clone, Debug, Default)]
pub struct TaskInfo {
    pub id: String,
    /// download | extract | pip | verify | done | error | cancelled
    pub stage: String,
    pub received: u64,
    pub total: u64,
    pub message: String,
    pub running: bool,
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
    next_id: u64,
}

pub struct VoiceRuntime {
    root: PathBuf,
    server: Mutex<Option<Server>>,
    /// 唤醒词检测专用实例（单线程，常驻时与识别 / 合成互不阻塞）
    kws: Mutex<Option<Server>>,
    tasks: Mutex<HashMap<String, TaskInfo>>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    baidu_token: Mutex<Option<(String, String, Instant)>>,
}

impl Drop for VoiceRuntime {
    fn drop(&mut self) {
        for slot in [&self.server, &self.kws] {
            if let Ok(mut s) = slot.lock() {
                if let Some(mut srv) = s.take() { let _ = srv.child.kill(); }
            }
        }
    }
}

/// 语音服务实例：主实例（识别 / 合成）与唤醒词实例
#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot { Main, Kws }

impl Slot {
    fn log(self) -> &'static str { match self { Slot::Main => "voice_server.log", Slot::Kws => "voice_kws.log" } }
}

impl VoiceRuntime {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            root: data_dir.join("tools").join("voice"),
            server: Mutex::new(None),
            kws: Mutex::new(None),
            tasks: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            baidu_token: Mutex::new(None),
        }
    }
    fn py_dir(&self) -> PathBuf { self.root.join("runtime").join("python") }
    fn site_dir(&self) -> PathBuf { self.root.join("runtime").join("site") }
    fn python(&self) -> PathBuf { self.py_dir().join(if cfg!(windows) { "python.exe" } else { "bin/python3" }) }
    fn ready_file(&self) -> PathBuf { self.root.join("runtime").join("ready.json") }
    fn model_dir(&self, id: &str) -> PathBuf { self.root.join("models").join(id) }
    fn packs_dir(&self) -> PathBuf { self.root.join("voices") }
    fn runtime_ready(&self) -> bool { self.ready_file().is_file() && self.python().is_file() }
    fn model_installed(&self, m: &ModelSpec) -> bool { self.model_dir(m.id).join(m.check).is_file() }

    fn slot(&self, slot: Slot) -> &Mutex<Option<Server>> { match slot { Slot::Main => &self.server, Slot::Kws => &self.kws } }

    /// 停掉全部语音服务实例（安装 / 删除组件前释放文件占用）
    fn stop_server(&self) {
        self.stop_slot(Slot::Main);
        self.stop_slot(Slot::Kws);
    }

    fn stop_slot(&self, slot: Slot) {
        if let Ok(mut s) = self.slot(slot).lock() {
            if let Some(mut srv) = s.take() {
                let _ = srv.child.kill();
                let _ = srv.child.wait();
            }
        }
    }

    fn spawn_server(&self, slot: Slot) -> Result<Server, String> {
        if !self.runtime_ready() { return Err("语音运行环境未安装：请在 设置 → 全局助手 → 语音 中安装".into()); }
        let script = self.root.join("runtime").join("voice_server.py");
        if std::fs::read_to_string(&script).ok().as_deref() != Some(SERVER_PY) {
            std::fs::write(&script, SERVER_PY).map_err(|e| format!("写入语音服务脚本失败：{e}"))?;
        }
        let log = std::fs::File::create(self.root.join(slot.log())).map_err(|e| format!("创建日志失败：{e}"))?;
        let mut cmd = Command::new(self.python());
        if slot == Slot::Kws { cmd.env("SHIDRIVE_VOICE_THREADS", "1"); }
        cmd.arg("-u").arg("-X").arg("utf8").arg(&script)
            .current_dir(self.root.join("runtime"))
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONNOUSERSITE", "1")
            .env_remove("PYTHONPATH")
            .env_remove("PYTHONHOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        crate::setup::hide_console(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("启动语音服务失败：{e}"))?;
        let stdin = child.stdin.take().ok_or("语音服务没有 stdin")?;
        let stdout = child.stdout.take().ok_or("语音服务没有 stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() { break; }
            }
        });
        Ok(Server { child, stdin, rx, next_id: 1 })
    }

    /// 向常驻语音服务发一个请求（串行）；超时或进程退出时重置，下次自动重启。
    fn request(&self, req: Value, timeout: Duration) -> Result<Value, String> {
        self.request_on(Slot::Main, req, timeout)
    }

    fn request_on(&self, slot: Slot, mut req: Value, timeout: Duration) -> Result<Value, String> {
        let mut guard = self.slot(slot).lock().map_err(|_| "语音服务状态异常")?;
        if let Some(srv) = guard.as_mut() {
            if !matches!(srv.child.try_wait(), Ok(None)) { *guard = None; }
        }
        if guard.is_none() { *guard = Some(self.spawn_server(slot)?); }
        let srv = guard.as_mut().unwrap();
        let id = srv.next_id;
        srv.next_id += 1;
        req["id"] = json!(id);
        let line = serde_json::to_string(&req).map_err(|e| e.to_string())? + "\n";
        if let Err(e) = srv.stdin.write_all(line.as_bytes()).and_then(|_| srv.stdin.flush()) {
            let tail = self.log_tail(slot);
            *guard = None;
            return Err(format!("语音服务已退出：{e}{tail}"));
        }
        let deadline = Instant::now() + timeout;
        let failure = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match srv.rx.recv_timeout(left) {
                Ok(l) => {
                    let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                    if v.get("id").and_then(Value::as_u64) != Some(id) { continue; }
                    if v.get("ok").and_then(Value::as_bool) == Some(true) { return Ok(v); }
                    return Err(format!("语音服务出错：{}", v.get("error").and_then(Value::as_str).unwrap_or("未知错误")));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break "语音服务响应超时（已重启）".to_string(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break format!("语音服务意外退出{}", self.log_tail(slot)),
            }
        };
        if let Some(mut s) = guard.take() { let _ = s.child.kill(); let _ = s.child.wait(); }
        Err(failure)
    }

    fn log_tail(&self, slot: Slot) -> String {
        let s = std::fs::read_to_string(self.root.join(slot.log())).unwrap_or_default();
        let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = lines[lines.len().saturating_sub(3)..].join(" | ");
        if tail.is_empty() { String::new() } else { format!("：{}", tail.chars().take(400).collect::<String>()) }
    }

    fn set_task(&self, app: Option<&AppHandle>, t: TaskInfo) {
        if let Some(app) = app { let _ = app.emit(EVT_PROGRESS, &t); }
        if let Ok(mut m) = self.tasks.lock() { m.insert(t.id.clone(), t); }
    }
}

// ---------------------------------------------------------------- status

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct VoicePack {
    pub id: String,
    pub name: String,
    pub lang: String,
    pub engine: String,
    /// engine = "preset" 时的底模："kokoro" | "melo" | "pack:<sherpa-vits 语音包 id>"（音色 = 底模说话人 + 变调/语速）
    pub base: String,
    pub skins: Vec<String>,
    pub characters: Vec<String>,
    pub license: String,
    pub source: String,
    pub credits: String,
}

#[derive(Serialize)]
pub struct ModelStatus {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub size_mb: u32,
    pub installed: bool,
}

#[derive(Serialize)]
pub struct VoiceStatus {
    pub root: String,
    pub runtime_installed: bool,
    pub runtime_info: Value,
    pub models: Vec<ModelStatus>,
    pub packs: Vec<VoicePack>,
    pub tasks: Vec<TaskInfo>,
    pub server_running: bool,
    pub kws_running: bool,
}

#[derive(Serialize)]
pub struct VoiceView {
    pub config: VoiceConfig,
    pub baidu_secret_set: bool,
    pub status: VoiceStatus,
}

fn list_packs(rt: &VoiceRuntime) -> Vec<VoicePack> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(rt.packs_dir()) else { return out };
    for e in rd.flatten() {
        let f = e.path().join("voice.json");
        let Some(mut p) = std::fs::read_to_string(&f).ok().and_then(|s| serde_json::from_str::<VoicePack>(&s).ok()) else { continue };
        p.id = e.file_name().to_string_lossy().to_string();
        if p.name.trim().is_empty() { p.name = p.id.clone(); }
        out.push(p);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn status(rt: &VoiceRuntime) -> VoiceStatus {
    let runtime_installed = rt.runtime_ready();
    let runtime_info = std::fs::read_to_string(rt.ready_file()).ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
    let models = MODELS.iter().map(|m| ModelStatus {
        id: m.id.into(), kind: m.kind.into(), name: m.name.into(), size_mb: m.size_mb, installed: rt.model_installed(m),
    }).collect();
    let tasks = rt.tasks.lock().map(|m| m.values().cloned().collect()).unwrap_or_default();
    let server_running = rt.server.lock().map(|s| s.is_some()).unwrap_or(false);
    let kws_running = rt.kws.lock().map(|s| s.is_some()).unwrap_or(false);
    VoiceStatus {
        root: rt.root.to_string_lossy().to_string(),
        runtime_installed, runtime_info, models, packs: list_packs(rt), tasks, server_running, kws_running,
    }
}

fn view(db: &Db, rt: &VoiceRuntime) -> VoiceView {
    let mut config = load_config(db);
    let baidu_secret_set = !config.baidu_secret_key.is_empty();
    config.baidu_secret_key.clear();
    VoiceView { config, baidu_secret_set, status: status(rt) }
}

// ---------------------------------------------------------------- download / install

fn agent(cfg: &VoiceConfig) -> Result<ureq::Agent, String> {
    let b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(90));
    Ok(match Some(cfg.proxy.as_str()).filter(|p| !p.is_empty()).map(ureq::Proxy::new) {
        Some(Ok(p)) => b.proxy(p).build(),
        Some(Err(e)) => return Err(format!("代理配置无效：{e}")),
        None => b.build(),
    })
}

fn github_urls(cfg: &VoiceConfig, url: &str) -> Vec<String> {
    let mut v = Vec::new();
    if !cfg.github_prefix.is_empty() { v.push(format!("{}{url}", cfg.github_prefix)); }
    v.push(url.to_string());
    v
}

fn pypi_index(cfg: &VoiceConfig) -> Option<String> {
    if !cfg.pypi_index.is_empty() { return Some(cfg.pypi_index.clone()); }
    (cfg.source == "cn").then(|| CN_PYPI.to_string())
}

struct Job<'a> {
    app: Option<&'a AppHandle>,
    rt: &'a VoiceRuntime,
    id: String,
    cancel: Arc<AtomicBool>,
    cfg_proxy: String,
}

impl Job<'_> {
    fn report(&self, stage: &str, received: u64, total: u64, message: impl Into<String>) {
        self.rt.set_task(self.app, TaskInfo {
            id: self.id.clone(), stage: stage.into(), received, total, message: message.into(), running: true,
        });
    }
    fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) { Err("已取消".into()) } else { Ok(()) }
    }

    /// 依次尝试候选地址下载到 dest（先写 .part，完成后改名）
    fn download(&self, agent: &ureq::Agent, urls: &[String], dest: &Path, label: &str) -> Result<(), String> {
        let mut errs = Vec::new();
        for url in urls {
            self.check()?;
            // 网络错误（超时 / 断流）同一地址再试一次；HTTP 状态错误直接换下一个
            for attempt in 0..2 {
                self.check()?;
                self.report("download", 0, 0, format!("正在下载 {label}：{url}"));
                let r = match self.download_one(agent, url, dest, label) {
                    // 个别网络下内置 TLS 客户端连 GitHub 会超时，改用系统自带的 curl.exe 再试
                    Err(e) if e != "已取消" && !e.starts_with("HTTP ") && !e.starts_with("写入失败") && !e.starts_with("保存失败") => {
                        self.download_curl(url, dest, label).map_err(|c| format!("{e}；curl：{c}"))
                    }
                    r => r,
                };
                match r {
                    Ok(()) => return Ok(()),
                    Err(e) if e == "已取消" => return Err(e),
                    Err(e) if attempt == 0 && !e.starts_with("HTTP ") => {
                        self.report("download", 0, 0, format!("下载 {label} 出错，重试中：{e}"));
                        std::thread::sleep(Duration::from_secs(2));
                    }
                    Err(e) => { errs.push(format!("{url} → {e}")); break; }
                }
            }
        }
        Err(format!("下载 {label} 失败（可在下载设置里换镜像 / 加速前缀 / 代理）：{}", errs.join("；")))
    }

    /// 兜底下载：Windows 10 1803+ 自带 System32\curl.exe（Schannel）
    fn download_curl(&self, url: &str, dest: &Path, label: &str) -> Result<(), String> {
        let curl = std::env::var_os("SystemRoot")
            .map(|r| PathBuf::from(r).join("System32").join("curl.exe"))
            .filter(|p| p.is_file())
            .ok_or("系统没有 curl.exe")?;
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&part);
        if let Some(d) = part.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
        let mut cmd = Command::new(curl);
        cmd.args(["-fsSL", "--retry", "2", "--connect-timeout", "20", "--speed-time", "90", "--speed-limit", "1", "-o"])
            .arg(&part).arg(url)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        if !self.cfg_proxy.is_empty() { cmd.arg("-x").arg(&self.cfg_proxy); }
        crate::setup::hide_console(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let status = loop {
            if self.cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&part);
                return Err("已取消".into());
            }
            if let Some(st) = child.try_wait().map_err(|e| e.to_string())? { break st; }
            let got = part.metadata().map(|m| m.len()).unwrap_or(0);
            self.report("download", got, 0, format!("正在下载 {label}（curl）"));
            std::thread::sleep(Duration::from_millis(300));
        };
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() { let _ = e.read_to_string(&mut err); }
        let got = part.metadata().map(|m| m.len()).unwrap_or(0);
        if !status.success() || got < 1024 {
            let _ = std::fs::remove_file(&part);
            return Err(if err.trim().is_empty() { format!("退出码 {:?}", status.code()) } else { err.trim().chars().take(300).collect() });
        }
        let _ = std::fs::remove_file(dest);
        std::fs::rename(&part, dest).map_err(|e| format!("保存失败：{e}"))?;
        self.report("download", got, got, format!("{label} 下载完成"));
        Ok(())
    }

    fn download_one(&self, agent: &ureq::Agent, url: &str, dest: &Path, label: &str) -> Result<(), String> {
        let resp = agent.get(url).call().map_err(|e| match e {
            ureq::Error::Status(c, _) => format!("HTTP {c}"),
            other => other.to_string(),
        })?;
        let total: u64 = resp.header("Content-Length").and_then(|s| s.parse().ok()).unwrap_or(0);
        let part = dest.with_extension("part");
        if let Some(d) = part.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
        let mut f = std::fs::File::create(&part).map_err(|e| format!("写入失败：{e}"))?;
        let mut r = resp.into_reader().take(1024 * 1024 * 1024);
        let mut buf = vec![0u8; 256 * 1024];
        let mut got = 0u64;
        let mut last = Instant::now();
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                drop(f);
                let _ = std::fs::remove_file(&part);
                return Err("已取消".into());
            }
            let n = r.read(&mut buf).map_err(|e| format!("下载中断：{e}"))?;
            if n == 0 { break; }
            f.write_all(&buf[..n]).map_err(|e| format!("写入失败：{e}"))?;
            got += n as u64;
            if last.elapsed() > Duration::from_millis(250) {
                last = Instant::now();
                self.report("download", got, total, format!("正在下载 {label}"));
            }
        }
        drop(f);
        if total > 0 && got < total {
            let _ = std::fs::remove_file(&part);
            return Err(format!("下载不完整（{got}/{total}）"));
        }
        if got < 1024 {
            let _ = std::fs::remove_file(&part);
            return Err("下载内容异常（过小）".into());
        }
        let _ = std::fs::remove_file(dest);
        std::fs::rename(&part, dest).map_err(|e| format!("保存失败：{e}"))?;
        self.report("download", got, total.max(got), format!("{label} 下载完成"));
        Ok(())
    }
}

/// 解压 zip（防 zip-slip）
fn unzip(file: &Path, dest: &Path) -> Result<(), String> {
    let f = std::fs::File::open(file).map_err(|e| format!("打开压缩包失败：{e}"))?;
    let mut a = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("读取压缩包失败：{e}"))?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for i in 0..a.len() {
        let mut e = a.by_index(i).map_err(|e| format!("压缩包损坏：{e}"))?;
        let Some(name) = e.enclosed_name().map(|p| p.to_path_buf()) else { continue };
        let out = dest.join(&name);
        if e.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(d) = out.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
            let mut w = std::fs::File::create(&out).map_err(|e| format!("解压失败：{e}"))?;
            std::io::copy(&mut e, &mut w).map_err(|e| format!("解压失败：{e}"))?;
        }
    }
    Ok(())
}

/// 解压 .tar.bz2：优先用语音运行环境里的 Python（tarfile，防路径穿越）；
/// Windows 自带的 bsdtar（System32\tar.exe）不支持 bzip2，仅作兜底
fn untar(rt: &VoiceRuntime, file: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut cmd = if rt.python().is_file() {
        let mut c = Command::new(rt.python());
        c.args(["-I", "-c", "import sys,tarfile\nwith tarfile.open(sys.argv[1]) as t: t.extractall(sys.argv[2], filter='data')"])
            .arg(file).arg(dest).env("PYTHONIOENCODING", "utf-8");
        c
    } else {
        let tar = std::env::var_os("SystemRoot")
            .map(|r| PathBuf::from(r).join("System32").join("tar.exe"))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("tar"));
        let mut c = Command::new(tar);
        c.arg("-xf").arg(file).arg("-C").arg(dest);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    crate::setup::hide_console(&mut cmd);
    let out = cmd.output().map_err(|e| format!("调用解压程序失败：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
        return Err(format!("解压失败：{}", tail[tail.len().saturating_sub(2)..].join(" | ")));
    }
    Ok(())
}

/// 解压后定位真正的模型目录（含 check 文件的那一层）
fn find_dir_with(dir: &Path, check: &str, depth: u32) -> Option<PathBuf> {
    if dir.join(check).is_file() { return Some(dir.to_path_buf()); }
    if depth == 0 { return None; }
    std::fs::read_dir(dir).ok()?.flatten()
        .filter(|e| e.path().is_dir())
        .find_map(|e| find_dir_with(&e.path(), check, depth - 1))
}

fn replace_dir(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() { std::fs::remove_dir_all(to).map_err(|e| format!("移除旧版本失败（文件被占用？）：{e}"))?; }
    if let Some(p) = to.parent() { std::fs::create_dir_all(p).map_err(|e| e.to_string())?; }
    std::fs::rename(from, to).map_err(|e| format!("安装失败：{e}"))
}

fn install_model(job: &Job, cfg: &VoiceConfig, m: &ModelSpec) -> Result<String, String> {
    let agent = agent(cfg)?;
    let dl = job.rt.root.join("downloads");
    let file = dl.join(format!("{}.tar.bz2", m.id));
    if !job.rt.runtime_ready() { return Err("请先安装语音运行环境".into()); }
    // 已完整下载（.part 已改名）但上次在解压前中断时直接复用
    if !file.metadata().is_ok_and(|md| md.len() > 1024 * 1024) {
        job.download(&agent, &github_urls(cfg, m.url), &file, m.name)?;
    }
    job.check()?;
    job.report("extract", 0, 0, format!("正在解压 {}…", m.name));
    let tmp = job.rt.root.join("models").join(format!(".tmp-{}", m.id));
    let _ = std::fs::remove_dir_all(&tmp);
    if let Err(e) = untar(job.rt, &file, &tmp) {
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_file(&file);
        return Err(e);
    }
    let inner = find_dir_with(&tmp, m.check, 2).ok_or_else(|| format!("压缩包里没有找到 {}", m.check))?;
    job.rt.stop_server();
    replace_dir(&inner, &job.rt.model_dir(m.id))?;
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&file);
    Ok(format!("{} 已安装", m.name))
}

fn run_logged(job: &Job, mut cmd: Command, stage: &str) -> Result<(), String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::setup::hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("启动失败：{e}"))?;
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut hs = Vec::new();
    for r in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten() {
        let lines = lines.clone();
        hs.push(std::thread::spawn(move || {
            for l in BufReader::new(r).lines().map_while(Result::ok) {
                if let Ok(mut v) = lines.lock() { v.push(l); }
            }
        }));
    }
    let mut shown = 0usize;
    let code = loop {
        if job.cancel.load(Ordering::SeqCst) { let _ = child.kill(); let _ = child.wait(); return Err("已取消".into()); }
        if let Ok(Some(st)) = child.try_wait() { break st; }
        let last = lines.lock().ok().and_then(|v| { let n = v.len(); (n > shown).then(|| { shown = n; v[n - 1].clone() }) });
        if let Some(l) = last { job.report(stage, 0, 0, l.chars().take(200).collect::<String>()); }
        std::thread::sleep(Duration::from_millis(300));
    };
    for h in hs { let _ = h.join(); }
    let all = lines.lock().map(|v| v.clone()).unwrap_or_default();
    let log = job.rt.root.join("install.log");
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&log)
        .and_then(|mut f| f.write_all((all.join("\n") + "\n").as_bytes()));
    if !code.success() {
        let tail: Vec<&String> = all.iter().filter(|l| !l.trim().is_empty()).rev().take(4).collect();
        let tail: Vec<&str> = tail.into_iter().rev().map(|s| s.as_str()).collect();
        return Err(format!("执行失败（{}）：{}", code, tail.join(" | ")));
    }
    Ok(())
}

fn install_runtime(job: &Job, cfg: &VoiceConfig) -> Result<String, String> {
    if !cfg!(windows) { return Err("语音运行环境目前只支持 Windows".into()); }
    let agent = agent(cfg)?;
    let rt = job.rt;
    let dl = rt.root.join("downloads");
    rt.stop_server();
    let _ = std::fs::remove_file(rt.ready_file());
    let _ = std::fs::remove_file(rt.root.join("install.log"));

    // 1) Python 嵌入版
    let name = format!("python-{PY_VERSION}-embed-amd64.zip");
    let official = format!("https://www.python.org/ftp/python/{PY_VERSION}/{name}");
    let mirror = format!("https://registry.npmmirror.com/-/binary/python/{PY_VERSION}/{name}");
    let urls = if cfg.source == "cn" { vec![mirror, official] } else { vec![official, mirror] };
    let zip_file = dl.join(&name);
    job.download(&agent, &urls, &zip_file, "Python 嵌入版")?;
    job.check()?;
    job.report("extract", 0, 0, "正在解压 Python…");
    let py = rt.py_dir();
    if py.exists() { std::fs::remove_dir_all(&py).map_err(|e| format!("清理旧环境失败（语音服务仍在运行？）：{e}"))?; }
    unzip(&zip_file, &py)?;
    let _ = std::fs::remove_file(&zip_file);
    // ._pth：加入 ..\site 并启用 site
    let pth = std::fs::read_dir(&py).map_err(|e| e.to_string())?.flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "_pth"))
        .ok_or("Python 嵌入版缺少 ._pth 文件")?;
    let zipname = pth.file_stem().map(|s| format!("{}.zip", s.to_string_lossy())).unwrap_or_default();
    std::fs::write(&pth, format!("{zipname}\r\n.\r\n..\\site\r\nimport site\r\n")).map_err(|e| e.to_string())?;

    // 2) pip（直接解压 wheel 到 site）
    let site = rt.site_dir();
    std::fs::create_dir_all(&site).map_err(|e| e.to_string())?;
    let mut pip_urls = Vec::new();
    if let Some(ix) = pypi_index(cfg) {
        if let Some(base) = ix.strip_suffix("/simple") { pip_urls.push(format!("{base}/{PIP_WHEEL}")); }
    }
    pip_urls.push(format!("https://files.pythonhosted.org/{PIP_WHEEL}"));
    let pip_file = dl.join("pip.whl");
    job.download(&agent, &pip_urls, &pip_file, "pip")?;
    unzip(&pip_file, &site)?;
    let _ = std::fs::remove_file(&pip_file);

    // 3) 依赖包
    let pip = |pkgs: &[&str], nodeps: bool| {
        let mut c = Command::new(rt.python());
        c.args(["-m", "pip", "install", "--disable-pip-version-check", "--no-warn-script-location", "--no-input", "--upgrade", "--prefer-binary", "--only-binary=:all:", "--target"])
            .arg(&site)
            .env("PYTHONNOUSERSITE", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env_remove("PYTHONPATH")
            .env_remove("PIP_CONFIG_FILE")
            .current_dir(&rt.root);
        if nodeps { c.arg("--no-deps"); }
        if let Some(ix) = pypi_index(cfg) { c.arg("-i").arg(ix); }
        if !cfg.proxy.is_empty() { c.arg("--proxy").arg(&cfg.proxy); }
        c.args(pkgs);
        c
    };
    job.check()?;
    job.report("pip", 0, 0, "正在安装 sherpa-onnx / onnxruntime / numpy 等（约 60MB）…");
    run_logged(job, pip(PIP_PACKAGES, false), "pip")?;
    job.check()?;
    job.report("pip", 0, 0, "正在安装 pyopenjtalk（日语文本前端，约 25MB）…");
    run_logged(job, pip(PIP_NODEPS, true), "pip")?;

    // 4) 校验：写一个临时 ready 标记，起服务 ping
    job.report("verify", 0, 0, "正在校验语音运行环境…");
    std::fs::write(rt.ready_file(), "{}").map_err(|e| e.to_string())?;
    let info = match rt.request(json!({ "op": "ping" }), Duration::from_secs(90)) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(rt.ready_file());
            return Err(format!("{e}（如提示缺少 DLL，请安装 Microsoft Visual C++ 2015-2022 运行库）"));
        }
    };
    let bad: Vec<String> = ["numpy", "onnxruntime", "sherpa_onnx", "pyopenjtalk"].iter()
        .filter_map(|k| info.get(*k).and_then(Value::as_str).filter(|s| s.starts_with("ERROR")).map(|s| format!("{k}: {s}")))
        .collect();
    if !bad.is_empty() {
        let _ = std::fs::remove_file(rt.ready_file());
        rt.stop_server();
        return Err(format!("组件加载失败：{}（如提示缺少 DLL，请安装 Microsoft Visual C++ 2015-2022 运行库）", bad.join("；")));
    }
    let mut info = info;
    if let Some(o) = info.as_object_mut() { o.remove("id"); o.remove("ok"); o.insert("installed_at".into(), json!(chrono::Local::now().format("%Y-%m-%d %H:%M").to_string())); }
    std::fs::write(rt.ready_file(), serde_json::to_string_pretty(&info).unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(format!("语音运行环境已就绪（Python {}）", info.get("python").and_then(Value::as_str).unwrap_or(PY_VERSION)))
}

// ---------------------------------------------------------------- packs

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 48 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.')
        && !s.starts_with('.')
}

/// preset 语音包的底模 → 模型 id
fn preset_base(base: &str) -> Option<&'static str> {
    match base { "kokoro" => Some("tts-kokoro"), "melo" => Some("tts-melo"), _ => None }
}

/// preset 的底模也可以是另一个 sherpa-vits 语音包（多说话人模型只装一份，各角色音色做成只有 voice.json 的小包）
fn preset_pack_ref(base: &str) -> Option<&str> {
    base.strip_prefix("pack:").filter(|id| valid_id(id))
}

fn read_pack_meta(dir: &Path) -> Option<Value> {
    std::fs::read_to_string(dir.join("voice.json")).ok().and_then(|s| serde_json::from_str(&s).ok())
}

/// sherpa-vits 语音包里引用的相对路径（不得越出包目录）
fn pack_rel_ok(f: &str) -> bool {
    !f.is_empty() && !f.contains("..") && !f.starts_with('/') && !f.starts_with('\\') && !f.contains(':')
}

/// preset / sherpa-vits 语音包的合成请求：模型目录 + 说话人 + 变调（半音，±8）+ 语速倍率（0.5–2）
/// meta = 被选中语音包的 voice.json；dir = 该语音包目录
fn preset_request(rt: &VoiceRuntime, meta: &Value, dir: &Path, text: &str, rate: f32) -> Result<Value, String> {
    let name = meta.get("name").and_then(Value::as_str).unwrap_or("语音包");
    let num = |k: &str, lo: f64, hi: f64, d: f64| meta.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(d).clamp(lo, hi);
    let speed = (rate as f64 * num("speed", 0.5, 2.0, 1.0)).clamp(0.5, 2.0);
    let pitch = num("pitch", -8.0, 8.0, 0.0);
    let sid = meta.get("sid").and_then(Value::as_u64).unwrap_or(0);
    let vits = |d: PathBuf, sid: u64| json!({ "op": "tts", "engine": "preset", "base": "vits", "dir": d, "text": text, "speed": speed, "sid": sid.min(9999), "pitch": pitch });
    if meta.get("engine").and_then(Value::as_str) == Some("sherpa-vits") {
        return Ok(vits(dir.to_path_buf(), sid));
    }
    let base = meta.get("base").and_then(Value::as_str).unwrap_or("");
    if let Some(id) = preset_pack_ref(base) {
        let d = rt.packs_dir().join(id);
        let engine = read_pack_meta(&d).and_then(|m| m.get("engine").and_then(Value::as_str).map(str::to_string));
        if engine.as_deref() != Some("sherpa-vits") {
            return Err(format!("「{name}」基于语音包 {id}，请先导入该语音包"));
        }
        return Ok(vits(d, sid));
    }
    let id = preset_base(base).ok_or_else(|| format!("语音包底模无效：{base}"))?;
    let m = model_spec(id).unwrap();
    if !rt.model_installed(m) {
        return Err(format!("「{name}」基于 {}，请先在 设置 → 全局助手 → 语音 里安装该模型", m.name));
    }
    let sid = sid.min(if base == "kokoro" { 102 } else { 0 });
    Ok(json!({ "op": "tts", "engine": "preset", "base": base, "dir": rt.model_dir(id), "text": text, "speed": speed, "sid": sid, "pitch": pitch }))
}

fn import_pack(rt: &VoiceRuntime, path: &Path, overwrite: bool) -> Result<VoicePack, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("无法读取文件：{e}"))?;
    if meta.len() > 1024 * 1024 * 1024 { return Err("语音包超过 1GB".into()); }
    let packs = rt.packs_dir();
    let tmp = packs.join(format!(".import-{}", uuid::Uuid::new_v4()));
    let r = (|| {
        unzip(path, &tmp)?;
        let dir = find_dir_with(&tmp, "voice.json", 1).ok_or("语音包里没有 voice.json")?;
        let raw = std::fs::read_to_string(dir.join("voice.json")).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&raw).map_err(|e| format!("voice.json 格式错误：{e}"))?;
        let mut pack: VoicePack = serde_json::from_value(v.clone()).map_err(|e| format!("voice.json 格式错误：{e}"))?;
        if !valid_id(&pack.id) { return Err("voice.json 的 id 只能包含小写字母、数字、-_.".to_string()); }
        match pack.engine.as_str() {
            "espnet-vits-onnx" => {
                let model = v.get("model").and_then(Value::as_str).unwrap_or("model.onnx");
                let tokens = v.get("tokens").and_then(Value::as_str).unwrap_or("tokens.json");
                for f in [model, tokens] {
                    if f.contains("..") || !dir.join(f).is_file() { return Err(format!("语音包缺少文件：{f}")); }
                }
            }
            // 预设音色：不带模型，复用已安装的通用 TTS（Kokoro / Melo）或 sherpa-vits 语音包的某个说话人 + 变调
            "preset" => {
                if preset_base(&pack.base).is_none() && preset_pack_ref(&pack.base).is_none() {
                    return Err(format!("preset 语音包的 base 只能是 kokoro、melo 或 pack:<语音包 id>：{}", pack.base));
                }
                if preset_pack_ref(&pack.base) == Some(pack.id.as_str()) { return Err("preset 语音包的 base 不能指向自己".into()); }
            }
            // sherpa-onnx VITS（中文 lexicon + jieba 词典，如原神/崩坏 804 角色模型）：复用运行环境里的 sherpa_onnx，无新依赖
            "sherpa-vits" => {
                let s = |k: &str, d: &'static str| v.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
                for f in [s("model", "model.onnx"), s("lexicon", "lexicon.txt"), s("tokens", "tokens.txt")] {
                    if !pack_rel_ok(&f) || !dir.join(&f).is_file() { return Err(format!("语音包缺少文件：{f}")); }
                }
                let dict = s("dict", "dict");
                if !pack_rel_ok(&dict) { return Err(format!("语音包 dict 路径无效：{dict}")); }
                if let Some(a) = v.get("rule_fsts").and_then(Value::as_array) {
                    for f in a.iter().filter_map(Value::as_str) {
                        if !pack_rel_ok(f) || !dir.join(f).is_file() { return Err(format!("语音包缺少文件：{f}")); }
                    }
                }
            }
            other => return Err(format!("暂不支持的语音包引擎：{other}")),
        }
        let dest = packs.join(&pack.id);
        if dest.exists() && !overwrite { return Err(format!("EXISTS:{}", pack.id)); }
        rt.stop_server();
        replace_dir(&dir, &dest)?;
        if pack.name.trim().is_empty() { pack.name = pack.id.clone(); }
        Ok(pack)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

// ---------------------------------------------------------------- Baidu

fn url_enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn baidu_token(rt: &VoiceRuntime, ak: &str, sk: &str) -> Result<String, String> {
    if let Ok(g) = rt.baidu_token.lock() {
        if let Some((k, tok, exp)) = g.as_ref() {
            if k == ak && Instant::now() < *exp { return Ok(tok.clone()); }
        }
    }
    let url = format!(
        "https://aip.baidubce.com/oauth/2.0/token?grant_type=client_credentials&client_id={}&client_secret={}",
        url_enc(ak), url_enc(sk)
    );
    let a = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(20)).build();
    let resp = match a.post(&url).set("Content-Type", "application/json").send_string("") {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("获取百度 access_token 失败：{e}")),
    };
    let v: Value = serde_json::from_reader(resp.into_reader()).map_err(|e| format!("百度鉴权返回异常：{e}"))?;
    let Some(tok) = v.get("access_token").and_then(Value::as_str) else {
        let msg = v.get("error_description").or_else(|| v.get("error")).and_then(Value::as_str).unwrap_or("未知错误");
        return Err(format!("百度鉴权失败：{msg}（请检查 API Key / Secret Key）"));
    };
    let secs = v.get("expires_in").and_then(Value::as_u64).unwrap_or(86400 * 30).saturating_sub(86400).max(600);
    if let Ok(mut g) = rt.baidu_token.lock() {
        *g = Some((ak.to_string(), tok.to_string(), Instant::now() + Duration::from_secs(secs)));
    }
    Ok(tok.to_string())
}

fn b64_len(b64: &str) -> usize {
    let pad = b64.bytes().rev().take_while(|b| *b == b'=').count();
    (b64.len() / 4) * 3 - pad.min(2)
}

fn baidu_stt(rt: &VoiceRuntime, cfg: &VoiceConfig, pcm_b64: &str, rate: u32) -> Result<String, String> {
    if cfg.baidu_api_key.is_empty() || cfg.baidu_secret_key.is_empty() {
        return Err("请先在 设置 → 全局助手 → 语音 中填写百度 API Key 与 Secret Key".into());
    }
    if rate != 16000 { return Err("百度极速版只支持 16k 采样率".into()); }
    let token = baidu_token(rt, &cfg.baidu_api_key, &cfg.baidu_secret_key)?;
    let body = json!({
        "format": "pcm", "rate": 16000, "channel": 1, "cuid": "shidrive-desktop", "token": token,
        "dev_pid": 80001, "speech": pcm_b64, "len": b64_len(pcm_b64),
    });
    let a = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(30)).build();
    let resp = match a.post("https://vop.baidu.com/pro_api").set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("百度语音识别请求失败：{e}")),
    };
    let v: Value = serde_json::from_reader(resp.into_reader()).map_err(|e| format!("百度语音识别返回异常：{e}"))?;
    let code = v.get("err_no").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        if matches!(code, 3302 | 110 | 111) { if let Ok(mut g) = rt.baidu_token.lock() { *g = None; } }
        let msg = v.get("err_msg").and_then(Value::as_str).unwrap_or("未知错误");
        return Err(match code {
            3301 => "没有听清（音频质量差或没有说话）".to_string(),
            3302 => format!("百度鉴权失败（{msg}）：请确认应用已开通「短语音识别极速版」"),
            3305 => "百度接口当日调用量已用完".to_string(),
            _ => format!("百度语音识别失败（{code}）：{msg}"),
        });
    }
    Ok(v.get("result").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("")).unwrap_or_default())
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub async fn voice_config_get(db: DbState<'_>, rt: VState<'_>) -> Result<VoiceView, String> {
    Ok(view(&db, &rt))
}

/// `baidu_secret`: None = 保留；Some("") = 清除
#[tauri::command]
pub async fn voice_config_set(db: DbState<'_>, rt: VState<'_>, config: VoiceConfig, baidu_secret: Option<String>) -> Result<VoiceView, String> {
    let old = load_config(&db);
    let mut cfg = config.normalized();
    cfg.baidu_secret_key = match baidu_secret {
        Some(s) => s.trim().to_string(),
        None => old.baidu_secret_key,
    };
    if cfg.baidu_api_key != old.baidu_api_key || cfg.baidu_secret_key.is_empty() {
        if let Ok(mut g) = rt.baidu_token.lock() { *g = None; }
    }
    db.set_setting(CFG_KEY, &serde_json::to_string(&cfg).map_err(|e| e.to_string())?)?;
    Ok(view(&db, &rt))
}

#[tauri::command]
pub async fn voice_status(rt: VState<'_>) -> Result<VoiceStatus, String> {
    Ok(status(&rt))
}

/// 安装组件："runtime" 或 模型 id；后台进行，进度走 voice://progress
#[tauri::command]
pub async fn voice_install(app: AppHandle, db: DbState<'_>, rt: VState<'_>, id: String) -> Result<(), String> {
    if id != "runtime" && model_spec(&id).is_none() { return Err(format!("未知组件：{id}")); }
    {
        let tasks = rt.tasks.lock().map_err(|_| "状态异常")?;
        if tasks.values().any(|t| t.running) { return Err("已有下载任务在进行，请稍候".into()); }
    }
    let cfg = load_config(&db);
    let rt = rt.inner().clone();
    let cancel = Arc::new(AtomicBool::new(false));
    rt.cancels.lock().map_err(|_| "状态异常")?.insert(id.clone(), cancel.clone());
    rt.set_task(Some(&app), TaskInfo { id: id.clone(), stage: "download".into(), message: "准备中…".into(), running: true, ..Default::default() });
    std::thread::spawn(move || {
        let job = Job { app: Some(&app), rt: &rt, id: id.clone(), cancel: cancel.clone(), cfg_proxy: cfg.proxy.clone() };
        let r = if id == "runtime" { install_runtime(&job, &cfg) } else { install_model(&job, &cfg, model_spec(&id).unwrap()) };
        let (stage, message) = match r {
            Ok(m) => ("done", m),
            Err(e) if cancel.load(Ordering::SeqCst) => ("cancelled", if e == "已取消" { "已取消".to_string() } else { e }),
            Err(e) => ("error", e),
        };
        crate::glog::info("voice", format!("安装 {id}：{stage} {message}"));
        rt.set_task(Some(&app), TaskInfo { id: id.clone(), stage: stage.into(), message, running: false, ..Default::default() });
        if let Ok(mut c) = rt.cancels.lock() { c.remove(&id); }
    });
    Ok(())
}

#[tauri::command]
pub async fn voice_cancel_install(rt: VState<'_>, id: String) -> Result<(), String> {
    if let Some(c) = rt.cancels.lock().map_err(|_| "状态异常")?.get(&id) { c.store(true, Ordering::SeqCst); }
    Ok(())
}

/// 删除组件："runtime" / 模型 id / "pack:<id>"
#[tauri::command]
pub async fn voice_remove(rt: VState<'_>, id: String) -> Result<VoiceStatus, String> {
    if rt.tasks.lock().map(|t| t.values().any(|t| t.running)).unwrap_or(false) { return Err("有下载任务在进行，请先取消".into()); }
    rt.stop_server();
    let dir = if id == "runtime" {
        rt.root.join("runtime")
    } else if let Some(p) = id.strip_prefix("pack:") {
        if !valid_id(p) { return Err("语音包 id 无效".into()); }
        rt.packs_dir().join(p)
    } else if let Some(m) = model_spec(&id) {
        rt.model_dir(m.id)
    } else {
        return Err(format!("未知组件：{id}"));
    };
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除失败：{e}"))?;
    }
    if let Ok(mut t) = rt.tasks.lock() { t.remove(&id); }
    Ok(status(&rt))
}

#[tauri::command]
pub async fn voice_pick_pack(window: tauri::WebviewWindow) -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(move || {
            rfd::FileDialog::new()
                .set_parent(&window)
                .set_title("选择语音包（.zip）")
                .add_filter("语音包", &["zip"])
                .pick_file()
                .map(|p| p.to_string_lossy().to_string())
        })
        .await
        .map_err(|e| format!("打开文件选择器失败：{e}"))
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err("文件选择器仅支持 Windows".into())
    }
}

#[tauri::command]
pub async fn voice_import_pack(rt: VState<'_>, path: String, overwrite: Option<bool>) -> Result<VoicePack, String> {
    let rt = rt.inner().clone();
    tauri::async_runtime::spawn_blocking(move || import_pack(&rt, Path::new(path.trim().trim_matches('"')), overwrite.unwrap_or(false)))
        .await
        .map_err(|e| format!("导入任务异常：{e}"))?
}

#[tauri::command]
pub async fn voice_open_dir(rt: VState<'_>) -> Result<(), String> {
    std::fs::create_dir_all(&rt.root).map_err(|e| e.to_string())?;
    open::that(&rt.root).map_err(|e| format!("打开目录失败：{e}"))
}

#[derive(Serialize)]
pub struct SttResult {
    pub text: String,
    pub engine: String,
}

/// 语音识别：pcm = 16k 单声道 PCM16LE 的 base64
#[tauri::command]
pub async fn voice_stt(db: DbState<'_>, rt: VState<'_>, pcm: String, rate: Option<u32>, engine: Option<String>) -> Result<SttResult, String> {
    if pcm.is_empty() { return Err("没有录到声音".into()); }
    if pcm.len() > MAX_PCM_B64 { return Err("录音太长（最长 60 秒）".into()); }
    let cfg = load_config(&db);
    let engine = engine.filter(|e| e == "local" || e == "baidu").unwrap_or_else(|| cfg.input_engine.clone());
    let rate = rate.unwrap_or(16000);
    let rt = rt.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let text = if engine == "baidu" {
            baidu_stt(&rt, &cfg, &pcm, rate)?
        } else {
            let m = model_spec("asr-sensevoice").unwrap();
            if !rt.model_installed(m) { return Err("本地识别模型（SenseVoice）未安装：请在 设置 → 全局助手 → 语音 中安装".into()); }
            let v = rt.request(json!({ "op": "asr", "dir": rt.model_dir(m.id), "pcm": pcm, "rate": rate }), Duration::from_secs(120))?;
            v.get("text").and_then(Value::as_str).unwrap_or("").to_string()
        };
        Ok(SttResult { text: text.trim().to_string(), engine })
    })
    .await
    .map_err(|e| format!("识别任务异常：{e}"))?
}

#[derive(Serialize)]
pub struct TtsResult {
    pub rate: u32,
    pub pcm: String,
    pub seconds: f64,
}

/// 本地语音合成：voice = "melo" | "kokoro" | "pack:<id>"；返回 PCM16LE base64
#[tauri::command]
pub async fn voice_tts(db: DbState<'_>, rt: VState<'_>, text: String, voice: String, speaker: Option<u32>) -> Result<TtsResult, String> {
    let text: String = text.trim().chars().take(400).collect();
    if text.is_empty() { return Ok(TtsResult { rate: 16000, pcm: String::new(), seconds: 0.0 }); }
    let cfg = load_config(&db);
    let req = if let Some(p) = voice.strip_prefix("pack:") {
        if !valid_id(p) { return Err("语音包 id 无效".into()); }
        let d = rt.packs_dir().join(p);
        if !d.join("voice.json").is_file() { return Err(format!("语音包未安装：{p}")); }
        let meta = read_pack_meta(&d).unwrap_or(Value::Null);
        if matches!(meta.get("engine").and_then(Value::as_str), Some("preset" | "sherpa-vits")) {
            preset_request(&rt, &meta, &d, &text, cfg.rate)?
        } else {
            json!({ "op": "tts", "engine": "pack", "dir": d, "text": text, "speed": cfg.rate })
        }
    } else {
        let id = match voice.as_str() { "melo" => "tts-melo", "kokoro" => "tts-kokoro", other => return Err(format!("未知语音：{other}")) };
        let m = model_spec(id).unwrap();
        if !rt.model_installed(m) { return Err(format!("{} 未安装", m.name)); }
        json!({ "op": "tts", "engine": voice, "dir": rt.model_dir(id), "text": text, "speed": cfg.rate, "sid": speaker.unwrap_or(cfg.kokoro_speaker) })
    };
    let rt = rt.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let v = rt.request(req, Duration::from_secs(120))?;
        Ok(TtsResult {
            rate: v.get("rate").and_then(Value::as_u64).unwrap_or(22050) as u32,
            pcm: v.get("pcm").and_then(Value::as_str).unwrap_or("").to_string(),
            seconds: v.get("seconds").and_then(Value::as_f64).unwrap_or(0.0),
        })
    })
    .await
    .map_err(|e| format!("合成任务异常：{e}"))?
}

/// 停止常驻语音服务（释放内存；下次使用时自动启动）
#[tauri::command]
pub async fn voice_stop_server(rt: VState<'_>) -> Result<(), String> {
    rt.stop_server();
    Ok(())
}

#[derive(Serialize)]
pub struct KwsResult {
    /// 这一段里命中的唤醒词（原样，如 "Christina"）
    pub hits: Vec<String>,
    /// 唤醒词列表刚生效时返回模型分词结果（调试用）
    pub tokens: Vec<String>,
}

fn kws_request(cfg: &VoiceConfig, rt: &VoiceRuntime, pcm: &str, rate: u32) -> Result<Value, String> {
    let m = model_spec("kws-gigaspeech").unwrap();
    if !rt.runtime_ready() { return Err("语音运行环境未安装".into()); }
    if !rt.model_installed(m) { return Err("唤醒词模型未安装".into()); }
    let words = wake_words(&cfg.wake_keywords);
    Ok(json!({
        "op": "kws_feed", "dir": rt.model_dir(m.id), "keywords": if words.is_empty() { vec![DEFAULT_WAKE.to_string()] } else { words },
        "threshold": cfg.wake_threshold, "boost": WAKE_BOOST, "pcm": pcm, "rate": rate,
    }))
}

/// 唤醒词检测：pcm = 一小段（约 0.3 秒）16k 单声道 PCM16LE base64；检测状态在服务里跨调用保持
#[tauri::command]
pub async fn voice_kws_feed(db: DbState<'_>, rt: VState<'_>, pcm: String, rate: Option<u32>) -> Result<KwsResult, String> {
    if pcm.len() > 400_000 { return Err("音频片段太长".into()); }
    let cfg = load_config(&db);
    if !cfg.wake_enabled { return Err("唤醒词未开启".into()); }
    let req = kws_request(&cfg, &rt, &pcm, rate.unwrap_or(16000))?;
    let rt = rt.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let v = rt.request_on(Slot::Kws, req, Duration::from_secs(30))?;
        let strs = |k: &str| v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        Ok(KwsResult { hits: strs("hits"), tokens: strs("tokens") })
    })
    .await
    .map_err(|e| format!("唤醒词检测异常：{e}"))?
}

/// 停止唤醒词检测实例（关闭唤醒词 / 窗口隐藏时释放内存）
#[tauri::command]
pub async fn voice_kws_stop(rt: VState<'_>) -> Result<(), String> {
    let rt = rt.inner().clone();
    tauri::async_runtime::spawn_blocking(move || rt.stop_slot(Slot::Kws)).await.map_err(|e| e.to_string())
}

/// 后台唤醒：把主窗口拉到前台
#[tauri::command]
pub async fn voice_wake_raise(app: AppHandle) -> Result<(), String> {
    crate::raise_main_window(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_config_normalizes() {
        let c = VoiceConfig { input_engine: "x".into(), default_voice: "?".into(), rate: 9.0, volume: f32::NAN, input_max_seconds: 999, github_prefix: " https://gh.example ".into(), pypi_index: "https://m/simple/".into(), ..Default::default() }.normalized();
        assert_eq!(c.input_engine, "local");
        assert_eq!(c.default_voice, "system");
        assert_eq!(c.rate, 2.0);
        assert_eq!(c.volume, 1.0);
        assert_eq!(c.input_max_seconds, 60);
        assert_eq!(c.github_prefix, "https://gh.example/");
        assert_eq!(c.pypi_index, "https://m/simple");
        assert_eq!(pypi_index(&VoiceConfig { source: "cn".into(), ..Default::default() }).as_deref(), Some(CN_PYPI));
        assert_eq!(github_urls(&c, "https://github.com/a").len(), 2);
        assert_eq!(c.wake_keywords, "Christina");
        assert_eq!(c.wake_action, "listen");
        let w = VoiceConfig { wake_keywords: "  hey   Kurisu，christina; Christina,你好,Amadeus!,a,b,c".into(), wake_threshold: 9.0, wake_action: "x".into(), ..Default::default() }.normalized();
        assert_eq!(w.wake_keywords, "hey Kurisu, christina, Amadeus, a");
        assert_eq!(w.wake_threshold, 0.6);
        let e = VoiceConfig { wake_keywords: "你好".into(), wake_threshold: f32::NAN, wake_action: "open".into(), ..Default::default() }.normalized();
        assert_eq!((e.wake_keywords.as_str(), e.wake_threshold, e.wake_action.as_str()), ("Christina", 0.25, "open"));
        // 旧配置（没有唤醒词字段）按默认值读入
        let old: VoiceConfig = serde_json::from_str(r#"{"input_enabled":true}"#).unwrap();
        assert!(!old.wake_enabled && old.wake_chime && old.wake_keywords == "Christina" && old.wake_multiturn);
    }

    #[test]
    fn helpers() {
        assert_eq!(b64_len("AAAA"), 3);
        assert_eq!(b64_len("AAA="), 2);
        assert_eq!(b64_len("AA=="), 1);
        assert_eq!(url_enc("a b+/"), "a%20b%2B%2F");
        assert!(valid_id("amadeus-kurisu") && !valid_id("../x") && !valid_id("Ab") && !valid_id(".x"));
        assert!(SERVER_PY.contains("def op_tts") && SERVER_PY.contains("\"kws_feed\": op_kws_feed"));
        assert_eq!(Slot::Kws.log(), "voice_kws.log");
        assert!(model_spec("kws-gigaspeech").is_some_and(|m| m.kind == "kws"));
    }

    #[test]
    fn import_pack_validates() {
        let dir = std::env::temp_dir().join(format!("sd-voice-{}", uuid::Uuid::new_v4()));
        let rt = VoiceRuntime::new(dir.clone());
        let zp = dir.join("p.zip");
        std::fs::create_dir_all(&dir).unwrap();
        {
            let f = std::fs::File::create(&zp).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("pack/voice.json", o).unwrap();
            z.write_all(br#"{"id":"t-voice","name":"T","lang":"ja","engine":"espnet-vits-onnx","skins":["kurisu-dynamic"]}"#).unwrap();
            z.start_file("pack/model.onnx", o).unwrap();
            z.write_all(b"x").unwrap();
            z.start_file("pack/tokens.json", o).unwrap();
            z.write_all(b"[]").unwrap();
            z.finish().unwrap();
        }
        let p = import_pack(&rt, &zp, false).unwrap();
        assert_eq!(p.id, "t-voice");
        assert!(import_pack(&rt, &zp, false).unwrap_err().starts_with("EXISTS:"));
        assert!(import_pack(&rt, &zp, true).is_ok());
        let packs = list_packs(&rt);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].skins, vec!["kurisu-dynamic".to_string()]);
        // preset：只有 voice.json，base 必须是 kokoro / melo；合成前要求底模已安装
        let preset = |name: &str, json: &[u8]| {
            let zp = dir.join(name);
            let mut z = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
            z.start_file("voice.json", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(json).unwrap();
            z.finish().unwrap();
            zp
        };
        let ok = preset("ok.zip", br#"{"id":"fish","name":"Fish","lang":"zh","engine":"preset","base":"kokoro","sid":50,"pitch":2.5,"skins":["dafeiyu-dynamic"]}"#);
        let p = import_pack(&rt, &ok, false).unwrap();
        assert_eq!((p.engine.as_str(), p.base.as_str()), ("preset", "kokoro"));
        let bad = preset("bad.zip", br#"{"id":"fish2","engine":"preset","base":"espeak"}"#);
        assert!(import_pack(&rt, &bad, false).unwrap_err().contains("base"));
        let meta: Value = serde_json::from_str(r#"{"name":"Fish","base":"kokoro","sid":999,"pitch":40,"speed":0.1}"#).unwrap();
        let pd = rt.packs_dir().join("fish");
        assert!(preset_request(&rt, &meta, &pd, "你好", 1.0).unwrap_err().contains("Kokoro"));
        std::fs::create_dir_all(rt.model_dir("tts-kokoro")).unwrap();
        std::fs::write(rt.model_dir("tts-kokoro").join(model_spec("tts-kokoro").unwrap().check), b"x").unwrap();
        let r = preset_request(&rt, &meta, &pd, "你好", 1.0).unwrap();
        assert_eq!((r["engine"].as_str(), r["sid"].as_u64(), r["pitch"].as_f64(), r["speed"].as_f64()), (Some("preset"), Some(102), Some(8.0), Some(0.5)));
        // sherpa-vits：模型 / lexicon / tokens 必须在包里；rule_fsts 不得越界
        let vz = |name: &str, json: &[u8], files: &[&str]| {
            let zp = dir.join(name);
            let mut z = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("gv/voice.json", o).unwrap();
            z.write_all(json).unwrap();
            for f in files { z.start_file(format!("gv/{f}"), o).unwrap(); z.write_all(b"x").unwrap(); }
            z.finish().unwrap();
            zp
        };
        let gv = br#"{"id":"gv","name":"GV","lang":"zh","engine":"sherpa-vits","sid":127,"rule_fsts":["number.fst"]}"#;
        assert!(import_pack(&rt, &vz("gv0.zip", gv, &["model.onnx", "lexicon.txt"]), false).unwrap_err().contains("tokens.txt"));
        assert!(import_pack(&rt, &vz("gv1.zip", gv, &["model.onnx", "lexicon.txt", "tokens.txt"]), false).unwrap_err().contains("number.fst"));
        let evil = br#"{"id":"gv","engine":"sherpa-vits","model":"../x.onnx"}"#;
        assert!(import_pack(&rt, &vz("gv2.zip", evil, &["model.onnx", "lexicon.txt", "tokens.txt"]), false).is_err());
        let p = import_pack(&rt, &vz("gv3.zip", gv, &["model.onnx", "lexicon.txt", "tokens.txt", "number.fst"]), false).unwrap();
        assert_eq!(p.engine, "sherpa-vits");
        let gmeta = read_pack_meta(&rt.packs_dir().join("gv")).unwrap();
        let r = preset_request(&rt, &gmeta, &rt.packs_dir().join("gv"), "你好", 1.0).unwrap();
        assert_eq!((r["base"].as_str(), r["sid"].as_u64()), (Some("vits"), Some(127)));
        // preset 指向 sherpa-vits 包：只带 voice.json 的角色小包
        let ayaka = preset("ayaka.zip", br#"{"id":"gv-ayaka","name":"Ayaka","lang":"zh","engine":"preset","base":"pack:gv","sid":87,"pitch":-0.5}"#);
        let p = import_pack(&rt, &ayaka, false).unwrap();
        assert_eq!(p.base, "pack:gv");
        let am = read_pack_meta(&rt.packs_dir().join("gv-ayaka")).unwrap();
        let r = preset_request(&rt, &am, &rt.packs_dir().join("gv-ayaka"), "你好", 1.0).unwrap();
        assert_eq!((r["base"].as_str(), r["sid"].as_u64(), r["dir"].as_str().map(|d| d.ends_with("gv"))), (Some("vits"), Some(87), Some(true)));
        let orphan: Value = serde_json::from_str(r#"{"name":"O","engine":"preset","base":"pack:nope"}"#).unwrap();
        assert!(preset_request(&rt, &orphan, &pd, "你好", 1.0).unwrap_err().contains("nope"));
        let selfref = preset("self.zip", br#"{"id":"loop","engine":"preset","base":"pack:loop"}"#);
        assert!(import_pack(&rt, &selfref, false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 联网实测（默认忽略）：SHIDRIVE_VOICE_LIVE_DIR=<数据目录> cargo test --bin shidrive voice::tests::live -- --ignored --nocapture
    /// 可选：SHIDRIVE_VOICE_LIVE_PACK=<语音包 zip>，SHIDRIVE_VOICE_LIVE_MODELS=asr-sensevoice,tts-melo，SHIDRIVE_VOICE_SOURCE=cn
    #[test]
    #[ignore]
    fn live() {
        let Ok(dir) = std::env::var("SHIDRIVE_VOICE_LIVE_DIR") else { eprintln!("skip: SHIDRIVE_VOICE_LIVE_DIR not set"); return };
        let rt = VoiceRuntime::new(PathBuf::from(dir));
        let cfg = VoiceConfig { source: std::env::var("SHIDRIVE_VOICE_SOURCE").unwrap_or_else(|_| "official".into()), ..Default::default() }.normalized();
        let job = |id: &str| Job { app: None, rt: &rt, id: id.into(), cancel: Arc::new(AtomicBool::new(false)), cfg_proxy: cfg.proxy.clone() };
        let t = Instant::now();
        if !rt.runtime_ready() {
            println!("runtime: {}", install_runtime(&job("runtime"), &cfg).unwrap());
        }
        println!("ready {:?} {:.1}s", std::fs::read_to_string(rt.ready_file()).unwrap_or_default(), t.elapsed().as_secs_f64());
        for id in std::env::var("SHIDRIVE_VOICE_LIVE_MODELS").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
            let m = model_spec(id).unwrap();
            if !rt.model_installed(m) { println!("{id}: {}", install_model(&job(id), &cfg, m).unwrap()); }
        }
        let tts = |req: Value| {
            let t = Instant::now();
            let v = rt.request(req, Duration::from_secs(180)).unwrap();
            let secs = v.get("seconds").and_then(Value::as_f64).unwrap_or(0.0);
            println!("tts rate={} seconds={secs:.2} took={:.2}s", v["rate"], t.elapsed().as_secs_f64());
            assert!(secs > 0.3);
            v
        };
        let asr = |v: &Value| {
            let m = model_spec("asr-sensevoice").unwrap();
            if !rt.model_installed(m) { return; }
            let r = rt.request(json!({ "op": "asr", "dir": rt.model_dir(m.id), "pcm": v["pcm"], "rate": v["rate"] }), Duration::from_secs(120)).unwrap();
            println!("asr: {}", r["text"]);
        };
        if let Ok(zp) = std::env::var("SHIDRIVE_VOICE_LIVE_PACK") {
            let p = import_pack(&rt, Path::new(&zp), true).unwrap();
            let v = tts(json!({ "op": "tts", "engine": "pack", "dir": rt.packs_dir().join(&p.id), "text": "まあ、いいわ。あなたの方から聞いてくれるなら、説明してあげる。", "speed": 1.0 }));
            asr(&v);
        }
        if rt.model_installed(model_spec("tts-melo").unwrap()) {
            let v = tts(json!({ "op": "tts", "engine": "melo", "dir": rt.model_dir("tts-melo"), "text": "你好，我是使驾助手。今天的构建已经完成了。", "speed": 1.0, "sid": 0 }));
            asr(&v);
        }
        rt.stop_server();
    }
}
