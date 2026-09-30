// Layered character scene: schema validation + a tiny WebGL2 renderer.
// All deformation happens in the vertex shader (mesh warp driven by an RGBA mask:
// R = hair wind, G = cloth wind, B = breathing, A = head follow). Expressions are small
// face patches blended on top of the body and deformed with the same mask.
//
// Everything a skin can tune lives in its skin.json `scene` block — the renderer has no
// knowledge of specific characters, expressions or events beyond the generic bus events.
import type { SkinEvent } from "./bus";
import { emitSkinEvent, isSkinEvent } from "./bus";
import type { MotionLevel } from "./motion.svelte";

type Vec2 = [number, number];
type Rect = [number, number, number, number];
type Keys = [number, number][];
export interface SceneLayer { id: string; src: string; rect: Rect; bind: string; slide: Vec2 }
/** Mouth flapping: open/close every `open` ms while speaking; speech comes in `burst`s separated by `rest`s. */
export interface Flap { param: string; open: Vec2; burst: Vec2 | null; rest: Vec2 | null }
export interface SceneClip {
  dur: number;
  keys: Record<string, Keys>;
  flap?: Flap;
  /** Keep running until this event (capped by maxDur) instead of stopping after `dur`. */
  until?: SkinEvent;
  maxDur: number;
  /** Minimum gap before the same clip may start again. */
  cooldown: number;
}
export interface SceneSpec {
  body: { src: string; size: Vec2; grid: Vec2 };
  mask: string;
  pivot: { neck: Vec2; chest: Vec2 };
  layers: SceneLayer[];
  idle: {
    wind: number; breath: number; hairAmp: number; coatAmp: number; breathAmp: number;
    gust: number; lag: number; windDir: number;
    blinkEvery: Vec2; blinkDouble: number; gazeMaxRot: number;
    actions: { every: Vec2; clips: string[] } | null;
  };
  clips: Record<string, SceneClip>;
  /** event → clip name(s); several names = random pick. */
  on: Partial<Record<SkinEvent, string[]>>;
}

const num = (v: unknown, min: number, max: number, def: number) => {
  const n = typeof v === "number" && Number.isFinite(v) ? v : def;
  return Math.min(max, Math.max(min, n));
};
const vec = (v: unknown, min: number, max: number, def: Vec2): Vec2 =>
  Array.isArray(v) && v.length === 2 ? [num(v[0], min, max, def[0]), num(v[1], min, max, def[1])] : def;
const range = (v: unknown, min: number, max: number, def: Vec2): Vec2 => {
  const r = vec(v, min, max, def);
  return r[0] <= r[1] ? r : [r[1], r[0]];
};
const PARAM = /^[a-z][a-zA-Z0-9_]{0,31}$/;
const MAX_SIZE = 8192;
/** Talking in bursts (not one endless flap) unless a skin explicitly disables it with `burst: null`. */
const DEFAULT_BURST: Vec2 = [900, 2200];
const DEFAULT_REST: Vec2 = [350, 1100];

function sanitizeFlap(param: string, raw: unknown): Flap | null {
  if (typeof raw === "string") {
    const m = /^flap\((\d{2,4})\.\.(\d{2,4})\)$/.exec(raw);
    if (!m) return null;
    return { param, open: range([Number(m[1]), Number(m[2])], 40, 2000, [100, 170]), burst: DEFAULT_BURST, rest: DEFAULT_REST };
  }
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const r = raw as Record<string, unknown>;
  if (!("flap" in r)) return null;
  return {
    param,
    open: range(r.flap, 40, 2000, [100, 170]),
    burst: r.burst === null ? null : range(r.burst, 200, 20000, DEFAULT_BURST),
    rest: r.rest === null ? null : range(r.rest, 100, 20000, DEFAULT_REST),
  };
}

