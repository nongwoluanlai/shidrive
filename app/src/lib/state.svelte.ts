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
  theme: { preset: "light", accent: "#268f78", radius: 10, font_size: 14 } as ThemeConfig,
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
  /** 用户显式选择的会话配置（模型/模式等），key=agent:cfgId；跨会话/重启记住 */
  cfgPref: {} as Record<string, string>,
  /** 每个会话未发送的输入草稿，key=ctxId:agent；切页/切会话不丢 */
  drafts: {} as Record<string, string>,
  chat: {} as Record<string, DisplayItem[]>,
  /** bumped whenever a chat key is (re)loaded/cleared — scroll anchoring reads it */
  chatRev: {} as Record<string, number>,
  /** per-key busy flag while the transcript is being replayed from the ACP session */
  chatLoading: {} as Record<string, boolean>,
  streaming: {} as Record<string, boolean>,
  /** right-dock file tree visible */
  fileTreeOpen: true,
  /** 回合结束后自增，目录树监听它自动刷新 */
  treeRev: 0,
  /** Enter 发送消息（默认关闭：Enter 换行、Ctrl+Enter 发送） */
  enterSend: false,
  /** 界面语言：zh 简体中文（默认）/ en English */
  locale: "zh" as "zh" | "en",
  /** 皮肤插件：空 = 不使用 */
  skin: "",
  /** 已导入的自定义皮肤（id → 名称） */
  customSkins: {} as Record<string, string>,
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
  // 错误/警告气泡同步写入日志文件，便于排查其它设备上的问题
  if (kind === "error" || kind === "warn") void api.uiLog(kind, text);
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
  void persistChat(key);
}

export function pushLocal(key: string, item: Omit<DisplayItem, "id">) {
  ensureChat(key).push({ id: nextId(), ...item });
  void persistChat(key);
}

export function clearChat(key: string) {
  app.chat[key] = [];
  app.chatRev[key] = (app.chatRev[key] ?? 0) + 1;
  void api.chatStoreDelete(key).catch(() => {});
}

// ---------- 会话本地持久化 ----------
// 每回合结束（finishTurn）或历史装载（setChatRows）时把整份条目快照到 SQLite，
// 打开聊天页先读本地立即渲染；适配器 session/load 全量重放只在本地为空时兜底。
let persistTimers: Record<string, ReturnType<typeof setTimeout>> = {};
export function persistChat(key: string) {
  const list = app.chat[key];
  if (!list || !list.length) return;
  clearTimeout(persistTimers[key]);
  persistTimers[key] = setTimeout(() => {
    const snapshot = (app.chat[key] ?? []).map((it) => ({ ...it, streaming: false }));
    void api.chatStoreSet(key, JSON.stringify(snapshot)).catch(() => {});
  }, 400);
}

/** 从本地库载入某会话的历史（无记录返回 null）。 */
export async function loadChatLocal(key: string): Promise<DisplayItem[] | null> {
  try {
    const raw = await api.chatStoreGet(key);
    if (!raw) return null;
    const rows = JSON.parse(raw) as DisplayItem[];
    return Array.isArray(rows) && rows.length ? rows : null;
  } catch {
    return null;
  }
}

// ---------- 会话配置记忆（模型/模式等） ----------
const CFG_PREF_KEY = "chat.cfgpref";
export function loadCfgPrefs() {
  void api
    .settingsGet(CFG_PREF_KEY)
    .then((raw) => {
      if (raw) app.cfgPref = JSON.parse(raw) as Record<string, string>;
    })
    .catch(() => {});
}
export function saveCfgPref(agent: string, cfgId: string, value: string) {
  app.cfgPref[`${agent}:${cfgId}`] = value;
  void api.settingsSet(CFG_PREF_KEY, JSON.stringify(app.cfgPref)).catch(() => {});
}
/** 该配置项的"完全访问"类选项值（各适配器叫法不同，取交集语义）。 */
const FULL_ACCESS_VALUES = new Set(["danger-full-access", "agent-full-access", "yolo", "bypasspermissions"]);
/** 按 cfgId 匹配偏好的完全访问值（找不到返回 undefined 表示不适用）。 */
export function fullAccessDefault(cfgId: string, optionValues: string[]): string | undefined {
  const id = cfgId.toLowerCase();
  const wanted =
    id === "sandbox" || id === "sandbox_mode" || id.includes("sandbox")
      ? ["danger-full-access"]
      : id.includes("approval")
        ? ["never", "agent"]
        : id === "mode" || id.includes("mode") || id.includes("permission")
          ? ["yolo", "bypasspermissions", "agent-full-access", "danger-full-access", "execute"]
          : [];
  for (const v of wanted) if (optionValues.includes(v)) return v;
  for (const v of optionValues) if (FULL_ACCESS_VALUES.has(v)) return v;
  return undefined;
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
  void persistChat(key);
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
