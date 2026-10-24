// 全局助手前端状态：配置 / 对话 / 记忆，流式增量（acp://assistant-update）与气泡分页。
import { listen } from "@tauri-apps/api/event";
import { api } from "../ipc";
import { app, currentContext, currentProject, NO_PROJECT_ID, refreshWorkflows, toast } from "../state.svelte";
import { skinAssistant } from "../skins.svelte";
import { emitSkinEvent } from "../skin-fx/bus";
import { skinPersona, toWire, type ResolvedPersona } from "./personas";
import { t } from "../i18n";
import { beginSpeech, feedSpeech, initVoice, speechHint, stopSpeaking } from "./voice.svelte";
import type { AssistantConfigView, AssistantMemory, AssistantMessage, AssistantSubtitle, AssistantTurnResult, SpeechHint } from "../types";

type Busy = "" | "send" | "greet" | "care" | "wake" | "summarize" | "reset";

export const ga = $state({
  loaded: false,
  view: null as AssistantConfigView | null,
  messages: [] as AssistantMessage[],
  memories: [] as AssistantMemory[],
  /** 当前对话回合（只接收这个回合的流式增量） */
  turnId: "",
  busy: "" as Busy,
  /** 流式中的原始文本（含尚未剥离的指令）——正文层，进对话记录 */
  streaming: "",
  /** 本回合的字幕层（say 工具 / [[字幕: …]]）：气泡里显示、开启语音时朗读 */
  subs: [] as AssistantSubtitle[],
  /** 气泡：分页后的内容；visible=false 时只显示小图标；more = 字幕之外还有完整正文 */
  bubble: { visible: false, pages: [] as string[], index: 0, error: "", rev: 0, more: false },
  /** 展开的面板：null = 只有气泡；chat = 对话记录；memory = 记忆 */
  panel: null as null | "chat" | "memory",
  composer: false,
  /** 输入框聚焦请求（自增） */
  focusRev: 0,
});

export const enabled = () => !!ga.view?.config.enabled;

/** 连续语音对话（唤醒 / 主动关怀后）：说完自动接着听，助手判断何时结束 */
export const conv = $state({
  active: false,
  source: "" as "" | "wake" | "care",
  /** 已完成的轮数 */
  rounds: 0,
});

/** 发给后端的语音提示：连续对话中额外告诉模型「说完会继续听，由你判断何时收尾」 */
function hint(): SpeechHint | null {
  const h = speechHint();
  if (!conv.active) return h;
  return { ...(h ?? { enabled: false, lang: "", layer: false }), conversation: true };
}

/** 当前人设（皮肤角色或自定义） */
export function persona(): ResolvedPersona {
  const base = skinPersona({
    skin: app.skin,
    skinName: app.customSkins[app.skin],
    spec: skinAssistant.id === app.skin ? skinAssistant.spec : null,
    hasCharacter: !!app.skin && skinAssistant.id === app.skin && skinAssistant.character,
  });
  const cfg = ga.view?.config;
  if (cfg?.persona_mode === "custom" && (cfg.custom_name.trim() || cfg.custom_persona.trim())) {
    const name = cfg.custom_name.trim() || base.name;
    return { ...base, id: "custom", name, title: name, nick: undefined, persona: cfg.custom_persona.trim() || base.persona, greeting: "" };
  }
  return base;
}

function appState(): string | null {
  if (!ga.view?.config.share_app_state) return null;
  const parts: string[] = [];
  const p = currentProject();
  if (p) parts.push(`项目「${p.name}」(${p.root_path})`);
  const c = currentContext();
  if (c) parts.push(`共享上下文「${c.name}」`);
  const tabName: Record<string, string> = { chat: "聊天", context: "共享上下文", workflows: "工作流" };
  parts.push(`页面 ${tabName[app.tab] ?? app.tab}`);
  if (app.agent) parts.push(`编程 Agent ${app.agents.find((a) => a.id === app.agent)?.name ?? app.agent}`);
  if (app.skin) parts.push(`皮肤 ${app.customSkins[app.skin] ?? app.skin}`);
  return parts.join(" · ");
}

