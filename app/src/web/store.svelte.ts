// 网站接入：全局状态与事件处理（移植自桌面端 state.svelte.ts 的会话更新逻辑，去掉 Tauri 依赖）。
import { api, rid, ApiError, type Agent, type Context, type Item, type PermissionReq, type Project, type ReplayRow, type ToolCall, type WorkflowSummary } from "./api";

export type View = "chat" | "files" | "workflows" | "context";

export interface ChatState {
  sessionId: string | null;
  title: string | null;
  items: Item[];
  running: boolean;
  turnId: string | null;
  loading: boolean;
  syncing: boolean;
  /** 本地快照已显示，但尚未从 Agent 会话重放校准 */
  fromSnapshot: boolean;
  error: string;
  /** 服务端省略的更早消息条数（长会话首屏只取最近一段） */
  omitted: number;
}

export interface Toast { id: number; kind: "info" | "ok" | "warn" | "error"; text: string }

const LS = "shidrive.web.";
const lsGet = (k: string) => { try { return localStorage.getItem(LS + k) ?? ""; } catch { return ""; } };
const lsSet = (k: string, v: string) => { try { localStorage.setItem(LS + k, v); } catch { /* ignore */ } };

export const ui = $state({
  authed: null as boolean | null,
  version: "",
  connected: false,
  theme: (lsGet("theme") || (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark")) as "dark" | "light",
  view: (lsGet("view") || "chat") as View,
  drawer: false,
  projects: [] as Project[],
  projectId: lsGet("project") || null as string | null,
  contexts: [] as Context[],
  contextId: lsGet("context") || null as string | null,
  agents: [] as Agent[],
  agent: lsGet("agent") || "",
  chats: {} as Record<string, ChatState>,
  permissions: [] as PermissionReq[],
  workflows: [] as WorkflowSummary[],
  wfRunning: {} as Record<string, string>,
  wfLogs: {} as Record<string, string>,
  wfRev: 0,
  /** 文件预览请求（聊天里点击路径时跳到文件页） */
  openFile: null as string | null,
  toasts: [] as Toast[],
});

export const chatKey = (ctx: string | null, agent: string) => `${ctx ?? "-"}:${agent}`;
export const currentProject = () => ui.projects.find((p) => p.id === ui.projectId) ?? null;
export const currentContext = () => ui.contexts.find((c) => c.id === ui.contextId) ?? null;
export const currentChat = (): ChatState | null => (ui.contextId && ui.agent ? ui.chats[chatKey(ui.contextId, ui.agent)] ?? null : null);

let toastSeq = 0;
export function toast(kind: Toast["kind"], text: string) {
  const id = ++toastSeq;
  ui.toasts.push({ id, kind, text });
  setTimeout(() => { ui.toasts = ui.toasts.filter((t) => t.id !== id); }, kind === "error" ? 6000 : 3500);
}
export const errText = (e: unknown) => (e instanceof ApiError ? e.message : String((e as any)?.message ?? e));

export function setTheme(t: "dark" | "light") {
  ui.theme = t;
  lsSet("theme", t);
  document.documentElement.dataset.theme = t;
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", t === "dark" ? "#0e1017" : "#f2f4f9");
}
export function setView(v: View) { ui.view = v; ui.drawer = false; lsSet("view", v); }

const nid = () => Math.random().toString(36).slice(2) + Date.now().toString(36);
const nowTime = () => new Date().toLocaleString("zh-CN", { hour12: false });

// ---------------------------------------------------------------- loading

export async function bootstrap() {
  try {
    const me = await api.me();
    ui.version = me.version;
    ui.authed = me.authed;
    if (me.authed) await loadAll();
  } catch (e) {
    ui.authed = false;
    toast("error", errText(e));
  }
}

export async function loadAll() {
  const [projects, agents] = await Promise.all([api.projects(), api.agents()]);
  ui.projects = projects;
  ui.agents = agents;
  if (!agents.some((a) => a.id === ui.agent)) ui.agent = agents[0]?.id ?? "";
  if (!projects.some((p) => p.id === ui.projectId)) ui.projectId = projects[0]?.id ?? null;
  await selectProject(ui.projectId, true);
  void refreshPermissions();
}

export async function selectProject(id: string | null, keepContext = false) {
  ui.projectId = id;
  lsSet("project", id ?? "");
  ui.contexts = id ? await api.contexts(id).catch((e) => { toast("error", errText(e)); return []; }) : [];
  if (!keepContext || !ui.contexts.some((c) => c.id === ui.contextId)) ui.contextId = ui.contexts[0]?.id ?? null;
  lsSet("context", ui.contextId ?? "");
  ui.wfRev++;
  void openChat();
}

export function selectContext(id: string) {
  ui.contextId = id;
  lsSet("context", id);
  ui.drawer = false;
  void openChat();
}

export function selectAgent(id: string) {
  ui.agent = id;
  lsSet("agent", id);
  void openChat();
}

function blankChat(): ChatState {
  return { sessionId: null, title: null, items: [], running: false, turnId: null, loading: true, syncing: false, fromSnapshot: false, error: "", omitted: 0 };
}

/** 打开当前上下文 + Agent 的会话：先显示桌面端保存的快照，无快照时从 Agent 会话重放历史 */
export async function openChat(force = false) {
  const ctx = ui.contextId, agent = ui.agent;
  if (!ctx || !agent) return;
  const key = chatKey(ctx, agent);
  const existing = ui.chats[key];
  if (existing && !force && !existing.error) { void refreshChatMeta(key, ctx, agent); return; }
  ui.chats[key] = existing ?? blankChat();
  const chat = ui.chats[key];
  chat.loading = true;
  chat.error = "";
  try {
    const r = await api.chat(ctx, agent);
    chat.sessionId = r.sessionId;
    chat.title = r.binding?.title ?? null;
    chat.running = r.binding?.status === "running";
    if (r.items?.length) {
      chat.items = r.items.map((it) => ({ ...it, id: nid(), streaming: false }));
      chat.omitted = r.omitted ?? 0;
      chat.fromSnapshot = true;
    } else if (!chat.running) {
      chat.items = [];
      chat.omitted = 0;
      if (r.sessionId) await syncHistory(key);
    }
  } catch (e) {
    chat.error = errText(e);
  } finally {
    chat.loading = false;
  }
}

/** 加载服务端省略的更早消息（快照直接取全量；重放结果需重新重放全量） */
export async function loadEarlier(key: string): Promise<boolean> {
  const chat = ui.chats[key];
  if (!chat || !chat.omitted || chat.loading || chat.syncing) return false;
  if (!chat.fromSnapshot) { await syncHistory(key, true); return true; }
  const [ctx, agent] = [key.slice(0, key.lastIndexOf(":")), key.slice(key.lastIndexOf(":") + 1)];
  chat.loading = true;
  try {
    const r = await api.chat(ctx, agent, 0);
    if (chat.running || r.sessionId !== chat.sessionId || !r.items?.length) return false;
    chat.items = r.items.map((it) => ({ ...it, id: nid(), streaming: false }));
    chat.omitted = 0;
    return true;
  } catch (e) {
    toast("warn", errText(e));
    return false;
  } finally {
    chat.loading = false;
  }
}

/** 已打开过的会话再次切回：只刷新绑定状态（运行中 / 会话变化） */
async function refreshChatMeta(key: string, ctx: string, agent: string) {
  const chat = ui.chats[key];
  try {
    const r = await api.chat(ctx, agent);
    if (r.sessionId !== chat.sessionId) { delete ui.chats[key]; return openChat(); }
    chat.title = r.binding?.title ?? chat.title;
    const running = r.binding?.status === "running";
    if (chat.running && !running) finishTurn(chat);
    chat.running = running;
  } catch { /* 网络抖动时保留现有内容 */ }
}

/** 从 Agent 会话重放历史（权威来源；会启动适配器，可能需要几秒） */
export async function syncHistory(key: string, full = false) {
  const chat = ui.chats[key];
  if (!chat || chat.syncing) return;
  const [ctx, agent] = [key.slice(0, key.lastIndexOf(":")), key.slice(key.lastIndexOf(":") + 1)];
  chat.syncing = true;
  try {
    const r = await api.history(ctx, agent, full ? 0 : undefined);
    if (chat.running) return; // 重放期间开始了新回合：以实时流为准
    chat.sessionId = r.sessionId;
    chat.items = rowsToItems(r.rows);
    chat.omitted = r.omitted ?? 0;
    chat.fromSnapshot = false;
  } catch (e) {
    toast("warn", `历史同步失败：${errText(e)}`);
  } finally {
    chat.syncing = false;
  }
}

function rowsToItems(rows: ReplayRow[]): Item[] {
  const items: Item[] = [];
  for (const r of rows) {
    if (r.role === "tools") {
      let tools: ToolCall[] = [];
      try { tools = JSON.parse(r.content); } catch { /* ignore */ }
      const merged: ToolCall[] = [];
      const byId = new Map<string, ToolCall>();
      for (const t of tools) {
        const ex = byId.get(t.toolCallId);
        if (ex) Object.assign(ex, t);
        else { merged.push(t); byId.set(t.toolCallId, t); }
      }
      if (merged.length) items.push({ id: nid(), kind: "tools", text: "", tools: merged });
    } else if (["user", "assistant", "thought", "error"].includes(r.role)) {
      items.push({ id: nid(), kind: r.role as Item["kind"], text: r.content });
    }
  }
  return items;
}

// ---------------------------------------------------------------- actions

export async function sendMessage(text: string): Promise<boolean> {
  const ctx = ui.contextId, agent = ui.agent;
  if (!ctx || !agent || !text.trim()) return false;
  const key = chatKey(ctx, agent);
  ui.chats[key] ??= { ...blankChat(), loading: false };
  const chat = ui.chats[key];
  const requestId = rid();
  chat.items.push({ id: nid(), kind: "user", text, time: nowTime(), requestId });
  chat.running = true;
  chat.turnId = requestId;
  try {
    await api.send(ctx, agent, text, requestId);
    return true;
  } catch (e) {
    chat.running = false;
    chat.items.push({ id: nid(), kind: "error", text: `发送失败：${errText(e)}` });
    return false;
  }
}

export async function cancelTurn() {
  if (!ui.contextId || !ui.agent) return;
  try { await api.cancel(ui.contextId, ui.agent); toast("info", "已请求停止"); } catch (e) { toast("error", errText(e)); }
}

export async function newSession() {
  const ctx = ui.contextId, agent = ui.agent;
  if (!ctx || !agent) return;
  const key = chatKey(ctx, agent);
  try {
    const r = await api.newSession(ctx, agent);
    ui.chats[key] = { ...blankChat(), loading: false, sessionId: r.sessionId, title: "新会话" };
    toast("ok", "已创建新会话");
  } catch (e) {
    toast("error", errText(e));
  }
}

export async function refreshPermissions() {
  try { ui.permissions = await api.permissions(); } catch { /* ignore */ }
}

export async function answerPermission(req: PermissionReq, optionId: string) {
  ui.permissions = ui.permissions.filter((p) => p.requestId !== req.requestId);
  try { await api.respondPermission(req.requestId, optionId); } catch (e) { toast("warn", `应答失败：${errText(e)}`); }
}

export async function logout() {
  try { await api.logout(); } catch { /* ignore */ }
  ui.authed = false;
  ui.chats = {};
}

// ---------------------------------------------------------------- live events

function textOf(content: unknown): string {
  if (Array.isArray(content)) return content.map((c: any) => c?.text ?? "").join("");
  if (content && typeof content === "object" && "text" in (content as any)) return String((content as any).text ?? "");
  return "";
}

function upsertTool(list: Item[], update: ToolCall) {
  for (let i = list.length - 1; i >= 0; i--) {
    const it = list[i];
    if (it.kind !== "tools" || !it.tools) continue;
    const idx = it.tools.findIndex((t) => t.toolCallId === update.toolCallId);
    if (idx >= 0) { it.tools[idx] = { ...it.tools[idx], ...update }; return; }
    if (update.sessionUpdate === "tool_call_update") continue;
  }
  const last = list[list.length - 1];
  if (last?.kind === "tools" && last.streaming && last.tools) { last.tools.push(update); return; }
  list.push({ id: nid(), kind: "tools", text: "", tools: [update], streaming: true });
}

function applyUpdate(chat: ChatState, update: any, turnId?: string) {
  const list = chat.items;
  switch (update?.sessionUpdate) {
    case "agent_message_chunk":
    case "agent_thought_chunk": {
      const t = textOf(update.content);
      if (!t) return;
      const kind = update.sessionUpdate === "agent_message_chunk" ? "assistant" : "thought";
      let last = list[list.length - 1];
      if (!last || last.kind !== kind || !last.streaming) {
        list.push({ id: nid(), kind, text: "", streaming: true });
        last = list[list.length - 1];
      }
      last.text += t;
      break;
    }
    case "tool_call":
    case "tool_call_update":
      upsertTool(list, update as ToolCall);
      break;
    case "plan":
      upsertTool(list, {
        sessionUpdate: "tool_call_update",
        toolCallId: `__plan__:${turnId ?? "x"}`,
        title: "计划",
        kind: "plan",
        status: "in_progress",
        rawOutput: update.entries ?? [],
      });
      break;
    default:
      break;
  }
}

function finishTurn(chat: ChatState) {
  chat.running = false;
  chat.turnId = null;
  for (const it of chat.items) it.streaming = false;
}

let resyncTimer: ReturnType<typeof setTimeout> | null = null;

export function handleEvent(name: string, p: any) {
  switch (name) {
    case "hello": {
      // （重新）连上：补拉错过的状态
      if (resyncTimer) clearTimeout(resyncTimer);
      resyncTimer = setTimeout(() => {
        void refreshPermissions();
        const ctx = ui.contextId, agent = ui.agent;
        if (ctx && agent && ui.chats[chatKey(ctx, agent)]) void refreshChatMeta(chatKey(ctx, agent), ctx, agent);
        ui.wfRev++;
      }, 300);
      break;
    }
    case "web://logout":
      ui.authed = false;
      break;
    case "acp://update": {
      if (!p?.update || p.source === "workflow" || !p.contextId) return;
      const chat = ui.chats[chatKey(p.contextId, p.agentType)];
      if (!chat || (chat.sessionId && p.sessionId && chat.sessionId !== p.sessionId)) return;
      if (!chat.running && p.turnId) { chat.running = true; chat.turnId = p.turnId; }
      applyUpdate(chat, p.update, p.turnId);
      break;
    }
    case "acp://binding-status": {
      if (p?.source === "workflow" || !p?.contextId) return;
      const chat = ui.chats[chatKey(p.contextId, p.agentType)];
      if (!chat) return;
      if (p.sessionId && !chat.sessionId) chat.sessionId = p.sessionId;
      if (chat.sessionId && p.sessionId && chat.sessionId !== p.sessionId) return;
      if (p.status === "running") { chat.running = true; chat.turnId = p.turnId ?? chat.turnId; }
      else finishTurn(chat);
      break;
    }
    case "acp://session-ready": {
      if (!p?.contextId) return;
      const key = chatKey(p.contextId, p.agentType);
      const chat = ui.chats[key];
      if (!chat || chat.sessionId === p.sessionId) return;
      if (!chat.sessionId && chat.running) { chat.sessionId = p.sessionId; return; }
      // 绑定换了（新建 / 绑定历史会话）：重新加载
      delete ui.chats[key];
      if (key === chatKey(ui.contextId, ui.agent)) void openChat();
      break;
    }
    case "chat://prompt": {
      if (!p?.contextId) return;
      const chat = ui.chats[chatKey(p.contextId, p.agentType)];
      if (!chat) return;
      if (p.requestId && chat.items.some((it) => it.requestId === p.requestId)) return;
      const extra = p.images ? `\n\n[图片 ×${p.images}]` : "";
      chat.items.push({ id: nid(), kind: "user", text: String(p.text ?? "") + extra, time: p.time ?? nowTime(), requestId: p.requestId ?? undefined });
      break;
    }
    case "web://prompt-done": {
      const chat = ui.chats[chatKey(p?.contextId, p?.agentType)];
      if (!chat) return;
      if (!p.ok) chat.items.push({ id: nid(), kind: "error", text: String(p.error ?? "发送失败") });
      if (!chat.turnId || chat.turnId === p.requestId) finishTurn(chat);
      break;
    }
    case "acp://permission":
      if (p?.requestId && !ui.permissions.some((x) => x.requestId === p.requestId)) ui.permissions.push(p);
      break;
    case "acp://requests-cancelled": {
      const ids = new Set<string>(p?.requestIds ?? []);
      ui.permissions = ui.permissions.filter((r) => !ids.has(r.requestId));
      break;
    }
    case "acp://status": {
      const a = ui.agents.find((x) => x.id === p?.agentType);
      if (a) a.status = p.state;
      break;
    }
    case "wf://status":
      if (!p?.workflowId) return;
      if (p.status === "running") { ui.wfRunning[p.workflowId] = p.runId; ui.wfLogs[p.runId] ??= ""; }
      else delete ui.wfRunning[p.workflowId];
      for (const w of ui.workflows) if (w.id === p.workflowId) w.running = p.status === "running" ? p.runId : null;
      break;
    case "wf://log": {
      if (!p?.runId) return;
      const cur = (ui.wfLogs[p.runId] ?? "") + p.line + "\n";
      ui.wfLogs[p.runId] = cur.length > 400_000 ? cur.slice(cur.length - 300_000) : cur;
      const keys = Object.keys(ui.wfLogs);
      if (keys.length > 12) for (const k of keys.slice(0, keys.length - 12)) if (!Object.values(ui.wfRunning).includes(k)) delete ui.wfLogs[k];
      break;
    }
    case "wf://deleted":
      ui.wfRev++;
      break;
  }
}
