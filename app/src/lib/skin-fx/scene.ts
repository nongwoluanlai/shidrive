// Layered character scene: schema validation + a tiny WebGL2 renderer.
// All deformation happens in the vertex shader (mesh warp driven by an RGBA mask:
// R = hair wind, G = cloth wind, B = breathing, A = head follow). Expressions are small
// face patches blended on top of the body and deformed with the same mask.
import type { SkinEvent } from "./bus";
import { isSkinEvent } from "./bus";
import type { MotionLevel } from "./motion.svelte";

type Vec2 = [number, number];
type Rect = [number, number, number, number];
type Keys = [number, number][];
export interface SceneLayer { id: string; src: string; rect: Rect; bind: string; slide: Vec2 }
export interface SceneClip { dur: number; keys: Record<string, Keys>; flap?: { param: string; min: number; max: number } }
export interface SceneSpec {
  body: { src: string; size: Vec2; grid: Vec2 };
  mask: string;
  pivot: { neck: Vec2; chest: Vec2 };
  layers: SceneLayer[];
  idle: { wind: number; breath: number; hairAmp: number; coatAmp: number; breathAmp: number; blinkEvery: Vec2; blinkDouble: number; gazeMaxRot: number };
  clips: Record<string, SceneClip>;
  on: Partial<Record<SkinEvent, string>>;
}

const num = (v: unknown, min: number, max: number, def: number) => {
  const n = typeof v === "number" && Number.isFinite(v) ? v : def;
  return Math.min(max, Math.max(min, n));
};
const vec = (v: unknown, min: number, max: number, def: Vec2): Vec2 =>
  Array.isArray(v) && v.length === 2 ? [num(v[0], min, max, def[0]), num(v[1], min, max, def[1])] : def;
const PARAM = /^[a-z][a-zA-Z0-9_]{0,31}$/;
const MAX_SIZE = 8192;

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
  for (const [name, c] of Object.entries((r.clips ?? {}) as Record<string, any>).slice(0, 16)) {
    if (!PARAM.test(name) || !c || typeof c !== "object") continue;
    const clip: SceneClip = { dur: num(c.dur, 50, 30000, 1500), keys: {} };
    for (const [param, keys] of Object.entries((c.keys ?? {}) as Record<string, unknown>).slice(0, 16)) {
      if (!PARAM.test(param)) continue;
      const flap = typeof keys === "string" && /^flap\((\d{2,4})\.\.(\d{2,4})\)$/.exec(keys);
      if (flap) { clip.flap = { param, min: num(Number(flap[1]), 40, 2000, 100), max: num(Number(flap[2]), 40, 2000, 170) }; continue; }
      if (!Array.isArray(keys)) continue;
      const clean = keys.slice(0, 32).filter((k) => Array.isArray(k) && k.length === 2)
        .map((k) => [num(k[0], 0, 30000, 0), num(k[1], -1, 1, 0)] as [number, number]).sort((a, b) => a[0] - b[0]);
      if (clean.length) clip.keys[param] = clean;
    }
    clips[name] = clip;
  }
  const on: SceneSpec["on"] = {};
  for (const [event, clip] of Object.entries((r.on ?? {}) as Record<string, unknown>)) {
    if (isSkinEvent(event) && typeof clip === "string" && clips[clip]) on[event] = clip;
  }
  return {
    body: { src: r.body.src, size, grid: vec(r.body.grid, 4, 96, [40, 72]).map(Math.round) as Vec2 },
    mask: r.mask,
    pivot: { neck: vec(r.pivot?.neck, 0, MAX_SIZE, [W * 0.48, H * 0.24]), chest: vec(r.pivot?.chest, 0, MAX_SIZE, [W * 0.52, H * 0.35]) },
    layers,
    idle: {
      wind: num(idle.wind, 0, 2, 1), breath: num(idle.breath, 0, 2, 1),
      hairAmp: num(idle.hairAmp, 0, 60, 26), coatAmp: num(idle.coatAmp, 0, 40, 16), breathAmp: num(idle.breathAmp, 0, 0.04, 0.014),
      blinkEvery: vec(idle.blink?.every, 0.5, 30, [2, 6]), blinkDouble: num(idle.blink?.double, 0, 1, 0.15),
      gazeMaxRot: num(idle.gaze?.maxRot, 0, 0.12, 0.045),
    },
    clips,
    on,
  };
}

// ------------------------------------------------------------------------------------------------