const newTurn = () => `ga-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

/** 去掉记忆指令（含流式中尚未闭合的半截） */
export function visibleText(raw: string): string {
  return raw
    .replace(SUB_STRIP, "")
    .replace(/^[ \t]*\[\[\s*(记忆|忘记|memory|forget)\s*[:：][^\]]*\]\][ \t]*$/gim, "")
    .replace(/\[\[\s*(记忆|忘记|memory|forget)\s*[:：][^\]]*\]\]/gi, "")
    .replace(/[ \t]*\[\[\s*(ja|en|ko|yue|zh|speech)\s*[:：][^\[\]]*\]\]/gi, "")
    .replace(/[ \t]*\[\[\s*(结束对话|结束|end[ _-]?(conversation|chat)?|跳过|skip)\s*\]\][ \t]*/gi, "")
    .replace(/\[\[[^\]]*$/, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

// ---------------- 字幕层 ----------------

// 字幕内容允许成对的单层方括号（如「看看 [README] 吧」），否则整条字幕会匹配失败、被跳过不读；与后端 sub_re 保持一致
const SUB_STRIP = /[ \t]*\[\[\s*(字幕|subtitle|say)\s*[:：](?:[^\[\]]|\[[^\[\]]*\])*\]\](?:[ \t]*\[\[\s*(ja|en|ko|yue|zh|speech)\s*[:：](?:[^\[\]]|\[[^\[\]]*\])*\]\])?[ \t]*\n?/gi;
const SUB_RE = /\[\[\s*(?:字幕|subtitle|say)\s*[:：]\s*((?:[^\[\]]|\[[^\[\]]*\])*?)\s*\]\](?:[ \t]*\[\[\s*(?:ja|en|ko|yue|zh|speech)\s*[:：]\s*((?:[^\[\]]|\[[^\[\]]*\])*?)\s*\]\])?/gi;

/** 从流式原文里取出已写完的 [[字幕: …]]；配音模式下要等紧跟的 [[ja: …]] 也写完 */
export function tagSubtitles(raw: string, final: boolean, layer: boolean): AssistantSubtitle[] {
  const out: AssistantSubtitle[] = [];
  for (const m of raw.matchAll(SUB_RE)) {
    const text = m[1].trim();
    if (!text) continue;
    if (layer && !final && m[2] === undefined) {
      const rest = raw.slice((m.index ?? 0) + m[0].length);
      if (/^\s*(\[(\[[^\]]*)?)?$/.test(rest)) break;
    }
    out.push({ text, speech: (m[2] ?? "").trim() });
  }
  return out;
}

/** 本回合的配音设置（回合开始时取定） */
let turnSpeech: { lang: string; layer: boolean } = { lang: "", layer: false };
/** say 工具发来的字幕 */
let toolSubs: AssistantSubtitle[] = [];
/** 已并入 ga.subs 的工具字幕 / 标记字幕条数：字幕只追加不重排，朗读按偏移续读才不会漏句 */
let toolSeen = 0;
let tagSeen = 0;

function resetSubs() {
  ga.subs = [];
  toolSubs = [];
  toolSeen = 0;
  tagSeen = 0;
}

/** 把新到的工具字幕与标记字幕按到达顺序追加到 ga.subs 末尾 */
function appendSubs(final: boolean): boolean {
  const tags = tagSubtitles(ga.streaming, final, turnSpeech.layer);
  const add = [...toolSubs.slice(toolSeen), ...tags.slice(tagSeen)];
  toolSeen = toolSubs.length;
  tagSeen = Math.max(tagSeen, tags.length);
  if (!add.length) return false;
  ga.subs = [...ga.subs, ...add];
  return true;
}

/** 字幕 → 朗读原文：配音模式每条附 [[lang: …]]，没有配音版本的不读 */
function subSpeech(subs: AssistantSubtitle[]): string {
  const { lang, layer } = turnSpeech;
  return subs.map((s) => (layer ? `${s.text}${s.speech ? ` [[${lang}: ${s.speech}]]` : ""}\n` : `${s.text}\n`)).join("");
}

export const subText = (subs: AssistantSubtitle[]) => subs.map((s) => s.text).join("\n");

/** 合并工具字幕与标记字幕；有新字幕就送去朗读 */
function syncSubs() {
  if (appendSubs(false)) feedSpeech(subSpeech(ga.subs), false);
}

/** 回合结束：补上最后的标记字幕；一条都没收到时改用后端给的（推导的），然后把朗读收尾 */
function finishSubs(fallback: AssistantSubtitle[]) {
  appendSubs(true);
  if (!ga.subs.length && fallback.length) ga.subs = fallback;
  feedSpeech(subSpeech(ga.subs), true);
}

/** 把一条回复切成气泡页：按段落，超长段按句子再切；代码块整段保留 */
export function paginate(text: string, max = 110): string[] {
  const src = text.trim();
  if (!src) return [];
  if (src.length <= max * 1.4 && !/\n\s*\n/.test(src)) return [src];
  const blocks: string[] = [];
  const fence = /```[\s\S]*?```/g;
  let last = 0;
  for (const m of src.matchAll(fence)) {
    blocks.push(...src.slice(last, m.index).split(/\n\s*\n/));
    blocks.push(m[0]);
    last = m.index! + m[0].length;
  }
  blocks.push(...src.slice(last).split(/\n\s*\n/));
  const pages: string[] = [];
  let cur = "";
  const flush = () => { if (cur.trim()) pages.push(cur.trim()); cur = ""; };
  for (const raw of blocks) {
    const b = raw.trim();
    if (!b) continue;
    // 代码块 / 列表 / 表格：整块一页
    if (b.startsWith("```") || /^(\s*([-*+]|\d+\.)\s|\|)/m.test(b)) {
      flush();
      pages.push(b);
      continue;
    }
    const sentences = b.length > max ? b.match(/[^。！？!?…\n]+[。！？!?…]*[”」』）)]*\s*|\n/g) ?? [b] : [b];
    for (const s of sentences) {
      if (cur && (cur + s).length > max) flush();
      cur += s;
    }
    flush();
  }
  // 太碎的尾页并到前一页
  for (let i = pages.length - 1; i > 0; i--) {
    if (pages[i].length < 18 && !pages[i].startsWith("```") && pages[i - 1].length + pages[i].length <= max * 1.3) {
      pages[i - 1] = `${pages[i - 1]}${/[\n]$/.test(pages[i - 1]) ? "" : " "}${pages[i]}`;
      pages.splice(i, 1);
    }
  }
  return pages;
}

