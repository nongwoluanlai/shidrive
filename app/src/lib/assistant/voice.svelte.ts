// 全局助手语音：配置 / 组件状态、麦克风录音（16k PCM16）、逐句朗读队列（本地 TTS / 角色语音包 / 系统语音）。
import { listen } from "@tauri-apps/api/event";
import { api } from "../ipc";
import { app, toast } from "../state.svelte";
import { skinAssistant } from "../skins.svelte";
import { t } from "../i18n";
import type { SpeechHint, VoiceConfig, VoicePack, VoiceTask, VoiceView } from "../types";

export const vc = $state({
  view: null as VoiceView | null,
  tasks: {} as Record<string, VoiceTask>,
  recording: false,
  /** 录音电平 0~1 */
  level: 0,
  transcribing: false,
  speaking: false,
  /** 这一回合在朗读（已选定语音）：气泡改为显示与音频同步的字幕 */
  voiceTurn: false,
  /** 正在朗读的那一句对应的字幕（界面语言） */
  subtitle: "",
  /** 字幕序号（每换一句 +1，用于动画） */
  subRev: 0,
  /** 一个朗读回合正常读完（+1）：人物气泡随之收起 */
  spokenRev: 0,
});

let inited = false;
export async function initVoice() {
  if (!inited) {
    inited = true;
    // 第一次点击 / 按键时就把播放用的 AudioContext 准备好（WebView 的自动播放限制），后面语音唤醒等无手势的场景也能直接出声
    const unlock = () => { void unlockAudio(); if (actx?.state === "running") for (const e of ["pointerdown", "keydown"]) removeEventListener(e, unlock, true); };
    for (const e of ["pointerdown", "keydown"]) addEventListener(e, unlock, true);
    void unlockAudio();
    await listen<VoiceTask>("voice://progress", (e) => {
      const task = e.payload;
      vc.tasks = { ...vc.tasks, [task.id]: task };
      if (!task.running) void refreshVoice();
    });
  }
  await refreshVoice();
}

export async function refreshVoice() {
  try {
    const v = await api.voiceConfigGet();
    vc.view = v;
    const tasks = { ...vc.tasks };
    for (const task of v.status.tasks) tasks[task.id] = task;
    vc.tasks = tasks;
  } catch (e) {
    console.warn("[voice]", e);
  }
}

export async function saveVoice(config: VoiceConfig, baiduSecret?: string | null) {
  vc.view = await api.voiceConfigSet(config, baiduSecret);
  return vc.view;
}

export const inputEnabled = () => !!vc.view?.config.input_enabled;

// ---------------------------------------------------------------- 输出语音解析

export interface OutputVoice {
  kind: "system" | "melo" | "kokoro" | "pack";
  /** voice_tts 用的 id：melo / kokoro / pack:<id>；system 不经后端 */
  id: string;
  name: string;
  /** 朗读语言 */
  lang: string;
  /** 朗读语言与显示语言不同：模型在每句后附 [[lang: …]] */
  layer: boolean;
}

export const displayLang = () => (app.locale === "en" ? "en" : "zh");

/** 皮肤绑定的角色语音包：手动绑定 > 皮肤 manifest.assistant.voice > 语音包声明的 skins */
export function packForSkin(skin: string): VoicePack | null {
  const v = vc.view;
  if (!v || !skin) return null;
  const packs = v.status.packs;
  const bound = v.config.bindings[skin];
  if (bound === "none") return null;
  if (bound) return packs.find((p) => p.id === bound) ?? null;
  const declared = skinAssistant.id === skin ? skinAssistant.spec?.voice : "";
  return (declared ? packs.find((p) => p.id === declared) : undefined) ?? packs.find((p) => p.skins.includes(skin)) ?? null;
}

export function localModelReady(id: "melo" | "kokoro") {
  const st = vc.view?.status;
  return !!st?.runtime_installed && !!st.models.find((m) => m.id === `tts-${id}`)?.installed;
}