/** Validate an untrusted `scene` manifest block. Returns null when unusable. */
export function sanitizeScene(raw: unknown): SceneSpec | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, any>;
  if (typeof r.body?.src !== "string" || typeof r.mask !== "string") return null;
  const size = vec(r.body.size, 1, MAX_SIZE, [0, 0]);
  if (!size[0] || !size[1]) return null;
  const [W, H] = size;
  const layers: SceneLayer[] = (Array.isArray(r.layers) ? r.layers : []).slice(0, 16).flatMap((l: any, i: number): SceneLayer[] => {
    if (typeof l?.src !== "string" || !Array.isArray(l.rect) || l.rect.length !== 4) return [];
    const rect = l.rect.map((v: unknown) => num(v, 0, MAX_SIZE, 0)) as Rect;
    if (rect[2] < 1 || rect[3] < 1) return [];
    const bind = typeof l.bind === "string" && PARAM.test(l.bind) ? l.bind : typeof l.id === "string" && PARAM.test(l.id) ? l.id : `layer${i}`;
    return [{ id: String(l.id ?? bind).slice(0, 32), src: l.src, rect, bind, slide: vec(l.slide, -200, 200, [0, 0]) }];
  });
  const idle = r.idle ?? {};
  const clips: Record<string, SceneClip> = {};
  for (const [name, c] of Object.entries((r.clips ?? {}) as Record<string, any>).slice(0, 24)) {
    if (!PARAM.test(name) || !c || typeof c !== "object") continue;
    const clip: SceneClip = {
      dur: num(c.dur, 50, 30000, 1500), keys: {},
      until: isSkinEvent(c.until) ? c.until : undefined,
      maxDur: num(c.maxDur, 500, 120000, 30000),
      cooldown: num(c.cooldown, 0, 60000, 0),
    };
    for (const [param, keys] of Object.entries((c.keys ?? {}) as Record<string, unknown>).slice(0, 16)) {
      if (!PARAM.test(param)) continue;
      const flap = sanitizeFlap(param, keys);
      if (flap) { clip.flap = flap; continue; }
      if (!Array.isArray(keys)) continue;
      const clean = keys.slice(0, 32).filter((k) => Array.isArray(k) && k.length === 2)
        .map((k) => [num(k[0], 0, 30000, 0), num(k[1], -1, 1, 0)] as [number, number]).sort((a, b) => a[0] - b[0]);
      if (clean.length) clip.keys[param] = clean;
    }
    clips[name] = clip;
  }
  const on: SceneSpec["on"] = {};
  for (const [event, value] of Object.entries((r.on ?? {}) as Record<string, unknown>)) {
    const names = (Array.isArray(value) ? value : [value]).filter((n): n is string => typeof n === "string" && !!clips[n]).slice(0, 8);
    if (isSkinEvent(event) && names.length) on[event] = names;
  }
  // Backwards compatible default: a flap clip on agent.streaming talks until the reply is done.
  for (const name of on["agent.streaming"] ?? []) {
    const c = clips[name];
    if (c.flap && !c.until) c.until = "agent.done";
  }
  const actionClips = (Array.isArray(idle.actions?.clips) ? idle.actions.clips : []).filter((n: unknown): n is string => typeof n === "string" && !!clips[n]).slice(0, 8);
  return {
    body: { src: r.body.src, size, grid: vec(r.body.grid, 4, 96, [40, 72]).map(Math.round) as Vec2 },
    mask: r.mask,
    pivot: { neck: vec(r.pivot?.neck, 0, MAX_SIZE, [W * 0.48, H * 0.24]), chest: vec(r.pivot?.chest, 0, MAX_SIZE, [W * 0.52, H * 0.35]) },
    layers,
    idle: {
      wind: num(idle.wind, 0, 2, 1), breath: num(idle.breath, 0, 2, 1),
      hairAmp: num(idle.hairAmp, 0, 90, 26), coatAmp: num(idle.coatAmp, 0, 60, 16), breathAmp: num(idle.breathAmp, 0, 0.04, 0.014),
      gust: num(idle.gust, 0, 1, 0.5), lag: num(idle.lag, 0, 3, 1), windDir: num(idle.windDir, -1, 1, 0.25),
      blinkEvery: range(idle.blink?.every, 0.5, 30, [2, 6]), blinkDouble: num(idle.blink?.double, 0, 1, 0.15),
      gazeMaxRot: num(idle.gaze?.maxRot, 0, 0.12, 0.045),
      actions: actionClips.length ? { every: range(idle.actions.every, 5, 600, [25, 60]), clips: actionClips } : null,
    },
    clips,
    on,
  };
}