export function showBubble(text: string, error = "", more = false) {
  ga.bubble = { visible: true, pages: text ? paginate(text) : [], index: 0, error, rev: ga.bubble.rev + 1, more };
}
const blankBubble = () => ({ visible: true, pages: [] as string[], index: 0, error: "", rev: ga.bubble.rev + 1, more: false });
export function hideBubble() {
  ga.bubble.visible = false;
}

let unlisten: (() => void) | null = null;
/** 当前回合是否安静开始（主动关怀：有正文了才弹气泡），以及气泡是否已为流式打开 */
let quietTurn = false;
let opened = false;

export async function initAssistant() {
  if (!unlisten) {
    unlisten = await listen<any>("acp://assistant-update", (e) => {
      const p = e.payload;
      if (!p || !ga.turnId || p.turnId !== ga.turnId || ga.busy === "summarize") return;
      const u = p.update;
      if (u?.sessionUpdate === "assistant_subtitle" && typeof u.text === "string" && u.text.trim()) {
        toolSubs = [...toolSubs, { text: u.text.trim(), speech: typeof u.speech === "string" ? u.speech.trim() : "" }];
      } else if (u?.sessionUpdate === "agent_message_chunk" && u.content?.type === "text" && typeof u.content.text === "string") {
        ga.streaming += u.content.text;
      } else return;
      syncSubs();
      if (!opened && (!quietTurn || ga.subs.length || visibleText(ga.streaming))) {
        opened = true;
        emitSkinEvent("agent.streaming");
        if (!ga.bubble.visible || ga.bubble.pages.length) ga.bubble = blankBubble();
      }
    });
  }
  if (!unlistenWake) {
    unlistenWake = await listen<any>("assistant://wake", (e) => {
      const id = e.payload?.reqId;
      if (typeof id !== "string" || !id) return;
      wakeQueue.push(id);
      void drainWakes();
    });
    await listen<any>("assistant://schedules", (e) => onSchedules(e.payload));
  }
  await reload();
  await initVoice();
  if (enabled() && needsNewSession()) {
    // 等皮肤装载完再问候，人设才是左下角的角色
    await waitUntil(() => app.ready, 8000);
    if (app.skin) await waitUntil(() => skinAssistant.id === app.skin, 4000);
    if (enabled() && needsNewSession() && !ga.busy) void greet("new");
  }
}

// ---------------- 唤醒（工作流「全局助手」节点 / 助手自己设的定时） ----------------

let unlistenWake: (() => void) | null = null;
const wakeQueue: string[] = [];
let draining = false;

