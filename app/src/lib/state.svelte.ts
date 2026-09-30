// Global reactive app state (Svelte 5 runes) + ACP event wiring.
import type {
  AgentType,
  TranscriptRow,
  Context,
  ElicitationRequest,
  PermissionRequest,
  OAuthPending,
  Project,
  Prompt,
  SessionReadyInfo,
  ThemeConfig,
  ToolCallUpdate,
  Workflow,
} from "./types";
import { api } from "./ipc";
import { applyTheme } from "./theme";
import { emitSkinEvent } from "./skin-fx/bus";

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

// IDs are UI identities, not process-local counters stored across restarts.
export const nextId = () => crypto.randomUUID();
export interface DraftImage { data: string; mime: string; preview: string }
export interface TurnIdentity { sessionId: string; turnId: string; connectionId?: string; source?: string }

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
  /** 页面切换时也保留待审批请求；标题栏随时可打开全局审批窗口。 */
  oauthPending: [] as OAuthPending[],
  oauthConsentOpen: false,
  overlay: null as Overlay,
  /** history-session binding dialog for (agent) */
  historyBind: null as AgentType | null,
  toasts: [] as Toast[],
  permissions: [] as PermissionRequest[],
  /** ACP elicitation：agent 请求用户输入（选项/自由文本） */
  elicitations: [] as ElicitationRequest[],
  sessionInfo: {} as Record<string, SessionReadyInfo>,
  /** ctxId:agent -> bound session id (kept in sync by acp://session-ready) */
  bindingSession: {} as Record<string, string | null>,
  /** ctxId:agent -> session title (local note or adapter-provided) */
  bindingTitleMap: {} as Record<string, string | null>,
  /** per-agent cached capability payload (models/configOptions) for pre-session display */
  agentCaps: {} as Record<string, { models?: unknown; configOptions?: unknown }>,
  /** config chosen before a session existed; applied on session-ready */
  pendingCfg: {} as Record<string, Record<string, string>>,
  /** 用户显式选择的会话配置（模型/模式等），key=agent:cfgId；跨会话/重启记住 */
  cfgPref: {} as Record<string, string>,
  /** 每个会话未发送的输入草稿，key=ctxId:agent；切页/切会话不丢 */
  drafts: {} as Record<string, string>,
  draftImages: {} as Record<string, DraftImage[]>,
  draftImageRev: {} as Record<string, number>,
  /** Generation guards for binding reads and async UI operations. */
  bindingRev: {} as Record<string, number>,
  sessionBusy: {} as Record<string, boolean>,
  sessionOccupied: {} as Record<string, boolean>,
  chatLoadId: {} as Record<string, string>,
  chatSession: {} as Record<string, string | null>,
  promptRequests: {} as Record<string, string>,
  pendingMessage: {} as Record<string, DisplayItem>,
  activeTurn: {} as Record<string, TurnIdentity>,
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