/** 语音包能否合成：preset 包复用通用 TTS 模型或另一个 sherpa-vits 语音包，需要先装好底模 */
export function packReady(p: VoicePack): boolean {
  if (!vc.view?.status.runtime_installed) return false;
  if (p.engine !== "preset") return true;
  if (p.base === "kokoro" || p.base === "melo") return localModelReady(p.base);
  const ref = p.base?.startsWith("pack:") ? p.base.slice(5) : "";
  return !!ref && ref !== p.id && !!vc.view.status.packs.find((x) => x.id === ref && x.engine === "sherpa-vits");
}

/** preset 语音包的底模名（设置页提示用） */
export function packBaseName(p: VoicePack): string {
  if (p.base === "melo") return "MeloTTS";
  if (p.base === "kokoro") return "Kokoro";
  const ref = p.base?.startsWith("pack:") ? p.base.slice(5) : "";
  return vc.view?.status.packs.find((x) => x.id === ref)?.name ?? ref ?? "";
}

export function outputVoice(): OutputVoice | null {
  const v = vc.view;
  if (!v?.config.output_enabled) return null;
  const c = v.config;
  const lang = displayLang();
  const hasCharacter = !!app.skin && skinAssistant.id === app.skin && skinAssistant.character;
  if (c.character_voice && hasCharacter && v.status.runtime_installed) {
    const p = packForSkin(app.skin);
    if (p && packReady(p)) {
      const pl = p.lang || "ja";
      return { kind: "pack", id: `pack:${p.id}`, name: p.name, lang: pl, layer: pl !== lang };
    }
  }
  if ((c.default_voice === "melo" || c.default_voice === "kokoro") && localModelReady(c.default_voice)) {
    return { kind: c.default_voice, id: c.default_voice, name: c.default_voice === "melo" ? "MeloTTS" : "Kokoro", lang, layer: false };
  }
  return { kind: "system", id: "system", name: t("系统语音"), lang, layer: false };
}

/** 发给后端的语音提示（约束模型的输出形态） */
export function speechHint(): SpeechHint | null {
  const o = outputVoice();
  return o ? { enabled: true, lang: o.lang, layer: o.layer } : null;
}

// ---------------------------------------------------------------- 切句

/** 可朗读的正文：去掉代码块（未闭合的从开头截断）、指令标记（未闭合的截断） */
export function speakable(raw: string): string {
  let s = raw.replace(/```[\s\S]*?```/g, "\n");
  const open = s.indexOf("```");
  if (open >= 0) s = s.slice(0, open);
  s = s.replace(/\[\[[^\]]*\]\]/g, "");
  return s.replace(/\[\[[^\]]*$/, "");
}

const ENDERS = "。！？!?；;…\n";
const CLOSERS = "。！？!?…”」』）)\"'";

/** 从 text 切出完整的句子；返回 [句子, 已消费长度]。final = 末尾不完整的也算一句 */
export function cutSentences(text: string, final: boolean): [string[], number] {
  const out: string[] = [];
  let start = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    const next = text[i + 1];
    const end = ENDERS.includes(ch) || (ch === "." && (next === undefined ? final : /\s/.test(next)));
    const long = i - start >= 48 && "，,、：:".includes(ch);
    if (end || long) {
      let j = i + 1;
      while (j < text.length && CLOSERS.includes(text[j])) j++;
      out.push(text.slice(start, j));
      start = j;
      i = j - 1;
    }
  }
  if (final && start < text.length) {
    out.push(text.slice(start));
    start = text.length;
  }
  return [out, start];
}

// 单句长度上限（按「字」计：汉字 / 假名 = 1，拉丁字母等 ≈ 1/3）：字幕一屏读得完，语音包也不会因句子太长出错
const SEG_MAX = 26;
const isWide = (ch: string) => /[\u2e80-\u9fff\uac00-\ud7af\uf900-\ufaff\uff00-\uffef\u3000-\u303f]/.test(ch);
const unitOf = (ch: string) => (isWide(ch) ? 1 : 0.34);
export const segUnits = (s: string) => { let n = 0; for (const ch of s) n += unitOf(ch); return n; };
const SOFT = "，,、：:；;—";
/** 没有标点可断时，尽量断在虚词 / 连词后面，别把一个词从中间劈开 */
const WEAK = "的了着过吧呢啊呀吗和与及并而但就也都还又再才把被让给在从向对";

