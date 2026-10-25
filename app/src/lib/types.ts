// Shared TypeScript types mirroring the Rust models.

export type AgentType = string;
export type AgentId = string;

/** 内置 ShiDrive Agent 接口配置（API Key 不回显） */
export interface ShidriveAgentConfig {
  base_url: string;
  model: string;
  models: string;
  max_steps: number | null;
  temperature: number | null;
  /** 电脑操作工具（打开程序 / 窗口 / 截图 / 鼠标键盘） */
  desktop_tools: boolean;
  /** 模型支持读图 */
  vision: boolean;
}
export interface ShidriveAgentConfigView extends ShidriveAgentConfig {
  api_key_set: boolean;
  configured: boolean;
}

export interface AgentEnvStatusItem {
  id: string;
  name: string;
  npm: string | null;
  help: string;
  enabled: boolean;
  adapter_ready: boolean;
  adapter_path: string;
  manual_command: string;
  node_ready: boolean;
  node_path: string;
  auto_env: Record<string, string>;
  /** 仅 DeepSeek：dsh 来源 */
  deepseek?: DeepseekSource | null;
}

export interface DeepseekSource {
  /** acp | dsh | harness */
  mode: string;
  /** acp | manual | desktop | npm | invalid | none */
  kind: string;
  path: string;
  manual: string | null;
  desktop: string | null;
  npm: string | null;
  acp: string | null;
}

export interface OAuthPending {
  txnId: string;
  clientId: string;
  clientName: string;
  redirectUri: string;
  scopes: string[];
}