/** 逐个处理唤醒请求：等助手空闲再跑（最多等 5 分钟），跑不了就告诉后端，工作流节点据此失败 */
async function drainWakes() {
  if (draining) return;
  draining = true;
  try {
    while (wakeQueue.length) {
      const id = wakeQueue.shift()!;
      if (!ga.loaded) await waitUntil(() => ga.loaded, 10_000);
      if (!enabled()) { void api.assistantWakeFail(id, t("全局助手未启用")).catch(() => {}); continue; }
      await waitUntil(() => !ga.busy, 5 * 60_000);
      if (ga.busy) { void api.assistantWakeFail(id, t("助手一直在忙，这次没能处理")).catch(() => {}); continue; }
      let called = false;
      await runTurn("wake", (turnId) => {
        called = true;
        return api.assistantWake(id, toWire(persona(), app.locale), appState(), turnId, hint());
      }, true);
      if (!called) void api.assistantWakeFail(id, t("助手一直在忙，这次没能处理")).catch(() => {});
    }
  } finally {
    draining = false;
  }
}

function waitUntil(ok: () => boolean, ms: number): Promise<void> {
  return new Promise((resolve) => {
    const end = Date.now() + ms;
    const tickOnce = () => (ok() || Date.now() > end ? resolve() : setTimeout(tickOnce, 120));
    tickOnce();
  });
}

export async function reload() {
  try {
    ga.view = await api.assistantConfigGet();
    if (ga.view.config.enabled) ga.messages = await api.assistantMessages(200);
  } catch (e) {
    console.warn("[assistant]", e);
  } finally {
    ga.loaded = true;
  }
}

export async function loadMemories() {
  ga.memories = await api.assistantMemories().catch(() => ga.memories);
}

/** 当前配置会不会落到一个新的长期会话（需要问候） */
export function needsNewSession(view = ga.view): boolean {
  if (!view?.config.enabled) return false;
  const c = view.config;
  if (!view.session_id || !view.session_greeted) return true;
  if (view.session_provider && view.session_provider !== c.provider) return true;
  return c.provider === "acp" && !!view.session_agent && view.session_agent !== c.agent_type;
}

/** 设置保存后调用：首次启用 / 换了客户端 → 新会话问候 */
export function afterConfigSaved(view: AssistantConfigView) {
  const fresh = needsNewSession(view);
  ga.view = view;
  if (!view.config.enabled) {
    ga.composer = false;
    ga.panel = null;
    hideBubble();
    return;
  }
  void api.assistantMessages(200).then((m) => (ga.messages = m)).catch(() => {});
  if (fresh && !ga.busy) void greet("new");
}

async function runTurn(kind: Busy, call: (turnId: string) => Promise<AssistantTurnResult>, quiet = false): Promise<AssistantTurnResult | null> {
  if (ga.busy) { if (!quiet) toast("warn", t("助手正在回复中，请稍后")); return null; }
  const turnId = newTurn();
  ga.turnId = turnId;
  ga.busy = kind;
  ga.streaming = "";
  resetSubs();
  const sh = speechHint();
  turnSpeech = { lang: sh?.lang ?? "", layer: !!sh?.layer };
  quietTurn = quiet;
  opened = false;
  if (!quiet) ga.bubble = blankBubble();
  beginSpeech();
  try {
    const r = await call(turnId);
    if (ga.turnId !== turnId) return null;
    if (r.skipped) {
      stopSpeaking();
      if (opened) hideBubble();
      return r;
    }
    // 字幕以流式收到的为准（已在朗读）；一条都没收到时用后端解析 / 推导的
    finishSubs(r.subtitles ?? []);
    if (r.message) {
      ga.messages = [...ga.messages, r.message].slice(-400);
      if (ga.subs.length) showBubble(subText(ga.subs), "", !!r.has_body);
      else showBubble(r.message.content);
    } else {
      showBubble("", t("（没有回复内容）"));
    }
    const bits = [];
    if (r.memories_added || r.memories_forgotten) {
      void loadMemories();
      if (r.memories_added) bits.push(t("记住了 {n} 条", { n: r.memories_added }));
      if (r.memories_forgotten) bits.push(t("忘掉了 {n} 条", { n: r.memories_forgotten }));
    }
    if (bits.length) {
      memoNote.text = bits.join(" · ");
      memoNote.rev++;
    }
    emitSkinEvent("agent.done");
    return r;
  } catch (e) {
    if (ga.turnId !== turnId) return null;
    if (quiet) {
      // 主动关怀失败不打扰用户
      stopSpeaking();
      if (opened) hideBubble();
      console.warn("[assistant care]", e);
      return null;
    }
    if (String(e).includes("已停止") || String(e).toLowerCase().includes("cancel")) stopSpeaking();
    else finishSubs([]);
    const body = visibleText(ga.streaming);
    const partial = ga.subs.length ? subText(ga.subs) : body;
    showBubble(partial, String(e), !!ga.subs.length && !!body);
    emitSkinEvent("agent.error");
    return null;
  } finally {
    if (ga.turnId === turnId) {
      ga.busy = "";
      ga.streaming = "";
      resetSubs();
      quietTurn = false;
      void refreshView();
    }
  }
}