// ---------- 工具调用负载瘦身 ----------
// 聊天条目整体放在 `$state` 里：Svelte 5 的深代理会为每个被读到的属性生成一个常驻
// signal，persistChat 的 JSON.stringify 又会把整棵树读一遍——工具调用的命令输出 /
// 整文件 diff / MCP 结果（content、rawInput、rawOutput 三份）一旦进来，WebView 内存
// 就按历史体量成倍放大并随聊天一直驻留。MessageItem 只用 toolCallId/title/kind/
// status/locations/content/rawInput/rawOutput，展开详情限长，因此这里与后端
// (acp::compact_tool_update) 同口径：保留标准 content，并对超限字段截断。后端已瘦身过的负载
// 再过一遍是常数开销；实时 acp://update 与旧版本落库的本地快照则靠这里兜底。
const TOOL_OUTPUT_MAX_CHARS = 16 * 1024;
const TOOL_INPUT_MAX_CHARS = 64 * 1024;
const TOOL_LOCATIONS_MAX = 8;
function capToolField(v: unknown, max: number): unknown {
  if (v == null || typeof v === "boolean" || typeof v === "number") return v;
  if (typeof v === "string") return v.length <= max ? v : v.slice(0, max) + `\n…(截断，共 ${v.length} 字符)`;
  let s: string;
  try {
    s = JSON.stringify(v) ?? "";
  } catch {
    return undefined;
  }
  return s.length <= max ? v : s.slice(0, max) + `\n…(截断，共 ${s.length} 字符)`;
}
export function compactTool<T extends Partial<ToolCallUpdate>>(t: T): T {
  if (!t || typeof t !== "object") return t;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(t)) {
    if (k === "content") out[k] = capToolField(v, TOOL_OUTPUT_MAX_CHARS);
    else if (k === "rawInput") out[k] = capToolField(v, TOOL_INPUT_MAX_CHARS);
    else if (k === "rawOutput") out[k] = capToolField(v, TOOL_OUTPUT_MAX_CHARS);
    else if (k === "locations") out[k] = Array.isArray(v) ? v.slice(0, TOOL_LOCATIONS_MAX) : v;
    else out[k] = v;
  }
  return out as T;
}
function compactItem(it: DisplayItem): DisplayItem {
  return Array.isArray(it.tools) ? { ...it, tools: it.tools.map((t) => compactTool(t)) } : it;
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
    const byId = new Map<string, ToolCallUpdate>();
    for (const raw of tools) {
      const t = compactTool(raw);
      const existing = byId.get(t.toolCallId);
      if (existing) Object.assign(existing, t);
      else {
        merged.push(t);
        byId.set(t.toolCallId, t);
      }
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

/** Change binding identity before accepting snapshots/notifications for it. */
export function adoptBinding(key: string, sessionId: string | null) {
  const previous = app.bindingSession[key];
  if (previous === sessionId) return;
  app.bindingRev[key] = (app.bindingRev[key] ?? 0) + 1;
  if ((previous !== undefined && previous !== sessionId)
      || (app.chatSession[key] !== undefined && app.chatSession[key] !== sessionId)) {
    // A first prompt can create a session. Retain only its just-submitted user
    // message, not a transcript from the previous binding.
    const pending = app.promptRequests[key] ? app.pendingMessage[key] : undefined;
    clearChat(key);
    if (pending) app.chat[key] = [pending];
    delete app.sessionInfo[key];
    delete app.activeTurn[key];
    markTurn(key, false);
    if (!pending) app.streaming[key] = false;
  }
  app.bindingSession[key] = sessionId;
  app.chatSession[key] = sessionId;
}

export function invalidateBindingRead(key: string) {
  app.bindingRev[key] = (app.bindingRev[key] ?? 0) + 1;
}

export function setChatRows(key: string, rows: TranscriptRow[], sessionId = app.bindingSession[key] ?? null) {
  cancelPersist(key);
  app.chat[key] = historyToItems(rows);
  app.chatSession[key] = sessionId;
  app.chatRev[key] = (app.chatRev[key] ?? 0) + 1;
  if (rows.length) persistChat(key);
  else void queueStore(key, () => api.chatStoreDelete(key));
}

export function pushLocal(key: string, item: Omit<DisplayItem, "id">): DisplayItem {
  const message = { id: nextId(), ...item };
  ensureChat(key).push(message);
  app.chatSession[key] ??= app.bindingSession[key] ?? null;
  persistChat(key);
  return message;
}

export function clearChat(key: string) {
  cancelPersist(key);
  app.chat[key] = [];
  app.chatSession[key] = app.bindingSession[key] ?? null;
  app.chatRev[key] = (app.chatRev[key] ?? 0) + 1;
  void queueStore(key, () => api.chatStoreDelete(key));
}

export function addDraftImage(key: string, image: DraftImage, revision = app.draftImageRev[key] ?? 0) {
  if (revision !== (app.draftImageRev[key] ?? 0)) return;
  app.draftImages[key] ??= [];
  app.draftImages[key].push(image);
}
export function clearDraftImages(key: string) {
  app.draftImageRev[key] = (app.draftImageRev[key] ?? 0) + 1;
  app.draftImages[key] = [];
}

// ---------- 会话本地持久化 ----------
// Version 2 snapshots are bound to a SID. Legacy arrays are read once against a
// known binding, get fresh UI IDs, then migrate without changing their content.
const SNAPSHOT_MAX_CHARS = 8 * 1024 * 1024;
const persistTimers: Record<string, ReturnType<typeof setTimeout>> = {};
const storeQueues = new Map<string, Promise<void>>();
function queueStore(key: string, operation: () => Promise<unknown>): Promise<void> {
  const task = (storeQueues.get(key) ?? Promise.resolve()).then(operation).then(() => {}, () => {});
  storeQueues.set(key, task);
  void task.then(() => { if (storeQueues.get(key) === task) storeQueues.delete(key); });
  return task;
}
function cancelPersist(key: string) {
  clearTimeout(persistTimers[key]);
  delete persistTimers[key];
}
function snapshotJson(sessionId: string | null, items: DisplayItem[]) {
  return JSON.stringify({ version: 2, sessionId, items });
}
function persistNow(key: string) {
  cancelPersist(key);
  const list = app.chat[key];
  if (!list?.length) return;
  const sid = app.chatSession[key] ?? app.bindingSession[key] ?? null;
  let snapshot = list.map((it) => ({ ...it, streaming: false }));
  try {
    let json = snapshotJson(sid, snapshot);
    while (json.length > SNAPSHOT_MAX_CHARS && snapshot.length > 20) {
      snapshot = snapshot.slice(Math.ceil(snapshot.length / 4));
      json = snapshotJson(sid, snapshot);
    }
    if (json.length <= SNAPSHOT_MAX_CHARS) void queueStore(key, () => api.chatStoreSet(key, json));
  } catch { /* invalid/oversized state must not overwrite the last good snapshot */ }
}
export function persistChat(key: string) {
  if (!app.chat[key]?.length) return;
  cancelPersist(key);
  persistTimers[key] = setTimeout(() => persistNow(key), 400);
}

export async function loadChatLocal(key: string, sessionId = app.bindingSession[key] ?? null): Promise<DisplayItem[] | null> {
  try {
    await storeQueues.get(key);
    const raw = await api.chatStoreGet(key);
    if (!raw) return null;
    const saved = JSON.parse(raw);
    const legacy = Array.isArray(saved);
    if (legacy && !sessionId) return null; // an unbound context cannot own an old session's history
    if (!legacy && (saved?.version !== 2 || saved.sessionId !== sessionId)) return null;
    const rows: DisplayItem[] = legacy ? saved : saved.items;
    if (!Array.isArray(rows) || !rows.length) return null;
    const items = rows.map((row) => ({ ...compactItem(row), id: nextId(), streaming: false }));
    const json = snapshotJson(sessionId, items);
    if (json.length > SNAPSHOT_MAX_CHARS) return null;
    if (legacy || raw.length > SNAPSHOT_MAX_CHARS) {
      void queueStore(key, () => api.chatStoreSet(key, json));
    }
    return items;
  } catch { return null; }
}

/** Shared lists only: never reuse another session's current model/mode value. */
export function capabilityLists(caps: { models?: any; configOptions?: any }): { models?: any; configOptions?: any } {
  return {
    models: caps.models ? { ...caps.models, currentModelId: "", currentModel: "" } : undefined,
    configOptions: Array.isArray(caps.configOptions)
      ? caps.configOptions.map((o: any) => ({ ...o, currentValue: "" })) : undefined,
  };
}

// ---------- 内存中聊天条目的数量上限（LRU） ----------
// app.chat 以 context:agent 为键，从不释放：每打开一个会话就多驻留一份完整历史，
// 用得越久 WebView 越大。本地库里有全量快照（persistChat），切回来时秒开，因此内存中
// 只保留最近看过的若干份；正在流式输出 / 正在装载 / 后端回合进行中的会话不淘汰，
// 淘汰前先把待写入的快照落库。
const CHAT_LRU_MAX = 6;
const chatLru: string[] = [];
const activeTurns = new Set<string>();
/** 后端 acp://binding-status 的 running/completed 状态：回合进行中的会话不能淘汰。 */
export function markTurn(key: string, running: boolean) {
  if (running) activeTurns.add(key);
  else activeTurns.delete(key);
}
/** 记录 key 刚被查看；超出上限时淘汰最久未看的其它会话。 */
export function touchChat(key: string) {
  const i = chatLru.indexOf(key);
  if (i >= 0) chatLru.splice(i, 1);
  chatLru.push(key);
  for (let j = 0; j < chatLru.length && chatLru.length > CHAT_LRU_MAX; ) {
    const k = chatLru[j];
    if (k !== key && !app.chat[k]?.length) {
      // 已被 clearChat/淘汰等释放：只从 LRU 名单里去掉
      chatLru.splice(j, 1);
      continue;
    }
    if (k === key || app.streaming[k] || app.chatLoading[k] || activeTurns.has(k)) {
      j++;
      continue;
    }
    if (persistTimers[k]) persistNow(k);
    delete app.chat[k];
    // 同步清理该 key 的会话元数据：重进此会话时走完整加载路径
    // （否则 bindingSession 残留旧值导致 adoptBinding 认为无变化而跳过刷新）
    delete app.bindingSession[k];
    delete app.chatSession[k];
    delete app.sessionInfo[k];
    delete app.bindingTitleMap[k];
    delete app.chatLoading[k];
    delete app.streaming[k];
    delete app.sessionOccupied[k];
    delete app.chatLoadId[k];
    chatLru.splice(j, 1);
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

function upsertTool(list: DisplayItem[], raw: ToolCallUpdate) {
  const update = compactTool(raw);
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
export function applySessionUpdate(agentType: AgentType, contextId: string | null, sessionId: string, update: { sessionUpdate?: string } & Record<string, unknown>, route?: Partial<TurnIdentity>) {
  const key = chatKey(contextId, agentType);
  if (!contextId || !sessionId || app.bindingSession[key] !== sessionId || route?.source === "workflow") return;
  const connectionId = app.sessionInfo[key]?.connectionId;
  if (route?.connectionId && connectionId && route.connectionId !== connectionId) return;
  if (route?.turnId) {
    const active = app.activeTurn[key];
    if (!active || active.turnId !== route.turnId || (route.connectionId && active.connectionId !== route.connectionId)) return;
  }
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
        if (key === chatKey(app.contextId, app.agent)) emitSkinEvent("agent.streaming");
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
        toolCallId: `__plan__:${route?.turnId ?? app.activeTurn[key]?.turnId ?? "legacy"}`,
        title: "计划",
        kind: "plan",
        status: "in_progress",
        rawOutput: update.entries ?? [],
      });
      break;
    }
    case "config_option_update": {
      const info = app.sessionInfo[key];
      if (info && Array.isArray(update.configOptions)) info.response.configOptions = update.configOptions as NonNullable<typeof info.response.configOptions>;
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

/** Return true only for the accepted completion (after-action must fire once). */
export function applyBindingStatus(p: TurnIdentity & { contextId: string; agentType: string; status: string }): boolean {
  const key = chatKey(p.contextId, p.agentType);
  if (p.source === "workflow" || !p.turnId || app.bindingSession[key] !== p.sessionId) return false;
  if (app.promptRequests[key] && app.promptRequests[key] !== p.turnId) return false;
  if (p.status === "running") {
    app.activeTurn[key] = p;
    app.promptRequests[key] ??= p.turnId;
    app.streaming[key] = true;
    markTurn(key, true);
  } else if (p.status === "completed" || p.status === "interrupted") {
    const active = app.activeTurn[key];
    if (!active || active.turnId !== p.turnId || active.connectionId !== p.connectionId) return false;
    finishTurn(p.agentType, p.contextId, p.turnId);
    return true;
  }
  return false;
}

/** Never let an old IPC finally/completion finish a newer request. */
export function finishTurn(agentType: AgentType, contextId: string | null, requestId?: string) {
  const key = chatKey(contextId, agentType);
  if (requestId && app.promptRequests[key] !== requestId) return;
  const list = app.chat[key];
  if (list) for (const it of list) it.streaming = false;
  const wasStreaming = !!app.streaming[key];
  app.streaming[key] = false;
  // A failed turn already reported agent.error; don't follow it with a "done" reaction.
  if (wasStreaming && key === chatKey(app.contextId, app.agent) && list?.[list.length - 1]?.kind !== "error") emitSkinEvent("agent.done");
  delete app.promptRequests[key];
  delete app.pendingMessage[key];
  delete app.activeTurn[key];
  markTurn(key, false);
  persistChat(key);
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

// Every project switch gets a generation number; responses that arrive after a
// newer switch started are dropped so a slow request cannot resurrect stale
// contexts/workflows (or re-select a context) for a project that is no longer
// the current one.
let projectGen = 0;

// Hooks that run before `app.workflows` is replaced from the backend. The workflow
// editor registers its pending-save flush here, so a reload can never discard
// edits that are still waiting for the autosave debounce.
const beforeWorkflowsReload: Array<() => Promise<unknown>> = [];
export function onBeforeWorkflowsReload(fn: () => Promise<unknown>) {
  beforeWorkflowsReload.push(fn);
}
async function flushWorkflowEdits() {
  await Promise.all(beforeWorkflowsReload.map((fn) => fn().catch(() => {})));
}

export async function selectProject(id: string | null) {
  const gen = ++projectGen;
  await flushWorkflowEdits();
  if (gen !== projectGen) return;
  app.projectId = id;
  app.contextId = null;
  app.contexts = [];
  app.workflows = [];
  if (!id) return;
  const [contexts, workflows] = await Promise.all([
    api.contextsList(id),
    api.workflowsList(id).catch(() => [] as Workflow[]),
  ]);
  if (gen !== projectGen || app.projectId !== id) return;
  app.contexts = contexts;
  app.workflows = workflows;
  if (contexts.length) await selectContext(contexts[0].id);
}

export async function refreshContexts() {
  const id = app.projectId;
  const gen = projectGen;
  const list = id ? await api.contextsList(id).catch(() => [] as Context[]) : [];
  if (gen !== projectGen || app.projectId !== id) return;
  app.contexts = list;
}

export async function refreshWorkflows() {
  const id = app.projectId;
  const gen = projectGen;
  await flushWorkflowEdits();
  const list = id ? await api.workflowsList(id).catch(() => [] as Workflow[]) : [];
  // Ignore the result if the user switched projects while the request was in flight.
  if (gen !== projectGen || app.projectId !== id) return;
  app.workflows = list;
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
