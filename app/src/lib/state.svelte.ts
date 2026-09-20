// Global reactive app state (Svelte 5 runes) + ACP event wiring.
import type {
  AgentType,
  TranscriptRow,
  Context,
  PermissionRequest,
  Project,
  Prompt,
  SessionReadyInfo,
  ThemeConfig,
  ToolCallUpdate,
  Workflow,
} from "./types";
import { api } from "./ipc";
import { applyTheme } from "./theme";

export type Tab = "chat" | "context" | "workflows";

/** 内置「无项目」的固定 ID（后端 models.rs NO_PROJECT_ID 同值） */
export const NO_PROJECT_ID = "00000000-0000-0000-0000-000000000000";
export const isNoProject = (id: string | null) => id === NO_PROJECT_ID;
export type Overlay = null | "sessions" | "tasks" | "prompts";

export interface FileEditorState {
  path: string;
  content: string;
  binary: boolean;
  dirty: boolean;
  /** collapsed => only a slim strip; open => editor visible */
  collapsed: boolean;
}

export interface DisplayItem {
  id: string;
  kind: "user" | "assistant" | "thought" | "tools" | "error";
  text: string;
  tools?: ToolCallUpdate[];
  streaming?: boolean;
  time?: string;
}

export interface Toast {
  id: number;
  kind: "info" | "ok" | "warn" | "error";
  text: string;
}

let uid = 0;
export const nextId = () => `i${++uid}`;

export const app = $state({
  ready: false,
  projects: [] as Project[],
  contexts: [] as Context[],
  projectId: null as string | null,
  contextId: null as string | null,
  tab: "chat" as Tab,
  agent: "" as string,
  agentStatus: {} as Record<string, string>,
  /** 启用的 agent（有序）：[{id, name}] */
  agents: [] as { id: string; name: string }[],
  theme: { preset: "dark", accent: "#4da3ff", radius: 10, font_size: 14 } as ThemeConfig,
  settingsOpen: false,
  overlay: null as Overlay,
  /** history-session binding dialog for (agent) */
  historyBind: null as AgentType | null,
  toasts: [] as Toast[],
  permissions: [] as PermissionRequest[],
  sessionInfo: {} as Record<string, SessionReadyInfo>,
  /** ctxId:agent -> bound session id (kept in sync by acp://session-ready) */
  bindingSession: {} as Record<string, string | null>,
  /** ctxId:agent -> session title (local note or adapter-provided) */
  bindingTitleMap: {} as Record<string, string | null>,
  /** per-agent cached capability payload (models/configOptions) for pre-session display */
  agentCaps: {} as Record<string, { models?: unknown; configOptions?: unknown }>,
  /** config chosen before a session existed; applied on session-ready */
  pendingCfg: {} as Record<string, { id: string; value: string }>,
  chat: {} as Record<string, DisplayItem[]>,
  /** bumped whenever a chat key is (re)loaded/cleared — scroll anchoring reads it */
  chatRev: {} as Record<string, number>,
  /** per-key busy flag while the transcript is being replayed from the ACP session */
  chatLoading: {} as Record<string, boolean>,
  streaming: {} as Record<string, boolean>,
  /** right-dock file tree visible */
  fileTreeOpen: true,
  /** open editor panel (file preview/edit) */
  editor: null as FileEditorState | null,
  /** workflows of the current project (sidebar bottom half) */
  workflows: [] as Workflow[],
  /** currently selected workflow id in the workflows view */
  workflowSelected: null as string | null,
  /** workflowId -> runId for runs currently executing (global so state survives view switches) */
  wfRunning: {} as Record<string, string>,
  /** runId -> accumulated live log */
  wfLiveLogs: {} as Record<string, string>,
  /** 常用提示词 */
  prompts: [] as Prompt[],
  /** set by prompts panel, consumed by chat composer */
  insertPrompt: null as { text: string; at: number } | null,
  mcpPort: 8345,
});

export const chatKey = (ctxId: string | null, agent: string) => `${ctxId ?? "-"}:${agent}`;

export const currentProject = () => app.projects.find((p) => p.id === app.projectId) ?? null;
export const currentContext = () => app.contexts.find((c) => c.id === app.contextId) ?? null;

export function toast(kind: Toast["kind"], text: string) {
  const id = Date.now() + Math.random();
  app.toasts.push({ id, kind, text });
  setTimeout(() => {
    const i = app.toasts.findIndex((t) => t.id === id);
    if (i >= 0) app.toasts.splice(i, 1);
  }, 4200);
}

// ---------- chat transcript helpers ----------

function ensureChat(key: string): DisplayItem[] {
  if (!app.chat[key]) app.chat[key] = [];
  return app.chat[key];
}

export function historyToItems(rows: TranscriptRow[]): DisplayItem[] {
  const items: DisplayItem[] = [];
  const pushToolRow = (raw: string, time?: string) => {
    let tools: ToolCallUpdate[] = [];
    try {
      tools = JSON.parse(raw);
    } catch {
      /* ignore */
    }
    // merge tool_call + later tool_call_update entries that share a toolCallId
    const merged: ToolCallUpdate[] = [];
    for (const t of tools) {
      const existing = merged.find((m) => m.toolCallId === t.toolCallId);
      if (existing) Object.assign(existing, t);
      else merged.push(t);
    }
    if (merged.length) items.push({ id: nextId(), kind: "tools", text: "", tools: merged, time });
  };
  for (const r of rows) {
    if (r.role === "tools") {
      pushToolRow(r.content, r.created_at);
    } else {
      const kind = r.role as DisplayItem["kind"];
      items.push({ id: nextId(), kind, text: r.content, time: r.created_at });
    }
  }
  return items;
}