// ------------------------------------------------------------------------------------------------

const VS = `#version 300 es
in vec2 a;
uniform vec4 uRect; uniform vec2 uBody, uCanvas, uOff, uOrigin, uNeck, uChest;
uniform float uScale, uT, uWind, uBreath, uRot, uNod, uJolt, uHair, uCoat, uBreathAmp, uLag, uDir;
uniform sampler2D uMask;
out vec2 vUv;
void main() {
  vec2 p = uRect.xy + a * uRect.zw;
  vec4 m = textureLod(uMask, p / uBody, 0.);
  float t = uT;
  // hair: a wave travelling down each strand (tips lag behind roots) + slow drift + wind bias
  float hp = t * 1.6 - p.y * .011 - m.r * uLag * 1.8;
  float sway = sin(hp) * .55 + sin(hp * 2.3 + p.x * .02) * .22 + sin(t * .47 + p.x * .004 + 1.7) * .3;
  p.x += m.r * uWind * (sway + uDir) * uHair;
  p.y += m.r * uWind * (sin(hp * 1.3 + .9) * .3 - abs(uDir) * .15) * uHair;
  // cloth: broader, heavier flutter with a faster ripple on top
  float cp = t * 1.9 - p.y * .009 + p.x * .012 - m.g * uLag;
  p.x += m.g * uWind * (sin(cp) * .6 + sin(cp * 2.7 + 1.1) * .22 + uDir * .7) * uCoat;
  p.y += m.g * uWind * sin(cp * 1.4 + .4) * uCoat * .3;
  float b = sin(t * 6.2832 / 4.2) * .5 + .5;
  p += (p - uChest) * m.b * uBreath * uBreathAmp * b;
  p.y -= m.a * uBreath * b * 2.2;
  vec2 d = p - uNeck; float r = uRot * m.a;
  p = uNeck + vec2(d.x * cos(r) - d.y * sin(r), d.x * sin(r) + d.y * cos(r));
  p.y += m.a * (uNod * 7. - uJolt * 6.);
  p.x += m.a * uJolt * sin(t * 80.) * 2.;
  p += uOff;
  vec2 s = uOrigin + p * uScale;
  gl_Position = vec4(s.x / uCanvas.x * 2. - 1., 1. - s.y / uCanvas.y * 2., 0, 1);
  vUv = a;
}`;
const FS = `#version 300 es
precision mediump float;
in vec2 vUv; uniform sampler2D uTex; uniform float uAlpha; out vec4 o;
void main() { o = texture(uTex, vUv) * uAlpha; }`;

interface Mesh { vao: WebGLVertexArrayObject; count: number }
interface GpuLayer { tex: WebGLTexture; mesh: Mesh; rect: Rect; bind: string; slide: Vec2 }
interface Running {
  clip: SceneClip; t0: number; until: number; stopOn?: SkinEvent;
  // flap state
  open: number; nextToggle: number; speaking: boolean; phaseEnd: number;
}

function sample(keys: Keys, t: number) {
  if (t <= keys[0][0]) return keys[0][1];
  for (let i = 1; i < keys.length; i++) {
    if (t <= keys[i][0]) {
      const [a0, v0] = keys[i - 1], [a1, v1] = keys[i];
      const k = (t - a0) / Math.max(1, a1 - a0), e = k < 0.5 ? 2 * k * k : 1 - (-2 * k + 2) ** 2 / 2;
      return v0 + (v1 - v0) * e;
    }
  }
  return keys[keys.length - 1][1];
}
const rand = ([a, b]: Vec2) => a + Math.random() * (b - a);
/** Clicks on real controls are never treated as "poking the character". */
const INTERACTIVE = "button,a,input,textarea,select,label,[role=button],[role=menuitem],[contenteditable=true],.cm-editor";

