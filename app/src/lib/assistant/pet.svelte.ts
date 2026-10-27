// 桌宠（主窗口一侧）：唤醒时使驾不在前台 → 屏幕右下角浮出人物小窗（pet.html）。
// 对话 / 语音 / 朗读仍由主窗口的全局助手完成；这里把要显示的状态推给小窗（pet://state），
// 并执行小窗发回的操作（pet://cmd）。小窗打开期间的提问会附上当前屏幕截图。
import { emit, listen } from "@tauri-apps/api/event";
import { api } from "../ipc";
import { app } from "../state.svelte";
import { ga, pet, conv, enabled, persona, send, cancel, visibleText, subText, openComposer } from "./store.svelte";
import { vc, inputEnabled, stopSpeaking } from "./voice.svelte";
import { endConversation } from "./conv.svelte";

export interface PetState {
  skin: string;
  name: string;
  /** 气泡正文（流式字幕 / 回复当前页） */
  text: string;
  error: string;
  busy: boolean;
  thinking: boolean;
  recording: boolean;
  transcribing: boolean;
  speaking: boolean;
  level: number;
  voiceIn: boolean;
  conv: boolean;
  enterSend: boolean;
  locale: string;
}

export type PetCmd =
  | { kind: "ready" }
  | { kind: "send"; text: string }
  | { kind: "stop" }
  | { kind: "close" }
  | { kind: "open-main" }
  | { kind: "hidden" };

/** 唤醒时使驾不在前台：用桌宠接住；返回 true = 已由桌宠处理 */
export function petWanted(): boolean {
  if (!enabled() || !ga.view?.config.pet_mode) return false;
  return document.visibilityState !== "visible" || !document.hasFocus();
}

export async function showPet(focus = false) {
  if (!enabled()) return;
  pet.active = true;
  try {
    await api.petShow(focus);
  } catch (e) {
    pet.active = false;
    throw e;
  }
  push();
}

export function hidePet() {
  if (!pet.active) return;
  petReady = false;
  listenAfterReady = false;
  pet.active = false;
  endConversation();
  void api.petHide().catch(() => {});
}

function snapshot(): PetState {
  const p = persona();
  const live = ga.subs.length ? subText(ga.subs) : visibleText(ga.streaming);
  const page = ga.bubble.visible ? ga.bubble.pages[ga.bubble.index] ?? "" : "";
  const busy = ga.busy === "send" || ga.busy === "greet" || ga.busy === "care" || ga.busy === "wake";
  return {
    skin: app.skin,
    name: p.nick || p.name,
    text: busy ? live : vc.voiceTurn && vc.subtitle ? vc.subtitle : page,
    error: ga.bubble.visible ? ga.bubble.error : "",
    busy,
    thinking: busy && !live,
    recording: vc.recording,
    transcribing: vc.transcribing,
    speaking: vc.speaking,
    level: vc.level,
    voiceIn: inputEnabled(),
    conv: conv.active,
    enterSend: app.enterSend,
    locale: app.locale,
  };
}

let last = "";
function push() {
  if (!pet.active) return;
  const s = snapshot();
  const sig = JSON.stringify(s);
  if (sig === last) return;
  last = sig;
  void emit("pet://state", s).catch(() => {});
}

let petReady = false;
function onCmd(c: PetCmd) {
  switch (c.kind) {
    case "ready":
      petReady = true;
      last = "";
      push();
      if (listenAfterReady) { listenAfterReady = false; void emit("pet://listen", null).catch(() => {}); }
      break;
    case "send": if (c.text.trim() && !ga.busy) { if (conv.active) endConversation(); void send(c.text, true); } break;
    case "stop": endConversation(); stopSpeaking(); void cancel(); break;
    case "close": hidePet(); break;
    case "hidden": pet.active = false; petReady = false; listenAfterReady = false; endConversation(); break;
    case "open-main":
      pet.active = false;
      endConversation();
      void api.petOpenMain().catch(() => {});
      openComposer();
      break;
  }
}

/** 桌宠里唤醒（或托盘「桌宠助手」）：显示并让桌宠自己开始听（录音在桌宠窗口里，主窗口可藏着） */
/** 桌宠页面就绪前到达的「开始听」（首次唤出窗口时页面还在加载，直接发会丢） */
let listenAfterReady = false;

export async function summonPet(fromWake: boolean) {
  await showPet(!fromWake || !inputEnabled());
  if (!fromWake || !inputEnabled()) return;
  if (vc.view?.config.wake_action === "open") return;
  // 连续对话循环在桌宠窗口里接续；主窗口的 conv 循环在隐藏窗口里录音会挂起，这里不再启动
  listenAfterReady = true;
  if (petReady) void emit("pet://listen", null).catch(() => {});
}

/** 主窗口挂载时调用一次；返回清理函数 */
export function initPet(): () => void {
  const offs: Array<Promise<() => void>> = [
    listen<PetCmd>("pet://cmd", (e) => onCmd(e.payload)),
    listen("pet://summon", () => { if (enabled()) void summonPet(false).catch(() => {}); else void api.voiceWakeRaise().catch(() => {}); }),
    listen("pet://hidden", () => { pet.active = false; }),
  ];
  // 状态变化频繁（录音电平、流式），节流到 ~12fps
  const timer = setInterval(push, 80);
  // 主窗口被切回前台：桌宠交还给主界面
  const onFocus = () => { if (pet.active && !ga.busy && !vc.recording && !conv.active) hidePet(); };
  addEventListener("focus", onFocus);
  return () => {
    clearInterval(timer);
    removeEventListener("focus", onFocus);
    for (const o of offs) void o.then((f) => f());
  };
}

