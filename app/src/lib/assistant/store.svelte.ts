// 全局助手前端状态：配置 / 对话 / 记忆，流式增量（acp://assistant-update）与气泡分页。
import { listen } from "@tauri-apps/api/event";
import { api } from "../ipc";
import { app, currentContext, currentProject, toast } from "../state.svelte";
import { skinAssistant } from "../skins.svelte";
import { emitSkinEvent } from "../skin-fx/bus";
import { skinPersona, toWire, type ResolvedPersona } from "./personas";
import { t } from "../i18n";
import { beginSpeech, feedSpeech, initVoice, speechHint, stopSpeaking } from "./voice.svelte";
import type { AssistantConfigView, AssistantMemory, AssistantMessage, AssistantTurnResult } from "../types";

type Busy = "" | "send" | "greet" | "summarize" | "reset";

export const ga = $state({
  loaded: false,
  view: null as AssistantConfigView | null,
  messages: [] as AssistantMessage[],
  memories: [] as AssistantMemory[],
  /** 当前对话回合（只接收这个回合的流式增量） */
  turnId: "",
  busy: "" as Busy,
  /** 流式中的原始文本（含尚未剥离的指令） */
  streaming: "",
  /** 气泡：分页后的内容；visible=false 时只显示小图标 */
  bubble: { visible: false, pages: [] as string[], index: 0, error: "", rev: 0 },
  /** 展开的面板：null = 只有气泡；chat = 对话记录；memory = 记忆 */
  panel: null as null | "chat" | "memory",
  composer: false,
  /** 输入框聚焦请求（自增） */
  focusRev: 0,
});

export const enabled = () => !!ga.view?.config.enabled;

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
    return { ...base, id: "custom", name, title: base.style === "amadeus" ? `AMADEUS · ${name}` : name, persona: cfg.custom_persona.trim() || base.persona, greeting: "" };
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
    .replace(/^[ \t]*\[\[\s*(记忆|忘记|memory|forget)\s*[:：][^\]]*\]\][ \t]*$/gim, "")
    .replace(/\[\[\s*(记忆|忘记|memory|forget)\s*[:：][^\]]*\]\]/gi, "")
    .replace(/[ \t]*\[\[\s*(ja|en|ko|yue|zh)\s*[:：][^\[\]]*\]\]/gi, "")
    .replace(/\[\[[^\]]*$/, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
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

export function showBubble(text: string, error = "") {
  ga.bubble = { visible: true, pages: text ? paginate(text) : [], index: 0, error, rev: ga.bubble.rev + 1 };
}
export function hideBubble() {
  ga.bubble.visible = false;
}

let unlisten: (() => void) | null = null;

export async function initAssistant() {
  if (!unlisten) {
    unlisten = await listen<any>("acp://assistant-update", (e) => {
      const p = e.payload;
      if (!p || !ga.turnId || p.turnId !== ga.turnId || ga.busy === "summarize") return;
      const u = p.update;
      if (u?.sessionUpdate !== "agent_message_chunk" || u.content?.type !== "text" || typeof u.content.text !== "string") return;
      const first = !ga.streaming;
      ga.streaming += u.content.text;
      feedSpeech(ga.streaming, false);
      if (first) {
        emitSkinEvent("agent.streaming");
        if (!ga.bubble.visible || ga.bubble.pages.length) ga.bubble = { visible: true, pages: [], index: 0, error: "", rev: ga.bubble.rev + 1 };
      }
    });
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

async function runTurn(kind: Busy, call: (turnId: string) => Promise<AssistantTurnResult>) {
  if (ga.busy) { toast("warn", t("助手正在回复中，请稍后")); return; }
  const turnId = newTurn();
  ga.turnId = turnId;
  ga.busy = kind;
  ga.streaming = "";
  ga.bubble = { visible: true, pages: [], index: 0, error: "", rev: ga.bubble.rev + 1 };
  beginSpeech();
  try {
    const r = await call(turnId);
    if (ga.turnId !== turnId) return;
    feedSpeech(ga.streaming || r.message?.content || "", true);
    if (r.message) {
      ga.messages = [...ga.messages, r.message].slice(-400);
      showBubble(r.message.content);
    } else {
      showBubble("", t("（没有回复内容）"));
    }
    if (r.memories_added || r.memories_forgotten) {
      void loadMemories();
      const bits = [];
      if (r.memories_added) bits.push(t("记住了 {n} 条", { n: r.memories_added }));
      if (r.memories_forgotten) bits.push(t("忘掉了 {n} 条", { n: r.memories_forgotten }));
      memoNote.text = bits.join(" · ");
      memoNote.rev++;
    }
    emitSkinEvent("agent.done");
  } catch (e) {
    if (ga.turnId !== turnId) return;
    if (String(e).includes("已停止") || String(e).toLowerCase().includes("cancel")) stopSpeaking();
    else feedSpeech(ga.streaming, true);
    const partial = visibleText(ga.streaming);
    showBubble(partial, String(e));
    emitSkinEvent("agent.error");
  } finally {
    if (ga.turnId === turnId) {
      ga.busy = "";
      ga.streaming = "";
      void refreshView();
    }
  }
}

/** 记忆变动的小提示（气泡底部短暂显示） */
export const memoNote = $state({ text: "", rev: 0 });

async function refreshView() {
  ga.view = await api.assistantConfigGet().catch(() => ga.view);
}

export async function send(text: string) {
  const body = text.trim();
  if (!body || !enabled()) return;
  const pending: AssistantMessage = { id: `local-${Date.now()}`, role: "user", content: body, provider: ga.view!.config.provider, created_at: new Date().toISOString(), interrupted: false, greeting: false };
  emitSkinEvent("message.send");
  await runTurn("send", async (turnId) => {
    ga.messages = [...ga.messages, pending];
    return api.assistantSend(body, toWire(persona(), app.locale), appState(), turnId, speechHint());
  });
  // 用库里的记录替换本地占位（拿到真实 id）
  if (!ga.busy) ga.messages = await api.assistantMessages(200).catch(() => ga.messages);
}

export async function greet(reason: "new" | "resume") {
  if (!enabled()) return;
  await runTurn("greet", (turnId) => api.assistantGreet(toWire(persona(), app.locale), appState(), turnId, reason, speechHint()));
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
  ga.bubble = { visible: true, pages: [], index: 0, error: "", rev: ga.bubble.rev + 1 };
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
  return (ga.busy === "send" || ga.busy === "greet" || ga.busy === "reset") && !ga.streaming;
}

/** Alt+A / 点击人物：打开输入框 */
export function openComposer() {
  if (!enabled()) return;
  ga.composer = true;
  ga.focusRev++;
}
export function toggleComposer() {
  if (!enabled()) return;
  if (ga.composer) ga.composer = false;
  else openComposer();
}

/** 请求设置窗口打开到某个页签（助手卡片里的「设置」按钮用） */
export const settingsRequest = $state({ tab: "" });
