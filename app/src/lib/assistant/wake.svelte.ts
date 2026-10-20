// 唤醒词（如「Christina」）：开启后常开麦克风，每约 0.3 秒把 16k 音频片段交给本地 sherpa-onnx 关键词检测；
// 听到唤醒词 → 提示音 → 唤起助手（直接语音输入或只打开输入框）。全部离线，音频不离开本机。
// 录音 / 识别 / 朗读期间不检测（避免助手自己的声音触发）；窗口在后台时按设置暂停并释放麦克风。
import { api } from "../ipc";
import { t } from "../i18n";
import { vc } from "./voice.svelte";

export const wk = $state({
  /** 麦克风已打开、正在检测 */
  listening: false,
  error: "",
  /** 页面可见（后台时按设置暂停） */
  visible: typeof document === "undefined" ? true : document.visibilityState === "visible",
  /** 出错后重试的计数（变化即重新评估） */
  retry: 0,
  /** 设置页打开时只提示「听到了」，不唤起助手 */
  testing: false,
  hits: 0,
  lastHit: "",
  lastAt: 0,
  level: 0,
  /** 音频上下文被自动播放策略挂起（还没点过窗口）：点一下即开始 */
  suspended: false,
});

/** 当前设置下是否应当监听（不含助手开关等外部条件） */
export function wakeReady(): boolean {
  const v = vc.view;
  if (!v?.config.wake_enabled) return false;
  const st = v.status;
  return !!st.runtime_installed && !!st.models.find((m) => m.id === "kws-gigaspeech")?.installed;
}

type Hit = (word: string) => void;

const CHUNK = 4800; // 0.3 s @16k
const MAX_BACKLOG = 16000 * 2;
const COOLDOWN_MS = 2500;

let stream: MediaStream | null = null;
let ac: AudioContext | null = null;
let proc: ScriptProcessorNode | null = null;
let src: MediaStreamAudioSourceNode | null = null;
let sink: GainNode | null = null;
let pending: Int16Array[] = [];
let pendingLen = 0;
let inflight = false;
let starting = false;
let session = 0;
let cooldownUntil = 0;
let onHit: Hit | null = null;
let retryTimer: ReturnType<typeof setTimeout> | undefined;
let visInit = false;

function initVisibility() {
  if (visInit || typeof document === "undefined") return;
  visInit = true;
  document.addEventListener("visibilitychange", () => (wk.visible = document.visibilityState === "visible"));
}

/** 打开 / 关闭检测（由组件里的 $effect 根据设置与可见性调用） */
export function setWake(on: boolean, hit: Hit) {
  initVisibility();
  onHit = hit;
  if (on) void start();
  else stop(true);
}

const paused = () => vc.recording || vc.transcribing || vc.speaking || performance.now() < cooldownUntil;

function bytesToB64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

async function start() {
  if (stream || starting) return;
  starting = true;
  const my = ++session;
  try {
    const ms = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
    if (my !== session) { ms.getTracks().forEach((tr) => tr.stop()); return; }
    stream = ms;
    ac = new AudioContext();
    src = ac.createMediaStreamSource(ms);
    proc = ac.createScriptProcessor(4096, 1, 1);
    sink = ac.createGain();
    sink.gain.value = 0;
    src.connect(proc);
    proc.connect(sink);
    sink.connect(ac.destination);
    // 启动时还没有用户操作，AudioContext 可能被挂起：先试着恢复，不行就等第一次点击 / 按键
    const ctx = ac;
    const sync = () => { if (ac === ctx) wk.suspended = ctx.state !== "running"; };
    ctx.onstatechange = sync;
    if (ctx.state !== "running") {
      void ctx.resume().catch(() => {});
      const kick = () => {
        removeEventListener("pointerdown", kick, true);
        removeEventListener("keydown", kick, true);
        if (ctx.state === "suspended") void ctx.resume().catch(() => {});
      };
      addEventListener("pointerdown", kick, true);
      addEventListener("keydown", kick, true);
    }
    sync();
    const inRate = ac.sampleRate;
    const step = inRate / 16000;
    let pos = 0; // 下一个输出样本在当前块里的位置（可为负：落在上一块末尾与本块开头之间）
    let last = 0;
    proc.onaudioprocess = (e) => {
      const d = e.inputBuffer.getChannelData(0);
      let sum = 0;
      for (let i = 0; i < d.length; i += 4) sum += d[i] * d[i];
      wk.level = Math.min(1, Math.sqrt(sum / (d.length / 4)) * 10);
      const out = new Int16Array(Math.ceil((d.length - pos) / step) + 1);
      let n = 0;
      let x = pos;
      for (; x < d.length - 1; x += step) {
        const k = Math.floor(x);
        const f = x - k;
        const a = k < 0 ? last : d[k];
        const b = k < 0 ? d[0] : d[k + 1];
        const v = a * (1 - f) + b * f;
        out[n++] = Math.max(-32768, Math.min(32767, Math.round(v * 32767)));
      }
      pos = x - d.length;
      last = d[d.length - 1];
      if (paused()) { pending = []; pendingLen = 0; return; }
      pending.push(out.subarray(0, n));
      pendingLen += n;
      while (pendingLen > MAX_BACKLOG && pending.length > 1) pendingLen -= pending.shift()!.length;
      if (pendingLen >= CHUNK && !inflight) void flush(my);
    };
    wk.listening = true;
    wk.error = "";
  } catch (e) {
    if (my === session) fail(`${t("无法使用麦克风")}：${e instanceof Error ? e.message : e}`);
  } finally {
    starting = false;
  }
}

