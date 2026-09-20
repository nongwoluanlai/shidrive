use serde::{Deserialize, Serialize};

pub type Id = String;

/// 内置「无项目」：不绑定工作目录，只存放便捷工作流；位于项目列表首位。
pub const NO_PROJECT_ID: &str = "00000000-0000-0000-0000-000000000000";
pub const NO_PROJECT_NAME: &str = "无项目";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    pub root_path: String,
    pub description: String,
    pub sort: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    pub id: Id,
    pub project_id: Id,
    pub name: String,
    pub overview: String,
    pub constraints: String,
    pub sort: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub id: Id,
    pub context_id: Id,
    /// todo | progress | decision | note
    pub kind: String,
    pub content: String,
    /// open | done (todos)
    pub status: String,
    pub sort: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBinding {
    pub id: Id,
    pub context_id: Id,
    /// codex | zcode | ...
    pub agent_type: String,
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub workspace: Option<String>,
    /// running | completed | interrupted | "" (未创建)
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub model: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingInfo {
    #[serde(flatten)]
    pub binding: AgentBinding,
    pub context_name: String,
    pub project_name: String,
}

/// One transcript row replayed from an ACP session and returned to the UI.
/// Chat history is not persisted locally — the bound ACP session is the source of truth.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayRow {
    pub role: String,
    pub content: String,
}

/// A session advertised by the adapter via session/list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

// ---------- Shared context (git-like commits) ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScCommit {
    pub id: Id,
    pub context_id: Id,
    pub seq: i64,
    pub agent_type: String,
    pub session_id: Option<String>,
    pub summary: String,
    /// JSON array of file paths
    pub files: String,
    /// JSON snapshot { overview, todos, progress, notes, constraints }
    pub snapshot: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScSnapshot {
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub todos: Vec<ScEntry>,
    #[serde(default)]
    pub progress: Vec<ScEntry>,
    #[serde(default)]
    pub notes: Vec<ScEntry>,
    #[serde(default)]
    pub constraints: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScEntry {
    pub content: String,
    #[serde(default)]
    pub status: String,
}

// ---------- Workflows ----------

/// Directed edge between step indices (free-form graph; cycles rejected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: i64,
    pub to: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ScheduleConfig {
    /// every N minutes
    #[serde(rename = "interval")]
    Interval { every_minutes: i64 },
    /// daily at HH:MM
    #[serde(rename = "daily")]
    Daily { time: String },
    /// weekly on selected weekdays (1=Mon .. 7=Sun) at HH:MM
    #[serde(rename = "weekly")]
    Weekly { weekdays: Vec<u32>, time: String },
    /// one shot at local datetime "YYYY-MM-DD HH:MM"
    #[serde(rename = "once")]
    Once { at: String },
}

/// Shared fields of every step (flattened into the tagged enum JSON by hand in db.rs helpers).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepMeta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub continue_on_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkflowStep {
    #[serde(rename = "start")]
    Start {
        #[serde(default)]
        name: String,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
    },
    #[serde(rename = "shell")]
    Shell {
        #[serde(default)]
        name: String,
        command: String,
        #[serde(default)]
        cwd: String,
        /// cmd | powershell | python (python uses the configured interpreter)
        #[serde(default)]
        shell: String,
        #[serde(default)]
        timeout_sec: Option<u64>,
        #[serde(default)]
        continue_on_error: bool,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
    },
    #[serde(rename = "agent")]
    Agent {
        #[serde(default)]
        name: String,
        context_id: String,
        agent_type: String,
        prompt: String,
        /// None => new temp session; Some => resume the given session id
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        timeout_sec: Option<u64>,
        #[serde(default)]
        continue_on_error: bool,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
    },
    #[serde(rename = "note")]
    Note {
        #[serde(default)]
        name: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
    },
    #[serde(rename = "balloon")]
    Balloon {
        #[serde(default)]
        name: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        message: String,
        /// none | open（打开目录/文件位置）| url（浏览器打开）
        #[serde(default)]
        click_action: String,
        #[serde(default)]
        click_target: String,
        /// 气泡伴随提示音
        #[serde(default)]
        sound: bool,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
    },
    #[serde(rename = "delay")]
    Delay {
        #[serde(default)]
        name: String,
        seconds: u64,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
    },
    #[serde(rename = "env")]
    EnvSet {
        #[serde(default)]
        name: String,
        #[serde(default)]
        vars: std::collections::BTreeMap<String, String>,
        /// 变量备注（key → 展示名），画布优先显示备注
        #[serde(default)]
        labels: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
    },
}