/** 助手用 MCP 工具设 / 取消了定时：小提示 + 刷新工作流列表（定时放在「无项目」下） */
function onSchedules(p: any) {
  const r = p?.result ?? {};
  let text = "";
  if (p?.tool === "assistant_schedule_create" && !r.existed) text = t("设了个定时：{when}", { when: String(r.when ?? "") });
  else if (p?.tool === "assistant_schedule_cancel" && r.cancelled) text = t("取消了 {n} 个定时", { n: r.cancelled });
  if (text) {
    memoNote.text = text;
    memoNote.rev++;
  }
  if (app.projectId === NO_PROJECT_ID) void refreshWorkflows().catch(() => {});
}

/** 记忆变动的小提示（气泡底部短暂显示） */
export const memoNote = $state({ text: "", rev: 0 });

async function refreshView() {
  ga.view = await api.assistantConfigGet().catch(() => ga.view);
}

export async function send(text: string): Promise<AssistantTurnResult | null> {
  const body = text.trim();
  if (!body || !enabled()) return null;
  const pending: AssistantMessage = { id: `local-${Date.now()}`, role: "user", content: body, provider: ga.view!.config.provider, created_at: new Date().toISOString(), interrupted: false, greeting: false };
  emitSkinEvent("message.send");
  const r = await runTurn("send", async (turnId) => {
    ga.messages = [...ga.messages, pending];
    return api.assistantSend(body, toWire(persona(), app.locale), appState(), turnId, hint());
  });
  // 用库里的记录替换本地占位（拿到真实 id）
  if (!ga.busy) ga.messages = await api.assistantMessages(200).catch(() => ga.messages);
  return r;
}

/** 主动关怀：安静地发起一轮（有正文才弹气泡；助手可以回 [[跳过]] 选择不打扰） */
export async function care(context: string): Promise<AssistantTurnResult | null> {
  if (!enabled() || ga.busy) return null;
  return runTurn("care", (turnId) => api.assistantCare(toWire(persona(), app.locale), appState(), turnId, context, hint()), true);
}

export async function greet(reason: "new" | "resume") {
  if (!enabled()) return;
  await runTurn("greet", (turnId) => api.assistantGreet(toWire(persona(), app.locale), appState(), turnId, reason, hint()));
}

export async function cancel() {
  const id = ga.turnId;
  stopSpeaking();
  if (!id || !ga.busy) return;
  await api.assistantCancel(id).catch((e) => toast("error", String(e)));
}

export async function resetSession() {
  if (ga.busy) { toast("warn", t("助手正在回复中，请先停止")); return; }
  ga.busy = "reset";
  ga.bubble = blankBubble();
  try {
    ga.view = await api.assistantSessionReset();
  } catch (e) {
    ga.busy = "";
    showBubble("", String(e));
    return;
  }
  ga.busy = "";
  await greet("new");
}

export async function clearMessages() {
  await api.assistantMessagesClear();
  ga.messages = [];
}

export async function summarize(mode: "consolidate" | "extract"): Promise<number | null> {
  if (ga.busy) { toast("warn", t("助手正在回复中，请稍后")); return null; }
  const turnId = newTurn();
  ga.turnId = turnId;
  ga.busy = "summarize";
  try {
    const n = await api.assistantMemorySummarize(mode, toWire(persona(), app.locale), turnId);
    await loadMemories();
    return n;
  } catch (e) {
    toast("error", String(e));
    return null;
  } finally {
    if (ga.turnId === turnId) ga.busy = "";
  }
}

/** 已发出、还没收到任何文字 */
export function isThinking() {
  return (ga.busy === "send" || ga.busy === "greet" || ga.busy === "care" || ga.busy === "wake" || ga.busy === "reset") && !ga.streaming && !ga.subs.length;
}

/** 点击人物 / 语音唤起：打开输入框 */
export function openComposer() {
  if (!enabled()) return;
  ga.composer = true;
  ga.focusRev++;
}

/** 请求设置窗口打开到某个页签（助手卡片里的「设置」按钮用） */
export const settingsRequest = $state({ tab: "" });