async function flush(my: number) {
  const all = new Int16Array(pendingLen);
  let o = 0;
  for (const c of pending) { all.set(c, o); o += c.length; }
  pending = [];
  pendingLen = 0;
  inflight = true;
  try {
    const r = await api.voiceKwsFeed(bytesToB64(new Uint8Array(all.buffer)), 16000);
    if (my !== session) return;
    if (wk.error) wk.error = "";
    if (r.hits.length && !paused()) {
      cooldownUntil = performance.now() + COOLDOWN_MS;
      wk.hits++;
      wk.lastHit = r.hits[0];
      wk.lastAt = Date.now();
      onHit?.(r.hits[0]);
    }
  } catch (e) {
    if (my === session) fail(String(e));
  } finally {
    inflight = false;
  }
}

function fail(msg: string) {
  stop(false);
  wk.error = msg;
  clearTimeout(retryTimer);
  // 过一会儿重新评估（比如麦克风被别的程序占用、语音服务刚被重启）
  retryTimer = setTimeout(() => wk.retry++, 15000);
}

function stop(releaseServer: boolean) {
  session++;
  const had = !!stream;
  if (proc) proc.onaudioprocess = null;
  try { proc?.disconnect(); src?.disconnect(); sink?.disconnect(); } catch { /* ignore */ }
  stream?.getTracks().forEach((tr) => tr.stop());
  if (ac) void ac.close().catch(() => {});
  stream = null; ac = null; proc = null; src = null; sink = null;
  pending = [];
  pendingLen = 0;
  wk.listening = false;
  wk.level = 0;
  wk.suspended = false;
  if (releaseServer) {
    clearTimeout(retryTimer);
    wk.error = "";
    if (had) void api.voiceKwsStop().catch(() => {});
  }
}

/** 唤醒提示音：两声短促的上行音 */
export function chime(volume = 1) {
  try {
    // 优先用检测在用的那个（已在运行，不受自动播放限制）
    const own = !(ac && ac.state === "running");
    const a = own ? new AudioContext() : ac!;
    const g = a.createGain();
    g.connect(a.destination);
    const t0 = a.currentTime + 0.01;
    g.gain.setValueAtTime(0, t0);
    [[740, 0], [1110, 0.11]].forEach(([f, dt]) => {
      const o = a.createOscillator();
      o.type = "sine";
      o.frequency.value = f;
      o.connect(g);
      o.start(t0 + dt);
      o.stop(t0 + dt + 0.1);
      g.gain.setValueAtTime(0.0001, t0 + dt);
      g.gain.exponentialRampToValueAtTime(0.12 * Math.max(0.05, volume), t0 + dt + 0.015);
      g.gain.exponentialRampToValueAtTime(0.0001, t0 + dt + 0.1);
    });
    if (own) setTimeout(() => void a.close().catch(() => {}), 600);
  } catch { /* 没有声卡 */ }
}

const KURISU_ZH = [
  "……都说了，不要加「蒂娜」！——什么事？",
  "我才不是克里斯蒂娜！……好啦，说吧。",
  "又是这个称呼……算了，我在听。",
];
const KURISU_EN = [
  "There's no \"-tina\"! …Fine, what is it?",
  "Don't call me Christina! …I'm listening.",
  "That nickname again… Go ahead.",
];

/** 牧濑红莉栖被叫「Christina」时的固定吐槽（其它人设返回空） */
export function wakeQuip(personaName: string, word: string, locale: string): string {
  if (personaName !== "牧濑红莉栖" || !/tina/i.test(word)) return "";
  const list = locale === "en" ? KURISU_EN : KURISU_ZH;
  return list[Math.floor(Math.random() * list.length)];
}
