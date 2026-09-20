// Shared TypeScript types mirroring the Rust models.

export type AgentType = string;
export type AgentId = string;

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
  | { kind: "once"; at: string };

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
  | { type: "delay"; name: string; seconds: number; x: number; y: number }
  | {
      type: "env";
      name: string;
      vars: Record<string, string>;
      labels?: Record<string, string>;
      x: number;
      y: number;
    };

export interface Edge {
  from: number;
  to: number;
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
  content?: unknown[];
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
  source: string;
}