/** 把过长的一句在逗号 / 顿号 / 空格处拆成几段，实在没有停顿处就硬切 */
export function splitLong(seg: string, max = SEG_MAX): string[] {
  const out: string[] = [];
  let rest = seg.trim();
  while (rest && segUnits(rest) > max) {
    const chars = Array.from(rest);
    let u = 0, limit = chars.length, soft = -1, space = -1, weak = -1;
    for (let i = 0; i < chars.length; i++) {
      u += unitOf(chars[i]);
      if (u > max) { limit = i; break; }
      if (u >= max * 0.35) {
        if (SOFT.includes(chars[i])) soft = i + 1;
        else if (chars[i] === " ") space = i;
        else if (WEAK.includes(chars[i]) && i + 1 < chars.length && !SOFT.includes(chars[i + 1])) weak = i + 1;
      }
    }
    const cut = soft > 0 ? soft : space > 0 ? space : weak > 0 ? weak : Math.max(1, limit);
    const head = chars.slice(0, cut).join("").trim();
    rest = chars.slice(cut).join("").trim();
    if (head) out.push(head);
  }
  if (rest) out.push(rest);
  return out;
}

/** 按比例把字幕切成 n 段（尽量落在标点处），与拆开的配音逐段对应 */
export function splitShow(show: string, weights: number[]): string[] {
  const n = weights.length;
  if (n <= 1) return [show];
  const chars = Array.from(show);
  const total = weights.reduce((a, b) => a + b, 0) || 1;
  const out: string[] = [];
  let start = 0, acc = 0;
  for (let k = 0; k < n - 1; k++) {
    acc += weights[k];
    let cut = Math.round((chars.length * acc) / total);
    // 附近 ±6 字内有停顿就挪到停顿后
    let best = -1;
    for (let d = 0; d <= 6 && best < 0; d++) {
      for (const c of [cut + d, cut - d]) {
        if (c > start && c < chars.length && (SOFT + "。！？!?…").includes(chars[c - 1])) { best = c; break; }
      }
    }
    cut = Math.min(chars.length, Math.max(start + 1, best > 0 ? best : cut));
    out.push(chars.slice(start, cut).join("").trim());
    start = cut;
  }
  out.push(chars.slice(start).join("").trim());
  return out.map((x, i) => x || (i ? out[i - 1] : show));
}

