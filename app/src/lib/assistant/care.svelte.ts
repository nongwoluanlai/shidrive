// 主动关怀：用户不忙时，低频地让助手主动搭句话（深夜、连续忙太久、饭点、待办、今天过得怎样…）。
// 只挑一个由头交给模型，用人设口吻自然地说一两句；模型觉得会打扰可以回 [[跳过]]。
import { app } from "../state.svelte";
import { care, conv, enabled, ga } from "./store.svelte";
import { inputEnabled, vc } from "./voice.svelte";
import { startConversation } from "./conv.svelte";

const KEY = "shidrive.assistant.care";
/** 每天最多主动搭话次数 */
const MAX_PER_DAY = 6;
/** 离开超过这么久算一次新的「连续使用」 */
const AWAY_MS = 20 * 60_000;

type Mem = { last: number; day: string; count: number; done: Record<string, string> };

function loadMem(): Mem {
  try {
    const m = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return { last: Number(m.last) || 0, day: String(m.day ?? ""), count: Number(m.count) || 0, done: m.done && typeof m.done === "object" ? m.done : {} };
  } catch {
    return { last: 0, day: "", count: 0, done: {} };
  }
}
const saveMem = (m: Mem) => { try { localStorage.setItem(KEY, JSON.stringify(m)); } catch { /* ignore */ } };

const pad = (n: number) => String(n).padStart(2, "0");
const dayKey = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
const hhmm = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;
const WEEK = ["日", "一", "二", "三", "四", "五", "六"];

export function duration(min: number): string {
  if (min < 60) return `${Math.max(1, Math.round(min))} 分钟`;
  const h = Math.floor(min / 60), m = Math.round(min % 60);
  return m >= 5 ? `${h} 小时 ${m} 分钟` : `${h} 小时`;
}

// ---------------- 用户活动 ----------------
const started = Date.now();
let lastInput = Date.now();
let lastKey = 0;
let activeSince = Date.now();

function onActivity(e: Event) {
  const now = Date.now();
  if (now - lastInput > AWAY_MS) activeSince = now;
  lastInput = now;
  if (e.type === "keydown") lastKey = now;
}

type Pick = { key: string; stamp: string; line: string };

/** 按优先级挑这次的由头；ignoreDone = 试一下（不看今天是否已经说过） */
export function pickReason(now: Date, opts: { ignoreDone?: boolean; mem?: Mem; activeMin?: number; lateHour?: number; intervalMin?: number } = {}): Pick | null {
  const mem = opts.mem ?? loadMem();
  const done = (k: string, stamp: string) => !opts.ignoreDone && mem.done[k] === stamp;
  const h = now.getHours();
  const hm = h * 60 + now.getMinutes();
  const today = dayKey(now);
  const activeMin = opts.activeMin ?? (Date.now() - activeSince) / 60_000;
  const late = opts.lateHour ?? ga.view?.config.care_late_hour ?? 23;
  const interval = opts.intervalMin ?? ga.view?.config.care_interval_minutes ?? 120;
  const pending = app.todos.filter((x) => !x.done);
  // 深夜：每晚一次（凌晨算前一天晚上）
  if (h >= late || h < 5) {
    const night = h < 5 ? dayKey(new Date(now.getTime() - 6 * 3600_000)) : today;
    if (!done("late", night)) return { key: "late", stamp: night, line: `已经很晚了（${hhmm(now)}），用户还在电脑前` };
  }
  // 饭点：连续忙了一阵才提
  const meal = hm >= 11 * 60 + 40 && hm <= 13 * 60 ? "lunch" : hm >= 17 * 60 + 50 && hm <= 19 * 60 + 30 ? "dinner" : "";
  if (meal && (activeMin >= 45 || opts.ignoreDone) && !done(meal, today)) {
    return { key: meal, stamp: today, line: `到${meal === "lunch" ? "午饭" : "晚饭"}时间了（${hhmm(now)}），用户已经连续忙了约 ${duration(activeMin)}` };
  }
  // 连续用了很久：每满 2 小时最多一次
  if (activeMin >= 120) {
    const stamp = `${activeSince}-${Math.floor(activeMin / 120)}`;
    if (!done("long", stamp)) return { key: "long", stamp, line: `用户已经连续用了约 ${duration(activeMin)}，几乎没停下来过` };
  }
  // 待办：每 4 小时最多一次
  if (pending.length) {
    const stamp = `${today}-${Math.floor(h / 4)}`;
    if (!done("todo", stamp)) {
      const list = pending.slice(0, 3).map((x) => `「${x.text.replace(/\s+/g, " ").slice(0, 40)}」`).join("");
      return { key: "todo", stamp, line: `用户的待办里还有 ${pending.length} 件没完成：${list}${pending.length > 3 ? "等" : ""}` };
    }
  }
  // 晚上：问问今天过得怎样
  if (hm >= 20 * 60 && hm <= 22 * 60 + 30 && !done("recap", today)) {
    return { key: "recap", stamp: today, line: "晚上了，今天还没问过用户今天过得怎么样、发生了什么" };
  }
  // 没什么特别的事：间隔够久才随口聊两句
  if (opts.ignoreDone || Date.now() - mem.last >= interval * 2 * 60_000) {
    return { key: "chat", stamp: String(Date.now()), line: "没什么特别的事，就是想随口和用户聊两句（可以接着之前聊过的话题或记忆里的事）" };
  }
  return null;
}