export interface OAuthToken {
  id: string;
  client_id: string;
  client_name: string;
  scopes: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

export interface Project {
  id: string;
  name: string;
  root_path: string;
  description: string;
  sort: number;
  created_at: string;
  updated_at: string;
}

export interface Context {
  id: string;
  project_id: string;
  name: string;
  overview: string;
  constraints: string;
  sort: number;
  created_at: string;
  updated_at: string;
}

export interface ContextEntry {
  id: string;
  context_id: string;
  kind: "todo" | "progress" | "decision" | "note";
  content: string;
  status: "open" | "done";
  sort: number;
  created_at: string;
  updated_at: string;
}

export interface BindingInfo extends AgentBinding {
  context_name: string;
  project_name: string;
}

// status is carried on AgentBinding (serde default)

export interface SessionInfo {
  session_id: string;
  title?: string | null;
  updated_at?: string | null;
  cwd?: string | null;
}

export interface Todo {
  id: string;
  text: string;
  done: boolean;
  /** 创建时间（ms） */
  at: number;
}

export interface Prompt {
  id: string;
  title: string;
  content: string;
}

export interface AgentBinding {
  id: string;
  context_id: string;
  agent_type: AgentType;
  session_id: string | null;
  title: string | null;
  workspace: string | null;
  status?: string;
  model?: string;
  created_at: string;
  updated_at: string;
}

export interface ScCommit {
  id: string;
  context_id: string;
  seq: number;
  agent_type: string;
  session_id: string | null;
  summary: string;
  files: string;
  snapshot: string;
  created_at: string;
}



/** One transcript row replayed from the bound ACP session. */
export interface TranscriptRow {
  role: "user" | "assistant" | "thought" | "tools" | "error";
  content: string;
  created_at?: string;
}

export type ScheduleConfig =
  | { kind: "interval"; every_minutes: number }
  | { kind: "daily"; time: string }
  | { kind: "weekly"; weekdays: number[]; time: string }
  | { kind: "once"; at: string; /** 执行后销毁：调度触发的运行成功后删除该工作流 */ delete_after_run?: boolean };

export type WorkflowStep =
  | {
      type: "start";
      name: string;
      x: number;
      y: number;
    }
  | {
      type: "note";
      name: string;
      text: string;
      /** 手动调整后的高度（px）；缺省按内容自适应 */
      h?: number;
      x: number;
      y: number;
    }
  | {
      type: "balloon";
      name: string;
      title: string;
      message: string;
      /** none | open（打开目录/文件位置）| url（浏览器打开） */
      click_action: string;
      click_target: string;
      sound: boolean;
      x: number;
      y: number;
    }
  | {
      type: "shell";
      name: string;
      command: string;
      cwd: string;
      /** cmd | powershell | python */
      shell: string;
      timeout_sec: number | null;
      continue_on_error: boolean;
      x: number;
      y: number;
    }
  | {
      type: "agent";
      name: string;
      context_id: string;
      agent_type: AgentType;
      prompt: string;
      /** null/empty => temp session */
      session_id: string | null;
      timeout_sec: number | null;
      continue_on_error: boolean;
      x: number;
      y: number;
    }
  | {
      type: "assistant";
      name: string;
      /** 发给全局助手的内容（支持插值） */
      message: string;
      timeout_sec: number | null;
      continue_on_error: boolean;
      /** 由助手自己创建（定时唤醒自己） */
      by_assistant?: boolean;
      x: number;
      y: number;
    }
  | { type: "delay"; name: string; seconds: number; x: number; y: number }
  | {
      type: "env";
      name: string;
      vars: Record<string, string>;
      labels?: Record<string, string>;
      /** 手动调整后的高度（px） */
      h?: number;
      x: number;
      y: number;
    };

export interface Edge {
  from: number;
  to: number;
}

/** Counts returned by projects_delete describing what was removed together with the project. */
export interface ProjectCleanup {
  contexts: number;
  workflows: number;
  grants: number;
  tokens_revoked: number;
}

export interface Workflow {
  id: string;
  project_id: string;
  name: string;
  description: string;
  enabled: boolean;
  trigger_type: "manual" | "schedule";
  schedule: ScheduleConfig | null;
  steps: WorkflowStep[];
  edges: Edge[];
  env: Record<string, string>;
  last_run_at: string | null;
  next_run_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface WorkflowRun {
  id: string;
  workflow_id: string;
  trigger: "manual" | "schedule";
  status: "running" | "success" | "failed" | "stopped";
  log: string;
  started_at: string;
  finished_at: string | null;
}

export interface AgentLaunch {
  command: string;
  args: string[];
  env: Record<string, string>;
}

export interface ThemeConfig {
  preset: "dark" | "light";
  accent: string;
  radius: number;
  font_size: number;
}

export interface SetupStatus {
  node_path: string;
  codex_path: string;
  zcode_cli_path: string;
  codex_adapter_path: string;
  zcode_adapter_path: string;
  python_path: string;
  vscode_path: string;
  tools_dir: string;
  data_dir: string;
  mcp_port: number;
}

export interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  mtime: string;
}

// ---------- ACP protocol shapes (subset used by the UI) ----------

export interface ContentBlockText {
  type: "text";
  text: string;
}

export interface ToolCallUpdate {
  sessionUpdate: "tool_call" | "tool_call_update";
  toolCallId: string;
  title?: string;
  kind?: string;
  status?: "pending" | "in_progress" | "completed" | "failed";
  /** Standard tool content, or a bounded string when the payload is oversized. */
  content?: unknown;
  rawInput?: unknown;
  rawOutput?: unknown;
  locations?: { path: string; line?: number }[];
}

export interface PlanEntry {
  content: string;
  priority: "high" | "medium" | "low";
  status: "pending" | "in_progress" | "completed";
}

export interface SessionUpdatePayload {
  sessionUpdate:
    | "user_message_chunk"
    | "agent_message_chunk"
    | "agent_thought_chunk"
    | "tool_call"
    | "tool_call_update"
    | "plan"
    | "current_mode_update"
    | string;
  content?: unknown;
  entries?: PlanEntry[];
  currentModeId?: string;
  [k: string]: unknown;
}

export interface PermissionOption {
  optionId: string;
  name: string;
  kind: "allow_once" | "allow_always" | "reject_once" | "reject_always" | string;
}

export interface PermissionRequest {
  requestId: string;
  agentType: AgentType;
  sessionId: string;
  params: {
    sessionId: string;
    toolCall?: { title?: string; kind?: string; rawInput?: unknown };
    options: PermissionOption[];
    [k: string]: unknown;
  };
}

export interface ElicitationRequest {
  requestId: string;
  agentType: AgentType;
  sessionId: string;
  params: Record<string, unknown>;
}

export interface SessionModes {
  currentModeId: string;
  availableModes: { id: string; name: string; description?: string }[];
}

export interface ConfigOption {
  id: string;
  name: string;
  value: unknown;
  options?: { value: unknown; name: string }[];
  [k: string]: unknown;
}

export interface SessionReadyInfo {
  title?: string | null;
  connectionId?: string;
  agentType: AgentType;
  contextId: string;
  sessionId: string;
  response: {
    sessionId: string;
    modes?: SessionModes;
    configOptions?: ConfigOption[];
    models?: { currentModelId?: string; currentModel?: string; availableModels?: { modelId: string; name: string }[] };
    [k: string]: unknown;
  };
}

export interface NodeStat {
  path: string;
  version: string;
  major: number;
  ok: boolean;
  deepseek_ok: boolean;
  source: string;
}

/** 全局日志（标题栏日志面板；后端 glog.rs） */
export interface GlobalLogEntry {
  id: number;
  time: string;
  level: "info" | "warn" | "error" | "success" | string;
  category: string;
  message: string;
}

export interface SearchOpts {
  root: string;
  query: string;
  mode: "files" | "content";
  caseSensitive?: boolean;
  wholeWord?: boolean;
  regex?: boolean;
  include?: string;
  exclude?: string;
  useIgnore?: boolean;
  maxResults?: number;
}
export interface SearchFileHit { path: string; rel: string; score: number; indices: number[] }
export interface SearchLineMatch { line: number; col: number; len: number; preview: string; pstart: number; plen: number }
export interface SearchContentHit { path: string; rel: string; matches: SearchLineMatch[]; more: boolean }
export interface SearchResult {
  files: SearchFileHit[];
  content: SearchContentHit[];
  totalMatches: number;
  searchedFiles: number;
  truncated: boolean;
  cancelled: boolean;
  elapsedMs: number;
}

// ---------- 全局助手 ----------
export interface AssistantApiConfig { base_url: string; api_key: string; model: string; temperature: number | null }
export interface AssistantConfig {
  enabled: boolean;
  provider: "acp" | "api";
  agent_type: string;
  /** ACP 模型配置值；null = 用适配器默认 */
  model: unknown;
  workspace: string;
  api: AssistantApiConfig;
  persona_mode: "skin" | "custom";
  custom_name: string;
  custom_persona: string;
  auto_memory: boolean;
  memory_limit: number;
  history_turns: number;
  share_app_state: boolean;
  bubble_seconds: number;
  care_enabled: boolean;
  care_interval_minutes: number;
  care_late_hour: number;
  care_voice: boolean;
  /** 给助手开放使驾本地 MCP（上下文 / 工作流 / 自己的定时等全部工具） */
  mcp_tools: boolean;
  /** 电脑操作（仅 API 模式）：终端、打开程序、窗口、截图 / 读图、鼠标键盘 */
  desktop_tools: boolean;
  desktop_confirm: boolean;
  desktop_hide: boolean;
  /** 模型支持读图（OpenAI image_url base64） */
  vision: boolean;
}
export interface AssistantConfigView {
  config: AssistantConfig;
  api_key_set: boolean;
  session_id: string;
  session_provider: string;
  session_agent: string;
  session_created_at: string;
  default_workspace: string;
  busy: boolean;
  /** 当前会话已问候或已有对话 */
  session_greeted: boolean;
}
export interface AssistantPersona { id: string; name: string; persona: string; greeting: string; locale: string }
export interface AssistantMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  provider: string;
  created_at: string;
  interrupted: boolean;
  greeting: boolean;
}
export interface AssistantMemory { id: string; content: string; source: "auto" | "manual" | "summary"; pinned: boolean; created_at: string; updated_at: string }
/** 字幕层：气泡里显示 / 朗读的短句；speech = 配音语言版本；derived = 模型没给、从正文推导 */
export interface AssistantSubtitle { text: string; speech: string; derived?: boolean }
export interface AssistantTurnResult { message: AssistantMessage | null; memories_added: number; memories_forgotten: number; end_conversation?: boolean; skipped?: boolean; subtitles?: AssistantSubtitle[]; has_body?: boolean }