/** 去掉 Markdown 记号、链接地址等不适合读出来的内容 */
export function cleanSpeech(seg: string): string {
  return seg
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/\S+/g, "")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/^\s*([-+*]|\d+\.)\s+/gm, "")
    .replace(/[*_#>|~`]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

const SAYABLE = /[\p{L}\p{N}]/u;

/** 一句朗读：say = 读出来的文本，show = 气泡里同步显示的字幕 */
export type SpeechSeg = { say: string; show: string };

/** 配音模式：每个 [[lang: …]] 是一句台词，字幕取它前面那段界面语言的原文 */
export function layerSegments(raw: string, lang: string): SpeechSeg[] {
  const re = new RegExp(`\\[\\[\\s*${lang}\\s*[:：]\\s*([^\\[\\]]*?)\\s*\\]\\]`, "gi");
  const out: SpeechSeg[] = [];
  let prev = 0;
  for (const m of raw.matchAll(re)) {
    const show = cleanSpeech(speakable(raw.slice(prev, m.index)));
    prev = (m.index ?? 0) + m[0].length;
    const say = m[1].trim();
    if (!SAYABLE.test(say)) continue;
    const says = splitLong(say);
    const shows = splitShow(show || say, says.map(segUnits));
    says.forEach((x, i) => out.push({ say: x, show: shows[i] }));
  }
  return out;
}

// ---------------------------------------------------------------- 朗读队列

type Clip = ({ rate: number; pcm: string } | { text: string }) & { show: string };

let gen = 0;
let queue: SpeechSeg[] = [];
let running = false;
/** 本回合的原文已全部喂入（队列读完即字幕结束） */
let ended = false;
let current: { stop: () => void } | null = null;
let actx: AudioContext | null = null;
let turnVoice: OutputVoice | null = null;
let consumed = 0;
let warned = false;
/** 这一回合确实读出过声音（读完时才通知气泡收起） */
let played = false;
/** 试听不通知气泡 */
let notify = false;

/** 新回合开始：停掉上一段朗读，按当前设置选定这一轮的语音 */
export function beginSpeech() {
  stopSpeaking();
  turnVoice = outputVoice();
  vc.voiceTurn = !!turnVoice;
  consumed = 0;
  warned = false;
  ended = false;
  played = false;
  notify = true;
}

export function stopSpeaking() {
  gen++;
  turnVoice = null;
  queue = [];
  running = false;
  const c = current;
  current = null;
  c?.stop();
  if ("speechSynthesis" in window) speechSynthesis.cancel();
  vc.speaking = false;
  vc.voiceTurn = false;
  vc.subtitle = "";
}

/** 喂入流式原文；final = 回合结束（把剩下的半句也读掉） */
export function feedSpeech(raw: string, final: boolean) {
  const v = turnVoice;
  if (!v) return;
  let segs: SpeechSeg[];
  if (v.layer) {
    const all = layerSegments(raw, v.lang);
    segs = all.slice(consumed);
    consumed = all.length;
  } else {
    const s = speakable(raw);
    const [parts, used] = cutSentences(s.slice(consumed), final);
    consumed += used;
    segs = parts.map(cleanSpeech).flatMap((x) => splitLong(x)).filter((x) => SAYABLE.test(x)).map((x) => ({ say: x, show: x }));
  }
  if (final) ended = true;
  if (!segs.length) {
    if (final && !running) { vc.voiceTurn = false; vc.subtitle = ""; }
    return;
  }
  queue.push(...segs);
  void pump();
}

/** 试听：用指定语音读一句（不影响对话回合的选择） */
export function previewVoice(voice: OutputVoice, text: string) {
  stopSpeaking();
  turnVoice = voice;
  consumed = 0;
  warned = false;
  ended = true;
  notify = false;
  queue.push({ say: text, show: text });
  void pump();
}

async function synth(v: OutputVoice, seg: SpeechSeg, my: number): Promise<Clip | null> {
  if (v.kind === "system") return { text: seg.say, show: seg.show };
  try {
    const r = await api.voiceTts(seg.say, v.id);
    return r.pcm ? { rate: r.rate, pcm: r.pcm, show: seg.show } : null;
  } catch (e) {
    if (my === gen && !warned) {
      warned = true;
      // 合成失败：气泡退回显示全文，不再等字幕
      vc.voiceTurn = false;
      toast("error", `${t("语音合成失败")}：${e}`);
    }
    return null;
  }
}

async function pump() {
  if (running) return;
  const v = turnVoice;
  if (!v) return;
  running = true;
  const my = gen;
  let next: Promise<Clip | null> | null = null;
  try {
    while (my === gen && (queue.length || next)) {
      const cur: Promise<Clip | null> = next ?? synth(v, queue.shift()!, my);
      next = queue.length ? synth(v, queue.shift()!, my) : null;
      const clip = await cur;
      if (my !== gen) break;
      if (!clip) continue;
      vc.speaking = true;
      played = true;
      vc.subtitle = clip.show;
      vc.subRev++;
      await ("text" in clip ? speakSystem(clip.text, v.lang) : playPcm(clip));
    }
  } finally {
    if (my === gen) {
      running = false;
      vc.speaking = false;
      if (ended && !queue.length) {
        vc.voiceTurn = false;
        // 字幕留着（气泡收起前继续显示最后一句），下一回合 / stopSpeaking 时清空
        if (played && notify) vc.spokenRev++;
        else vc.subtitle = "";
        played = false;
      }
    }
  }
}

const volume = () => vc.view?.config.volume ?? 1;
const rate = () => vc.view?.config.rate ?? 1;

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function bytesToB64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** 创建 / 恢复播放用的 AudioContext。没有用户手势时 resume() 可能一直挂起：最多等 timeout 毫秒，不卡住朗读队列 */
export async function unlockAudio(timeout = 600): Promise<boolean> {
  try {
    const a = (actx ??= new AudioContext());
    if (a.state === "suspended") await Promise.race([a.resume().catch(() => {}), sleep(timeout)]);
    return a.state === "running";
  } catch {
    return false;
  }
}

/** 预热：提前启动语音服务并加载当前语音的模型（第一句话不用等加载，也不会没声） */
const warmed = new Set<string>();
export async function warmVoice(v: OutputVoice | null = outputVoice()) {
  void unlockAudio();
  if (!v) return;
  if (v.kind === "system") { systemVoices(); return; }
  if (warmed.has(v.id)) return;
  warmed.add(v.id);
  try {
    await api.voiceTts(v.lang === "ja" ? "はい。" : v.lang === "en" ? "OK." : "好。", v.id);
  } catch (e) {
    warmed.delete(v.id);
    console.warn("[voice] warm-up", e);
  }
}

async function playPcm(c: { rate: number; pcm: string }): Promise<void> {
  await unlockAudio();
  const a = actx!;
  if (!a || a.state !== "running") {
    if (!warned) { warned = true; toast("warn", t("浏览器暂不允许自动播放声音：点一下窗口任意位置后再试")); }
    return;
  }
  const bytes = b64ToBytes(c.pcm);
  const n = bytes.length >> 1;
  if (!n) return;
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const buf = a.createBuffer(1, n, c.rate);
  const ch = buf.getChannelData(0);
  for (let i = 0; i < n; i++) ch[i] = dv.getInt16(i * 2, true) / 32768;
  const src = a.createBufferSource();
  src.buffer = buf;
  const g = a.createGain();
  g.gain.value = volume();
  src.connect(g).connect(a.destination);
  await new Promise<void>((resolve) => {
    let done = false;
    const end = () => { if (!done) { done = true; resolve(); } };
    src.onended = end;
    current = { stop: () => { try { src.stop(); } catch { /* 已结束 */ } end(); } };
    src.start();
  });
  current = null;
}

/** 系统语音（Web Speech，Windows 自带语音库） */
export function systemVoices(): SpeechSynthesisVoice[] {
  return "speechSynthesis" in window ? speechSynthesis.getVoices() : [];
}

function speakSystem(text: string, lang: string): Promise<void> {
  if (!("speechSynthesis" in window)) return Promise.resolve();
  return new Promise((resolve) => {
    const u = new SpeechSynthesisUtterance(text);
    u.lang = lang === "en" ? "en-US" : lang === "ja" ? "ja-JP" : "zh-CN";
    const name = vc.view?.config.system_voice;
    const list = systemVoices();
    u.voice = (name ? list.find((v) => v.name === name) : undefined) ?? list.find((v) => v.lang.toLowerCase().startsWith(u.lang.slice(0, 2).toLowerCase())) ?? null;
    u.rate = rate();
    u.volume = volume();
    let done = false;
    const end = () => { if (!done) { done = true; current = null; resolve(); } };
    u.onend = end;
    u.onerror = end;
    current = { stop: () => { speechSynthesis.cancel(); end(); } };
    speechSynthesis.speak(u);
  });
}

// ---------------------------------------------------------------- 录音

let stopHandle: ((keep: boolean) => void) | null = null;

/** 结束当前录音（keep = 保留已录内容去识别） */
export function stopCapture(keep = true) {
  stopHandle?.(keep);
}

/** 线性插值重采样到 16k，转 PCM16LE */
function to16k(chunks: Float32Array[], total: number, inRate: number): Int16Array {
  const all = new Float32Array(total);
  let o = 0;
  for (const c of chunks) { all.set(c, o); o += c.length; }
  const n = Math.floor((total * 16000) / inRate);
  const out = new Int16Array(n);
  const step = inRate / 16000;
  for (let i = 0; i < n; i++) {
    const x = i * step;
    const k = Math.floor(x);
    const f = x - k;
    const v = (all[k] ?? 0) * (1 - f) + (all[k + 1] ?? all[k] ?? 0) * f;
    out[i] = Math.max(-32768, Math.min(32767, Math.round(v * 32767)));
  }
  return out;
}

/**
 * 录一段话：检测到说话后静音约 1.4 秒自动结束；idleSeconds 秒（默认 8）内没说话则放弃；最长 maxSeconds 秒。
 * 返回 base64 PCM16（16k 单声道），没录到有效语音返回 null。
 */
export async function captureSpeech(maxSeconds: number, idleSeconds = 8): Promise<string | null> {
  if (vc.recording) { stopCapture(true); return null; }
  stopSpeaking();
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
  } catch (e) {
    throw new Error(`${t("无法使用麦克风")}：${e instanceof Error ? e.message : e}`);
  }
  const ac = new AudioContext();
  const source = ac.createMediaStreamSource(stream);
  const proc = ac.createScriptProcessor(4096, 1, 1);
  const mute = ac.createGain();
  mute.gain.value = 0;
  source.connect(proc);
  proc.connect(mute);
  mute.connect(ac.destination);
  const chunks: Float32Array[] = [];
  let total = 0;
  let floor = 0;
  let frames = 0;
  let speech = false;
  let silentMs = 0;
  const started = performance.now();
  vc.recording = true;
  vc.level = 0;
  return new Promise<string | null>((resolve) => {
    let done = false;
    const finish = (keep: boolean) => {
      if (done) return;
      done = true;
      stopHandle = null;
      proc.onaudioprocess = null;
      try { proc.disconnect(); source.disconnect(); mute.disconnect(); } catch { /* ignore */ }
      stream.getTracks().forEach((tr) => tr.stop());
      void ac.close();
      vc.recording = false;
      vc.level = 0;
      const enough = total / ac.sampleRate >= 0.4;
      if (!keep || !enough || (!speech && performance.now() - started < 600)) { resolve(null); return; }
      const pcm = to16k(chunks, total, ac.sampleRate);
      resolve(bytesToB64(new Uint8Array(pcm.buffer)));
    };
    stopHandle = finish;
    proc.onaudioprocess = (e) => {
      const d = new Float32Array(e.inputBuffer.getChannelData(0));
      chunks.push(d);
      total += d.length;
      let sum = 0;
      for (let i = 0; i < d.length; i++) sum += d[i] * d[i];
      const rms = Math.sqrt(sum / d.length);
      vc.level = Math.min(1, rms * 10);
      frames++;
      if (frames <= 4) floor = frames === 1 ? rms : Math.min(floor, rms);
      const thr = Math.max(0.012, floor * 2.5);
      const ms = (d.length / ac.sampleRate) * 1000;
      if (rms > thr) { speech = true; silentMs = 0; } else if (speech) silentMs += ms;
      const elapsed = performance.now() - started;
      if (speech && silentMs > 1400) finish(true);
      else if (elapsed > maxSeconds * 1000) finish(true);
      else if (!speech && elapsed > idleSeconds * 1000) finish(false);
    };
  });
}

/** 录音 + 识别；返回文字（没听清返回 ""） */
export async function listenOnce(idleSeconds = 8): Promise<string> {
  const cfg = vc.view?.config;
  const pcm = await captureSpeech(cfg?.input_max_seconds ?? 30, idleSeconds);
  if (!pcm) return "";
  vc.transcribing = true;
  try {
    const r = await api.voiceStt(pcm, 16000);
    return r.text;
  } finally {
    vc.transcribing = false;
  }
}
