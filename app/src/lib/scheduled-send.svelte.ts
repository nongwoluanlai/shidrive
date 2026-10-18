// 定时发送：在聊天输入框里选一个时间，到点自动把这段文字发给对应会话的 Agent。
// - 队列存 localStorage（重启后仍在）；1 秒一次检查（Worker 计时，窗口最小化时也不被节流）。
// - 到点时若该会话正在回答/加载，等它空闲再发（不打断当前回合）。
// - 应用关闭期间错过的：启动时标记为「已错过」并记入全局日志，不自动补发，可在列表里手动「立即发送」。
// - 目标会话未加载（太久没看过）时先切换过去加载历史再发，避免用不完整的缓存覆盖本地记录。
import { app, chatKey, finishTurn, gotoSession, nextId, pushLocal, toast } from "./state.svelte";
import { api } from "./ipc";
import { t } from "./i18n";
import { logGlobal } from "./global-log.svelte";
import type { AgentType, Context } from "./types";

export interface ScheduledSend {
  id: string;
  contextId: string;
  projectId: string | null;
  contextName: string;
  agent: AgentType;
  text: string;
  /** 计划发送时间（epoch ms） */
  at: number;
  status: "pending" | "waiting" | "missed" | "failed";
  note?: string;
}

const KEY = "shidrive.scheduledSends";
const MISSED_GRACE_MS = 60_000;

function load(): ScheduledSend[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || "[]");
    return Array.isArray(v) ? v.filter((x) => x && typeof x.id === "string" && typeof x.at === "number") : [];
  } catch {
    return [];
  }
}

export const sched = $state<{ items: ScheduledSend[] }>({ items: load() });

function save() {
  try {
    localStorage.setItem(KEY, JSON.stringify(sched.items));
  } catch {
    /* ignore */
  }
}