export interface SceneImages { body: HTMLImageElement; mask: HTMLImageElement; layers: HTMLImageElement[] }

export class SceneRenderer {
  private gl: WebGL2RenderingContext;
  private prog!: WebGLProgram;
  private u: Record<string, WebGLUniformLocation | null> = {};
  private body!: GpuLayer;
  private layers: GpuLayer[] = [];
  private maskTex!: WebGLTexture;
  private params: Record<string, number> = {};
  private running: Running[] = [];
  private lastStart = new Map<SceneClip, number>();
  private blink: { t0: number; double: boolean } | null = null;
  private nextBlink = 0;
  private nextAction = Infinity;
  private gust = 1;
  private gustTarget = 1;
  private nextGust = 0;
  private rot = 0;
  private targetRot = 0;
  private raf = 0;
  private lastFrame = 0;
  private lastTick = 0;
  private dirty = true;
  private disposed = false;
  private cssW = 1;
  private cssH = 1;
  private scale = 1;
  private originY = 0;
  private hit: { w: number; h: number; a: Uint8ClampedArray } | null = null;
  private resize: ResizeObserver;
  private onPointer = (e: PointerEvent) => {
    const r = this.canvas.getBoundingClientRect();
    const dx = (e.clientX - (r.left + r.width / 2)) / Math.max(400, innerWidth * 0.6);
    this.targetRot = Math.max(-1, Math.min(1, dx)) * this.spec.idle.gazeMaxRot;
  };
  private onDown = (e: PointerEvent) => {
    if (e.button !== 0 || (e.target instanceof Element && e.target.closest(INTERACTIVE))) return;
    if (this.hitTest(e.clientX, e.clientY)) emitSkinEvent("character.click");
  };
  private onLost = (e: Event) => { e.preventDefault(); this.fail("WebGL context lost"); };

  constructor(private canvas: HTMLCanvasElement, private spec: SceneSpec, images: SceneImages,
              private motion: () => MotionLevel, private onFail: (reason: string) => void) {
    const gl = canvas.getContext("webgl2", { premultipliedAlpha: true, alpha: true, antialias: false });
    if (!gl) throw new Error("WebGL2 unavailable");
    this.gl = gl;
    this.initProgram();
    this.maskTex = this.texture(images.mask);
    this.body = { tex: this.texture(images.body), mesh: this.grid(spec.body.grid[0], spec.body.grid[1]), rect: [0, 0, spec.body.size[0], spec.body.size[1]], bind: "", slide: [0, 0] };
    this.layers = spec.layers.map((l, i) => ({ tex: this.texture(images.layers[i]), mesh: this.grid(10, 10), rect: l.rect, bind: l.bind, slide: l.slide }));
    for (const l of spec.layers) this.params[l.bind] = 0;
    this.buildHitMap(images.body);
    const now = performance.now();
    this.scheduleBlink(now);
    this.scheduleAction(now);
    canvas.addEventListener("webglcontextlost", this.onLost);
    addEventListener("pointermove", this.onPointer, { passive: true });
    addEventListener("pointerdown", this.onDown, { passive: true, capture: true });
    this.resize = new ResizeObserver(() => this.fit());
    this.resize.observe(canvas);
    this.fit();
    this.raf = requestAnimationFrame(this.loop);
  }

  /** Is the (client) point over an opaque pixel of the character? */
  hitTest(clientX: number, clientY: number): boolean {
    if (!this.hit) return false;
    const r = this.canvas.getBoundingClientRect();
    const [W, H] = this.spec.body.size;
    const x = (clientX - r.left - 6) / this.scale, y = (clientY - r.top - this.originY) / this.scale;
    if (x < 0 || y < 0 || x >= W || y >= H) return false;
    const hx = Math.floor((x / W) * this.hit.w), hy = Math.floor((y / H) * this.hit.h);
    return this.hit.a[hy * this.hit.w + hx] > 96;
  }