/** Apply rows replayed from the bound ACP session (chat history is not stored locally). */
export function setChatRows(key: string, rows: TranscriptRow[]) {
  app.chat[key] = historyToItems(rows);
  app.chatRev[key] = (app.chatRev[key] ?? 0) + 1;
}

export function pushLocal(key: string, item: Omit<DisplayItem, "id">) {
  ensureChat(key).push({ id: nextId(), ...item });
}

export function clearChat(key: string) {
  app.chat[key] = [];
  app.chatRev[key] = (app.chatRev[key] ?? 0) + 1;
}

function upsertTool(list: DisplayItem[], update: ToolCallUpdate) {
  for (let i = list.length - 1; i >= 0; i--) {
    const it = list[i];
    if (it.kind !== "tools" || !it.tools) continue;
    const idx = it.tools.findIndex((t) => t.toolCallId === update.toolCallId);
    if (idx >= 0) {
      it.tools[idx] = { ...it.tools[idx], ...update };
      return;
    }
    // tool_call_update for unknown id inside the most recent live tools block
    if (update.sessionUpdate === "tool_call_update") continue;
  }
  list.push({ id: nextId(), kind: "tools", text: "", tools: [update] });
}

/** Apply one ACP session/update notification to the transcript. */
export function applySessionUpdate(agentType: AgentType, contextId: string | null, sessionId: string, update: { sessionUpdate?: string } & Record<string, unknown>) {
  const key = chatKey(contextId, agentType);
  const list = ensureChat(key);
  const type = update.sessionUpdate ?? "";
  const textOf = (content: unknown): string => {
    if (Array.isArray(content)) return content.map((c: any) => c?.text ?? "").join("");
    if (content && typeof content === "object" && "text" in (content as any)) return (content as any).text as string;
    return "";
  };
  switch (type) {
    case "agent_message_chunk": {
      const t = textOf(update.content);
      if (!t) return;
      let last = list[list.length - 1];
      if (!last || last.kind !== "assistant" || !last.streaming) {
        list.push({ id: nextId(), kind: "assistant", text: "", streaming: true });
        last = list[list.length - 1];
      }
      last.text += t;
      break;
    }
    case "agent_thought_chunk": {
      const t = textOf(update.content);
      if (!t) return;
      let last = list[list.length - 1];
      if (!last || last.kind !== "thought" || !last.streaming) {
        list.push({ id: nextId(), kind: "thought", text: "", streaming: true });
        last = list[list.length - 1];
      }
      last.text += t;
      break;
    }
    case "tool_call":
    case "tool_call_update": {
      upsertTool(list, update as unknown as ToolCallUpdate);
      break;
    }
    case "plan": {
      // render plan as a special tools block keyed by "plan"
      upsertTool(list, {
        sessionUpdate: "tool_call_update",
        toolCallId: "__plan__",
        title: "计划",
        kind: "plan",
        status: "in_progress",
        rawOutput: update.entries ?? [],
      });
      break;
    }
    case "current_mode_update": {
      const info = app.sessionInfo[key];
      if (info?.response?.modes) info.response.modes.currentModeId = String(update.currentModeId ?? "");
      break;
    }
    default:
      break;
  }
}

/** Mark streaming turns as finished (stop stream merging). */
export function finishTurn(agentType: AgentType, contextId: string | null) {
  const key = chatKey(contextId, agentType);
  const list = app.chat[key];
  if (list) for (const it of list) it.streaming = false;
  app.streaming[key] = false;
}

// ---------- settings persistence ----------

export async function saveTheme() {
  applyTheme(app.theme);
  await api.settingsSet("ui.theme", JSON.stringify(app.theme)).catch(() => {});
}

// ---------- boot ----------

export async function loadProjects(selectFirst = false) {
  app.projects = await api.projectsList();
  if (selectFirst && !app.projectId && app.projects.length) {
    await selectProject(app.projects[0].id);
  }
}

export async function selectProject(id: string | null) {
  app.projectId = id;
  app.contextId = null;
  app.contexts = id ? await api.contextsList(id) : [];
  await refreshWorkflows();
  if (app.contexts.length) await selectContext(app.contexts[0].id);
  else app.contextId = null;
}

export async function refreshWorkflows() {
  app.workflows = app.projectId ? await api.workflowsList(app.projectId).catch(() => []) : [];
}

export async function loadPrompts() {
  try {
    const raw = await api.settingsGet("ui.prompts");
    app.prompts = raw ? JSON.parse(raw) : [];
  } catch {
    app.prompts = [];
  }
}

export async function savePrompts() {
  await api.settingsSet("ui.prompts", JSON.stringify(app.prompts)).catch(() => {});
}

/** Copy the shared-context onboarding prompt for a context. */
export function sharedContextPrompt(contextId: string): string {
  return `http://127.0.0.1:${app.mcpPort}/skills?sharedsession=${contextId} ，请阅读这个技能，确认会话同步到共享上下文中`;
}

export function openEditor(path: string, content: string, binary: boolean) {
  app.editor = { path, content, binary, dirty: false, collapsed: false };
}

export function closeEditor() {
  app.editor = null;
}

export async function selectContext(id: string | null) {
  app.contextId = id;
}

export function switchAgent(agent: AgentType) {
  app.agent = agent;
  void api.acpStatus(agent).then((s) => (app.agentStatus[agent] = s));
}

// dev helper: inspect state from devtools/CDP
if (import.meta.env.DEV) {
  (window as any).__shidrive = { app, chatKey };
}