const VS = `#version 300 es
in vec2 a;
uniform vec4 uRect; uniform vec2 uBody, uCanvas, uOff, uOrigin, uNeck, uChest;
uniform float uScale, uT, uWind, uBreath, uRot, uNod, uJolt, uHair, uCoat, uBreathAmp;
uniform sampler2D uMask;
out vec2 vUv;
void main() {
  vec2 p = uRect.xy + a * uRect.zw;
  vec4 m = textureLod(uMask, p / uBody, 0.);
  float t = uT;
  float ph = t * 1.7 + p.y * .012;
  p.x += m.r * uWind * (sin(ph) * .6 + sin(t * .93 + p.y * .007 + 1.3) * .4) * uHair;
  p.y += m.r * uWind * sin(ph * .8 + .7) * uHair * .2;
  p.x += m.g * uWind * (sin(t * 1.25 + p.x * .01 + p.y * .006) * .7 + sin(t * 2.1 + p.y * .02) * .3) * uCoat;
  p.y += m.g * uWind * sin(t * 1.1 + p.x * .013) * uCoat * .25;
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
interface Running { clip: SceneClip; t0: number; until: number }

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
  private blink: { t0: number; double: boolean } | null = null;
  private nextBlink = 0;
  private rot = 0;
  private targetRot = 0;
  private flapOpen = 0;
  private nextFlap = 0;
  private raf = 0;
  private lastFrame = 0;
  private dirty = true;
  private disposed = false;
  private cssW = 1;
  private cssH = 1;
  private resize: ResizeObserver;
  private onPointer = (e: PointerEvent) => {
    const r = this.canvas.getBoundingClientRect();
    const dx = (e.clientX - (r.left + r.width / 2)) / Math.max(400, innerWidth * 0.6);
    this.targetRot = Math.max(-1, Math.min(1, dx)) * this.spec.idle.gazeMaxRot;
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
    this.scheduleBlink(performance.now());
    canvas.addEventListener("webglcontextlost", this.onLost);
    addEventListener("pointermove", this.onPointer, { passive: true });
    this.resize = new ResizeObserver(() => this.fit());
    this.resize.observe(canvas);
    this.fit();
    this.raf = requestAnimationFrame(this.loop);
  }

  trigger(event: SkinEvent) {
    const now = performance.now();
    if (event === "agent.done") { // stop open-ended talking
      this.running = this.running.filter((r) => !(r.clip.flap && r.until === Infinity));
    }
    const name = this.spec.on[event];
    const clip = name ? this.spec.clips[name] : undefined;
    if (!clip) return;
    this.running = this.running.filter((r) => r.clip !== clip);
    const openEnded = event === "agent.streaming" && !!clip.flap;
    this.running.push({ clip, t0: now, until: openEnded ? Infinity : now + clip.dur });
    this.dirty = true;
  }

  invalidate() { this.dirty = true; }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    cancelAnimationFrame(this.raf);
    this.resize.disconnect();
    removeEventListener("pointermove", this.onPointer);
    this.canvas.removeEventListener("webglcontextlost", this.onLost);
    const gl = this.gl;
    for (const l of [this.body, ...this.layers]) gl.deleteTexture(l.tex);
    gl.deleteTexture(this.maskTex);
    gl.deleteProgram(this.prog);
    gl.getExtension("WEBGL_lose_context")?.loseContext();
  }

  private fail(reason: string) {
    if (this.disposed) return;
    this.dispose();
    this.onFail(reason);
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
    for (const n of ["uRect", "uBody", "uCanvas", "uOff", "uOrigin", "uNeck", "uChest", "uScale", "uT", "uWind", "uBreath", "uRot", "uNod", "uJolt", "uHair", "uCoat", "uBreathAmp", "uMask", "uTex", "uAlpha"]) {
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
    this.dirty = true;
  }

  private scheduleBlink(now: number) {
    const [a, b] = this.spec.idle.blinkEvery;
    this.nextBlink = now + (a + Math.random() * (b - a)) * 1000;
  }

  /** Advance parameters; returns true while something is animating. */
  private update(now: number): boolean {
    let active = false;
    const P = this.params;
    for (const k of Object.keys(P)) P[k] = 0;
    P.nod = 0; P.jolt = 0;
    this.running = this.running.filter((r) => now < r.until);
    let flapParam = "";
    for (const r of this.running) {
      const t = now - r.t0;
      for (const [param, keys] of Object.entries(r.clip.keys)) P[param] = Math.max(P[param] ?? 0, sample(keys, t));
      if (r.clip.flap) {
        flapParam = r.clip.flap.param;
        if (now > this.nextFlap) { this.flapOpen = this.flapOpen ? 0 : 1; this.nextFlap = now + r.clip.flap.min + Math.random() * (r.clip.flap.max - r.clip.flap.min); }
      }
      active = true;
    }
    if (flapParam) P[flapParam] = Math.max(P[flapParam] ?? 0, this.flapOpen);
    else this.flapOpen = 0;
    // blink (paused while a clip drives the eyes wide open, e.g. surprise → params.blinkHold)
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
    return active;
  }

  private loop = (now: number) => {
    if (this.disposed) return;
    this.raf = requestAnimationFrame(this.loop);
    if (document.hidden) return;
    const level = this.motion();
    const wind = level === "full" ? this.spec.idle.wind : 0;
    const breath = level === "full" ? this.spec.idle.breath : 0;
    const active = this.update(now);
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
    const scale = Math.min((this.cssH * 0.98) / H, (this.cssW * 0.95) / W);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.useProgram(this.prog);
    gl.uniform2f(u.uBody, W, H);
    gl.uniform2f(u.uCanvas, this.cssW, this.cssH);
    gl.uniform2f(u.uOrigin, 6, this.cssH - H * scale);
    gl.uniform2f(u.uNeck, ...s.pivot.neck);
    gl.uniform2f(u.uChest, ...s.pivot.chest);
    gl.uniform1f(u.uScale, scale);
    gl.uniform1f(u.uT, now / 1000);
    gl.uniform1f(u.uWind, wind);
    gl.uniform1f(u.uBreath, breath);
    gl.uniform1f(u.uHair, s.idle.hairAmp);
    gl.uniform1f(u.uCoat, s.idle.coatAmp);
    gl.uniform1f(u.uBreathAmp, s.idle.breathAmp);
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
    for (const l of this.layers) {
      const v = Math.max(0, P[l.bind] ?? 0);
      draw(l, v, [l.slide[0] * (1 - v), l.slide[1] * (1 - v)]);
    }
    gl.bindVertexArray(null);
  }
}