  trigger(event: SkinEvent) {
    const now = performance.now();
    const before = this.running.length;
    this.running = this.running.filter((r) => r.stopOn !== event);
    if (this.running.length !== before) this.dirty = true;
    const names = this.spec.on[event];
    if (!names?.length) return;
    this.start(names[Math.floor(Math.random() * names.length)], now);
    this.scheduleAction(now);
  }

  invalidate() { this.dirty = true; }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    cancelAnimationFrame(this.raf);
    this.resize.disconnect();
    removeEventListener("pointermove", this.onPointer);
    removeEventListener("pointerdown", this.onDown, { capture: true });
    this.canvas.removeEventListener("webglcontextlost", this.onLost);
    const gl = this.gl;
    for (const l of [this.body, ...this.layers]) gl.deleteTexture(l.tex);
    gl.deleteTexture(this.maskTex);
    gl.deleteProgram(this.prog);
    gl.getExtension("WEBGL_lose_context")?.loseContext();
  }

  private start(name: string, now: number) {
    const clip = this.spec.clips[name];
    if (!clip) return;
    if (now - (this.lastStart.get(clip) ?? -Infinity) < clip.cooldown) return;
    this.lastStart.set(clip, now);
    this.running = this.running.filter((r) => r.clip !== clip);
    const until = clip.until ? now + clip.maxDur : now + clip.dur;
    this.running.push({ clip, t0: now, until, stopOn: clip.until, open: 0, nextToggle: now, speaking: true, phaseEnd: clip.flap?.burst ? now + rand(clip.flap.burst) : Infinity });
    this.dirty = true;
  }

  private fail(reason: string) {
    if (this.disposed) return;
    this.dispose();
    this.onFail(reason);
  }

  private buildHitMap(img: HTMLImageElement) {
    try {
      const w = Math.max(1, Math.round(img.naturalWidth / 4)), h = Math.max(1, Math.round(img.naturalHeight / 4));
      const c = document.createElement("canvas");
      c.width = w; c.height = h;
      const ctx = c.getContext("2d", { willReadFrequently: true });
      if (!ctx) return;
      ctx.drawImage(img, 0, 0, w, h);
      const data = ctx.getImageData(0, 0, w, h).data;
      const a = new Uint8ClampedArray(w * h);
      for (let i = 0; i < a.length; i++) a[i] = data[i * 4 + 3];
      this.hit = { w, h, a };
    } catch { this.hit = null; }
  }

  private initProgram() {
    const gl = this.gl;
    const shader = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src); gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader error");
      return s;
    };
    const p = gl.createProgram()!;
    gl.attachShader(p, shader(gl.VERTEX_SHADER, VS));
    gl.attachShader(p, shader(gl.FRAGMENT_SHADER, FS));
    gl.bindAttribLocation(p, 0, "a");
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? "link error");
    this.prog = p;
    gl.useProgram(p);
    for (const n of ["uRect", "uBody", "uCanvas", "uOff", "uOrigin", "uNeck", "uChest", "uScale", "uT", "uWind", "uBreath", "uRot", "uNod", "uJolt", "uHair", "uCoat", "uBreathAmp", "uLag", "uDir", "uMask", "uTex", "uAlpha"]) {
      this.u[n] = gl.getUniformLocation(p, n);
    }
  }

  private grid(cx: number, cy: number): Mesh {
    const gl = this.gl, v: number[] = [], idx: number[] = [];
    for (let y = 0; y <= cy; y++) for (let x = 0; x <= cx; x++) v.push(x / cx, y / cy);
    for (let y = 0; y < cy; y++) for (let x = 0; x < cx; x++) {
      const i = y * (cx + 1) + x, j = i + cx + 1;
      idx.push(i, i + 1, j, i + 1, j + 1, j);
    }
    const vao = gl.createVertexArray()!;
    gl.bindVertexArray(vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(v), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, new Uint16Array(idx), gl.STATIC_DRAW);
    gl.bindVertexArray(null);
    return { vao, count: idx.length };
  }

  private texture(img: HTMLImageElement): WebGLTexture {
    const gl = this.gl, t = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, img);
    // Plain LINEAR: skin authors ship textures at ~1.2–1.5× display size (see docs), so no mipmaps.
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return t;
  }

  private fit() {
    const dpr = Math.min(devicePixelRatio || 1, 1.5);
    this.cssW = Math.max(1, this.canvas.clientWidth);
    this.cssH = Math.max(1, this.canvas.clientHeight);
    this.canvas.width = Math.round(this.cssW * dpr);
    this.canvas.height = Math.round(this.cssH * dpr);
    const [W, H] = this.spec.body.size;
    this.scale = Math.min((this.cssH * 0.98) / H, (this.cssW * 0.95) / W);
    this.originY = this.cssH - H * this.scale;
    this.dirty = true;
  }

  private scheduleBlink(now: number) {
    this.nextBlink = now + rand(this.spec.idle.blinkEvery) * 1000;
  }
  private scheduleAction(now: number) {
    const a = this.spec.idle.actions;
    this.nextAction = a ? now + rand(a.every) * 1000 : Infinity;
  }

  /** Advance parameters; returns true while something is animating. */
  private update(now: number, level: MotionLevel): boolean {
    let active = false;
    const P = this.params;
    for (const k of Object.keys(P)) P[k] = 0;
    P.nod = 0; P.jolt = 0; P.blinkHold = 0;
    this.running = this.running.filter((r) => now < r.until);
    // occasional idle action when nothing else is playing
    const actions = this.spec.idle.actions;
    if (actions && now > this.nextAction) {
      if (!this.running.length && !document.hidden) this.start(actions.clips[Math.floor(Math.random() * actions.clips.length)], now);
      this.scheduleAction(now);
    }
    for (const r of this.running) {
      const t = now - r.t0;
      for (const [param, keys] of Object.entries(r.clip.keys)) P[param] = Math.max(P[param] ?? 0, sample(keys, r.stopOn ? Math.min(t, keys[keys.length - 1][0]) : t));
      const f = r.clip.flap;
      if (f) {
        if (f.burst && f.rest && now > r.phaseEnd) {
          r.speaking = !r.speaking;
          r.phaseEnd = now + rand(r.speaking ? f.burst : f.rest);
          if (!r.speaking) r.open = 0;
        }
        if (r.speaking && now > r.nextToggle) { r.open = r.open ? 0 : 1; r.nextToggle = now + rand(f.open); }
        P[f.param] = Math.max(P[f.param] ?? 0, r.open);
      }
      active = true;
    }
    // blink (paused while a clip drives the eyes wide open → params.blinkHold)
    if (!this.blink && now > this.nextBlink && !(P.blinkHold > 0.3)) this.blink = { t0: now, double: Math.random() < this.spec.idle.blinkDouble };
    if (this.blink) {
      const t = now - this.blink.t0, one: Keys = [[0, 0], [60, 1], [100, 1], [190, 0]];
      const end = this.blink.double ? 450 : 190;
      P.blink = Math.max(P.blink ?? 0, t < 190 ? sample(one, t) : this.blink.double && t > 260 && t < 450 ? sample(one, t - 260) : 0);
      if (t > end) { this.blink = null; this.scheduleBlink(now); }
      active = true;
    }
    const dr = this.targetRot - this.rot;
    if (Math.abs(dr) > 0.0005) { this.rot += dr * 0.12; active = true; }
    // wind gusts: a slowly wandering strength with occasional stronger puffs
    if (level === "full" && this.spec.idle.gust > 0) {
      if (now > this.nextGust) {
        const g = this.spec.idle.gust, strong = Math.random() < 0.3;
        this.gustTarget = strong ? 1 + g * (0.6 + Math.random() * 0.6) : 1 - g * 0.45 + Math.random() * g * 0.6;
        this.nextGust = now + (strong ? 1200 + Math.random() * 1200 : 2500 + Math.random() * 4500);
      }
      const dt = Math.min(100, now - this.lastTick);
      this.gust += (this.gustTarget - this.gust) * (1 - Math.exp(-dt / 700));
    }
    this.lastTick = now;
    return active;
  }

  private loop = (now: number) => {
    if (this.disposed) return;
    this.raf = requestAnimationFrame(this.loop);
    if (document.hidden) return;
    const level = this.motion();
    const wind = level === "full" ? this.spec.idle.wind * this.gust : 0;
    const breath = level === "full" ? this.spec.idle.breath : 0;
    const active = this.update(now, level);
    const continuous = wind > 0 || breath > 0;
    if ((continuous || active || this.dirty) && now - this.lastFrame >= 1000 / 30 - 1) {
      this.render(now, wind, breath);
      this.lastFrame = now;
      this.dirty = false;
    }
  };

  private render(now: number, wind: number, breath: number) {
    const gl = this.gl, u = this.u, s = this.spec, P = this.params;
    if (gl.isContextLost()) return;
    const [W, H] = s.body.size;
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.useProgram(this.prog);
    gl.uniform2f(u.uBody, W, H);
    gl.uniform2f(u.uCanvas, this.cssW, this.cssH);
    gl.uniform2f(u.uOrigin, 6, this.originY);
    gl.uniform2f(u.uNeck, ...s.pivot.neck);
    gl.uniform2f(u.uChest, ...s.pivot.chest);
    gl.uniform1f(u.uScale, this.scale);
    gl.uniform1f(u.uT, now / 1000);
    gl.uniform1f(u.uWind, wind);
    gl.uniform1f(u.uBreath, breath);
    gl.uniform1f(u.uHair, s.idle.hairAmp);
    gl.uniform1f(u.uCoat, s.idle.coatAmp);
    gl.uniform1f(u.uBreathAmp, s.idle.breathAmp);
    gl.uniform1f(u.uLag, s.idle.lag);
    gl.uniform1f(u.uDir, s.idle.windDir * Math.min(1.4, this.gust));
    gl.uniform1f(u.uRot, this.rot + (wind ? Math.sin(now / 1900) * 0.006 : 0));
    gl.uniform1f(u.uNod, P.nod ?? 0);
    gl.uniform1f(u.uJolt, P.jolt ?? 0);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.maskTex);
    gl.uniform1i(u.uMask, 0);
    gl.uniform1i(u.uTex, 1);
    const draw = (l: GpuLayer, alpha: number, off: Vec2) => {
      if (alpha < 0.01) return;
      gl.activeTexture(gl.TEXTURE1);
      gl.bindTexture(gl.TEXTURE_2D, l.tex);
      gl.uniform4f(u.uRect, ...l.rect);
      gl.uniform2f(u.uOff, ...off);
      gl.uniform1f(u.uAlpha, Math.min(1, alpha));
      gl.bindVertexArray(l.mesh.vao);
      gl.drawElements(gl.TRIANGLES, l.mesh.count, gl.UNSIGNED_SHORT, 0);
    };
    draw(this.body, 1, [0, 0]);
    // Later layers win: when several expressions are active only the strongest non-blink one shows,
    // so a blink over a smile doesn't produce a double face.
    // The most recently started clip owns the face.
    let top = "", topV = 0;
    const binds = new Set(this.layers.map((l) => l.bind));
    for (const r of this.running) {
      for (const param of [...Object.keys(r.clip.keys), ...(r.clip.flap ? [r.clip.flap.param] : [])]) {
        if (param !== "blink" && binds.has(param) && (P[param] ?? 0) > 0.01) { top = param; topV = P[param]; }
      }
    }
    for (const l of this.layers) {
      let v = Math.max(0, P[l.bind] ?? 0);
      if (l.bind !== "blink" && l.bind !== top) v = 0;
      if (l.bind === "blink" && top && topV > 0.5) v = 0;   // expressions carry their own eyes
      draw(l, v, [l.slide[0] * (1 - v), l.slide[1] * (1 - v)]);
    }
    gl.bindVertexArray(null);
  }
}