// ---------------- 全局助手 · 语音 ----------------
export interface VoiceConfig {
  input_enabled: boolean;
  input_engine: "local" | "baidu";
  baidu_api_key: string;
  baidu_secret_key: string;
  input_auto_send: boolean;
  input_max_seconds: number;
  output_enabled: boolean;
  default_voice: "system" | "melo" | "kokoro";
  kokoro_speaker: number;
  system_voice: string;
  character_voice: boolean;
  /** 完整朗读：字幕之外正文也逐句读出 */
  read_full: boolean;
  bindings: Record<string, string>;
  rate: number;
  volume: number;
  source: "official" | "cn";
  github_prefix: string;
  pypi_index: string;
  proxy: string;
  wake_enabled: boolean;
  /** 英文唤醒词，逗号分隔 */
  wake_keywords: string;
  wake_threshold: number;
  wake_action: "listen" | "open";
  wake_background: boolean;
  wake_chime: boolean;
  wake_multiturn: boolean;
}
export interface VoiceTask { id: string; stage: string; received: number; total: number; message: string; running: boolean }
export interface VoicePack { id: string; name: string; lang: string; engine: string; /** engine = "preset" 的底模：kokoro | melo | pack:<sherpa-vits 语音包 id> */ base?: string; skins: string[]; characters: string[]; license: string; source: string; credits: string }
export interface VoiceModelStatus { id: string; kind: "asr" | "tts" | "kws"; name: string; size_mb: number; installed: boolean }
export interface VoiceStatus {
  root: string;
  runtime_installed: boolean;
  runtime_info: Record<string, string> | null;
  models: VoiceModelStatus[];
  packs: VoicePack[];
  tasks: VoiceTask[];
  server_running: boolean;
  kws_running: boolean;
}
export interface VoiceView { config: VoiceConfig; baidu_secret_set: boolean; status: VoiceStatus }
export interface SpeechHint { enabled: boolean; lang: string; layer: boolean; conversation?: boolean; /** 完整朗读：正文也读（配音模式下要求把要说的话全部写成字幕） */ full?: boolean }