impl WorkflowStep {
    pub fn meta(&self) -> StepMeta {
        match self {
            WorkflowStep::Start { name, x, y, .. } => StepMeta {
                name: name.clone(), x: *x, y: *y, continue_on_error: true,
            },
            WorkflowStep::Note { name, x, y, .. } => StepMeta {
                name: name.clone(), x: *x, y: *y, continue_on_error: true,
            },
            WorkflowStep::Balloon { name, x, y, .. } => StepMeta {
                name: name.clone(), x: *x, y: *y, continue_on_error: true,
            },
            WorkflowStep::Shell { name, x, y, continue_on_error, .. } => StepMeta {
                name: name.clone(), x: *x, y: *y, continue_on_error: *continue_on_error,
            },
            WorkflowStep::Agent { name, x, y, continue_on_error, .. } => StepMeta {
                name: name.clone(), x: *x, y: *y, continue_on_error: *continue_on_error,
            },
            WorkflowStep::Delay { name, x, y, .. } => StepMeta {
                name: name.clone(), x: *x, y: *y, continue_on_error: false,
            },
            WorkflowStep::EnvSet { name, x, y, .. } => StepMeta {
                name: name.clone(), x: *x, y: *y, continue_on_error: true,
            },
        }
    }

    pub fn set_pos(&mut self, x: f64, y: f64) {
        match self {
            WorkflowStep::Start { x: ox, y: oy, .. }
            | WorkflowStep::Note { x: ox, y: oy, .. }
            | WorkflowStep::Balloon { x: ox, y: oy, .. }
            | WorkflowStep::Shell { x: ox, y: oy, .. }
            | WorkflowStep::Agent { x: ox, y: oy, .. }
            | WorkflowStep::Delay { x: ox, y: oy, .. }
            | WorkflowStep::EnvSet { x: ox, y: oy, .. } => {
                *ox = x;
                *oy = y;
            }
        }
    }

    pub fn timeout(&self) -> Option<u64> {
        match self {
            WorkflowStep::Shell { timeout_sec, .. } | WorkflowStep::Agent { timeout_sec, .. } => *timeout_sec,
            _ => None,
        }
    }

    pub fn continue_on_error(&self) -> bool {
        self.meta().continue_on_error
    }

    pub fn is_start(&self) -> bool {
        matches!(self, WorkflowStep::Start { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: Id,
    pub project_id: Id,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    /// manual | schedule
    pub trigger_type: String,
    pub schedule: Option<ScheduleConfig>,
    pub steps: Vec<WorkflowStep>,
    /// initial workflow env vars (values support {{date:...}})
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// connection graph; empty => sequential chain
    #[serde(default)]
    pub edges: Vec<Edge>,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: Id,
    pub workflow_id: Id,
    /// manual | schedule
    pub trigger: String,
    /// running | success | failed | stopped
    pub status: String,
    pub log: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

// ---------- Agent config ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLaunch {
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

/// One image attachment on a chat prompt (base64 payload, no data: prefix).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptImage {
    pub data: String,
    #[serde(default = "default_image_mime")]
    pub mime: String,
}

fn default_image_mime() -> String {
    "image/png".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    #[serde(default = "default_preset")]
    pub preset: String,
    #[serde(default = "default_accent")]
    pub accent: String,
    #[serde(default = "default_radius")]
    pub radius: i32,
    #[serde(default = "default_font_size")]
    pub font_size: i32,
}

fn default_preset() -> String { "dark".into() }
fn default_accent() -> String { "#4da3ff".into() }
fn default_radius() -> i32 { 10 }
fn default_font_size() -> i32 { 14 }

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            preset: default_preset(),
            accent: default_accent(),
            radius: default_radius(),
            font_size: default_font_size(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStatus {
    pub node_path: String,
    pub codex_path: String,
    pub zcode_cli_path: String,
    pub codex_adapter_path: String,
    pub zcode_adapter_path: String,
    pub python_path: String,
    pub vscode_path: String,
    pub tools_dir: String,
    pub data_dir: String,
    pub mcp_port: i64,
}