export function fmtAt(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

const excerpt = (s: string) => {
  const x = s.replace(/\s+/g, " ").trim();
  return x.length > 40 ? x.slice(0, 40) + "…" : x;
};

export function scheduledFor(contextId: string | null, agent: string): ScheduledSend[] {
  return sched.items.filter((s) => s.contextId === contextId && s.agent === agent).sort((a, b) => a.at - b.at);
}

export function addScheduledSend(ctx: Context, agent: AgentType, text: string, at: number): ScheduledSend {
  const item: ScheduledSend = {
    id: `ss-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`,
    contextId: ctx.id,
    projectId: ctx.project_id ?? null,
    contextName: ctx.name,
    agent,
    text,
    at,
    status: "pending",
  };
  sched.items.push(item);
  save();
  logGlobal("info", "chat", t("已安排定时发送：{at} →「{ctx}」({agent})：{text}", { at: fmtAt(at), ctx: ctx.name, agent, text: excerpt(text) }));
  return item;
}

export function cancelScheduledSend(id: string) {
  const i = sched.items.findIndex((s) => s.id === id);
  if (i < 0) return;
  const [s] = sched.items.splice(i, 1);
  save();
  logGlobal("info", "chat", t("已取消定时发送（原定 {at}）：{text}", { at: fmtAt(s.at), text: excerpt(s.text) }));
}

/** 立即发送（列表里的「立即发送」或重发已错过的项） */
export function sendScheduledNow(id: string) {
  const s = sched.items.find((x) => x.id === id);
  if (!s) return;
  s.at = Date.now();
  s.status = "pending";
  s.note = undefined;
  save();
  void tick();
}

async function findContext(s: ScheduledSend): Promise<Context | null> {
  const local = app.contexts.find((c) => c.id === s.contextId);
  if (local) return local;
  const all = await api.contextsAll().catch(() => [] as [Context, string][]);
  return all.find(([c]) => c.id === s.contextId)?.[0] ?? null;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const busy = (key: string) => !!(app.streaming[key] || app.chatLoading[key] || app.sessionBusy[key]);

/** 切到目标会话并等待其历史加载完毕（最多 30 秒） */
async function loadTarget(ctx: Context, s: ScheduledSend, key: string): Promise<boolean> {
  await gotoSession(ctx.project_id ?? s.projectId, ctx.id, s.agent);
  await sleep(400);
  const until = Date.now() + 30_000;
  let calm = 0;
  while (Date.now() < until) {
    if (app.contextId !== ctx.id || app.agent !== s.agent) return false; // 用户又切走了：下一轮再试
    calm = busy(key) ? 0 : calm + 1;
    if (calm >= 3) return true;
    await sleep(200);
  }
  return false;
}

let firing = false;
let navigating = false;
const inflight = new Set<string>(); // 正在处理的会话 key（含切换加载等待），防止重复触发
async function fire(s: ScheduledSend) {
  const key = chatKey(s.contextId, s.agent);
  if (busy(key)) {
    if (s.status !== "waiting") {
      s.status = "waiting";
      s.note = t("等待当前回合结束");
      save();
    }
    return;
  }
  const ctx = await findContext(s);
  if (!ctx) {
    s.status = "failed";
    s.note = t("上下文已不存在");
    save();
    logGlobal("error", "chat", t("定时发送失败：上下文「{ctx}」已不存在", { ctx: s.contextName }));
    return;
  }
  if (!app.chat[key]) {
    if (navigating) return; // 另一条正在切换加载：下一轮再试
    navigating = true;
    try {
      if (!(await loadTarget(ctx, s, key))) return;
    } finally {
      navigating = false;
    }
    if (busy(key)) return;
  }
  // 发送前从队列移除，避免重复发送
  const i = sched.items.findIndex((x) => x.id === s.id);
  if (i < 0) return;
  sched.items.splice(i, 1);
  save();
  const late = Date.now() - s.at;
  logGlobal(
    "success",
    "chat",
    t("定时发送 →「{ctx}」({agent})：{text}", { ctx: ctx.name, agent: s.agent, text: excerpt(s.text) }) +
      (late > MISSED_GRACE_MS ? t("（晚于计划 {m} 分钟）", { m: Math.round(late / 60_000) }) : ""),
  );
  if (key !== chatKey(app.contextId, app.agent)) toast("info", t("已定时发送到「{ctx}」", { ctx: ctx.name }));
  // 与 ChatView.send() 相同的回合流程
  const requestId = nextId();
  app.promptRequests[key] = requestId;
  app.pendingMessage[key] = pushLocal(key, { kind: "user", text: s.text });
  app.streaming[key] = true;
  try {
    await api.acpPrompt({ ...ctx }, s.agent, s.text, [], requestId);
  } catch (e) {
    if (app.promptRequests[key] === requestId) pushLocal(key, { kind: "error", text: String(e) });
    logGlobal("error", "chat", t("定时发送失败：{err}", { err: String(e) }));
  } finally {
    if (app.activeTurn[key]?.turnId !== requestId) finishTurn(s.agent, ctx.id, requestId);
  }
}

async function tick() {
  if (firing) return;
  const now = Date.now();
  const due = sched.items.filter((s) => (s.status === "pending" || s.status === "waiting") && s.at <= now).sort((a, b) => a.at - b.at);
  if (!due.length) return;
  firing = true;
  try {
    // 同一会话一次只发一条（发出后该会话进入回答中，下一条自然等待）
    const seen = new Set<string>();
    for (const s of due) {
      const k = chatKey(s.contextId, s.agent);
      if (seen.has(k) || inflight.has(k)) continue;
      seen.add(k);
      inflight.add(k);
      void fire(s)
        .catch((e) => console.warn("scheduled send", e))
        .finally(() => inflight.delete(k));
      await sleep(0);
    }
  } finally {
    firing = false;
  }
}

let started = false;
export function startScheduledSends() {
  if (started) return;
  started = true;
  // 启动时：关闭期间错过的标记为「已错过」
  const now = Date.now();
  let changed = false;
  for (const s of sched.items) {
    if ((s.status === "pending" || s.status === "waiting") && s.at < now - MISSED_GRACE_MS) {
      s.status = "missed";
      s.note = t("应用未运行，错过计划时间");
      changed = true;
      logGlobal("warn", "chat", t("错过定时发送（计划 {at}，应用未运行）→「{ctx}」：{text}", { at: fmtAt(s.at), ctx: s.contextName, text: excerpt(s.text) }));
    }
  }
  if (changed) save();
  setInterval(() => void tick(), 1000);
  // Worker 计时不受后台节流影响；CSP 不允许时退回上面的 setInterval
  try {
    const src = "setInterval(()=>postMessage(0),1000)";
    const w = new Worker(URL.createObjectURL(new Blob([src], { type: "text/javascript" })));
    w.onmessage = () => void tick();
    w.onerror = () => w.terminate();
  } catch {
    /* setInterval fallback */
  }
}
