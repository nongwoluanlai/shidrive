// 连续语音对话：唤醒词 / 主动关怀之后，助手说完自动接着听；她判断聊完了（[[结束对话]]）或用户不再说话时自动退出。
import { toast } from "../state.svelte";
import { t } from "../i18n";
import { conv, enabled, ga, openComposer, send } from "./store.svelte";
import { inputEnabled, listenOnce, stopCapture, vc } from "./voice.svelte";

/** 最多连续多少轮（防止误识别 / 回声导致停不下来） */
const MAX_ROUNDS = 20;
let gen = 0;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** 等助手回复完、读完（最多 maxMs） */
async function waitIdle(my: number, maxMs = 120000) {
  const end = Date.now() + maxMs;
  while (my === gen && Date.now() < end && (ga.busy || vc.speaking || vc.voiceTurn || vc.transcribing)) await sleep(150);
  // 读完后稍等片刻再开麦，免得录进朗读的尾音
  if (my === gen) await sleep(350);
}

/**
 * 开始连续对话。
 * - wake：先听用户说（第一轮没听清会提示）
 * - care：助手已经开口了，等她说完再听；用户不接话就安静退出
 */
export async function startConversation(source: "wake" | "care") {
  if (!enabled() || !inputEnabled()) return;
  if (conv.active) return;
  const my = ++gen;
  conv.active = true;
  conv.source = source;
  conv.rounds = 0;
  // 打开控制台但不抢焦点（用户可能正在别处打字）
  if (source === "wake") openComposer();
  else ga.composer = true;
  try {
    while (my === gen && conv.rounds < MAX_ROUNDS && enabled()) {
      await waitIdle(my);
      if (my !== gen) break;
      if (vc.recording) break;
      let text = "";
      try {
        text = (await listenOnce(conv.rounds === 0 && source === "wake" ? 8 : 10)).trim();
      } catch (e) {
        if (my === gen) toast("error", e instanceof Error ? e.message : String(e));
        break;
      }
      if (my !== gen) break;
      if (!text) {
        if (conv.rounds === 0 && source === "wake") toast("info", t("没有听清，请再说一次"));
        break;
      }
      conv.rounds++;
      const r = await send(text);
      if (my !== gen || !r || r.end_conversation) break;
    }
  } finally {
    if (my === gen) {
      conv.active = false;
      conv.source = "";
    }
  }
}

/** 用户手动结束（点「结束」、打字、Esc、关闭控制台…） */
export function endConversation() {
  if (!conv.active) return;
  gen++;
  conv.active = false;
  conv.source = "";
  if (vc.recording) stopCapture(false);
}