/** 交给模型的情况说明 */
function contextFor(pick: Pick, now: Date): string {
  const lines = [`- 这次的由头：${pick.line}`, `- 现在：${dayKey(now)} ${hhmm(now)} 周${WEEK[now.getDay()]}`];
  const activeMin = (Date.now() - activeSince) / 60_000;
  if (activeMin >= 20 && pick.key !== "long") lines.push(`- 用户这次已连续使用约 ${duration(activeMin)}`);
  const pending = app.todos.filter((x) => !x.done);
  if (pending.length && pick.key !== "todo") lines.push(`- 待办里还有 ${pending.length} 件没完成（不一定要提）`);
  const last = [...ga.messages].reverse().find((m) => !m.greeting);
  if (last) {
    const ago = (Date.now() - new Date(last.created_at).getTime()) / 60_000;
    if (Number.isFinite(ago)) lines.push(`- 上次和你聊天是 ${ago < 60 * 24 ? `${duration(ago)}前` : "一天多以前"}`);
  }
  return lines.join("\n");
}

/** 现在适合主动搭话吗（不忙、人在、没在打字、没在聊） */
function quietNow(): boolean {
  const now = Date.now();
  if (document.visibilityState !== "visible" || app.settingsOpen) return false;
  if (ga.busy || conv.active || vc.recording || vc.transcribing || vc.speaking || vc.voiceTurn || ga.bubble.visible) return false;
  if (now - lastInput > 10 * 60_000) return false; // 人不在
  if (now - lastKey < 60_000) return false; // 正在打字
  const el = document.activeElement as HTMLElement | null;
  if (el && (el.tagName === "TEXTAREA" || el.tagName === "INPUT") && (el as HTMLInputElement).value?.trim()) return false;
  const last = ga.messages[ga.messages.length - 1];
  if (last && now - new Date(last.created_at).getTime() < 20 * 60_000) return false; // 刚聊过
  return true;
}

async function fire(pick: Pick, count: boolean): Promise<"sent" | "skipped" | "failed"> {
  if (count) {
    const mem = loadMem();
    const today = dayKey(new Date());
    if (mem.day !== today) { mem.day = today; mem.count = 0; }
    mem.last = Date.now();
    mem.count++;
    mem.done[pick.key] = pick.stamp;
    saveMem(mem);
  }
  const r = await care(contextFor(pick, new Date()));
  if (!r) return "failed";
  if (r.skipped || !r.message) return "skipped";
  const cfg = ga.view?.config;
  if (cfg?.care_voice && inputEnabled() && !r.end_conversation) void startConversation("care");
  return "sent";
}

async function tick() {
  const cfg = ga.view?.config;
  if (!cfg?.care_enabled || !enabled()) return;
  if (Date.now() - started < 10 * 60_000) return; // 刚打开有问候，别紧接着又搭话
  if (!quietNow()) return;
  const mem = loadMem();
  const now = new Date();
  if (mem.day === dayKey(now) && mem.count >= MAX_PER_DAY) return;
  const pick = pickReason(now, { mem });
  if (!pick) return;
  // 深夜提醒间隔放宽到 ≤1 小时，其余按设置的间隔
  const gap = (pick.key === "late" ? Math.min(60, cfg.care_interval_minutes) : cfg.care_interval_minutes) * 60_000;
  if (Date.now() - mem.last < gap) return;
  await fire(pick, true);
}

/** 设置页「现在试一下」：忽略间隔与今天是否说过 */
export async function tryCare(): Promise<"sent" | "skipped" | "failed" | "busy"> {
  if (!enabled() || ga.busy || conv.active) return "busy";
  const pick = pickReason(new Date(), { ignoreDone: true });
  return pick ? fire(pick, false) : "skipped";
}

let timer: ReturnType<typeof setInterval> | null = null;
export function initCare(): () => void {
  if (timer) return () => {};
  const evs = ["pointerdown", "keydown", "wheel"] as const;
  for (const e of evs) addEventListener(e, onActivity, { capture: true, passive: true });
  timer = setInterval(() => void tick(), 60_000);
  return () => {
    for (const e of evs) removeEventListener(e, onActivity, { capture: true });
    if (timer) clearInterval(timer);
    timer = null;
  };
}
