// Global (full-window) skin effects: declarative timelines built from a small set of built-in
// primitives. Skin packages can only pick primitives and numbers — nothing is executed.
import type { SkinEvent } from "./bus";
import { isSkinEvent } from "./bus";
import type { MotionLevel } from "./motion.svelte";

export type FxName = "shake" | "flash" | "slices" | "rgbSplit" | "hueShift" | "vignette" | "overlayText"
  | "divergence" | "scanlines" | "noise" | "tint" | "ring" | "particles";
export interface FxStep { at: number; fx: FxName; [param: string]: unknown }
export interface GlobalFxConfig {
  presets: Record<string, FxStep[]>;
  /** event → preset name(s); several = random pick */
  on: Partial<Record<SkinEvent, string[]>>;
  /** occasional background effect while the app is idle */
  ambient: { every: [number, number]; play: string[] } | null;
}

const FX_NAMES: FxName[] = ["shake", "flash", "slices", "rgbSplit", "hueShift", "vignette", "overlayText", "divergence", "scanlines", "noise", "tint", "ring", "particles"];
/** Primitives that neither flash nor move the whole UI: the only ones kept at "lite". */
const LITE_SAFE = new Set<FxName>(["overlayText", "vignette", "divergence", "tint"]);
const BLENDS = ["normal", "multiply", "screen", "overlay", "color", "soft-light", "hue", "saturation"];
const MAX_PRESETS = 12, MAX_STEPS = 24, MAX_AT = 3000, COOLDOWN_MS = 1200;


const num = (v: unknown, min: number, max: number, def: number) => {
  const n = typeof v === "number" && Number.isFinite(v) ? v : def;
  return Math.min(max, Math.max(min, n));
};
const pick = <T,>(list: T[]) => list[Math.floor(Math.random() * list.length)];
const hex = (v: unknown, def: string) => (typeof v === "string" && /^#[\da-f]{3}([\da-f]{3})?$/i.test(v) ? v : def);

function sanitizeStep(raw: unknown): FxStep | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const fx = r.fx as FxName;
  if (!FX_NAMES.includes(fx)) return null;
  const at = num(r.at, 0, MAX_AT, 0);
  switch (fx) {
    case "shake": return { at, fx, px: num(r.px, 1, 20, 8), dur: num(r.dur, 100, 1500, 400), steps: r.steps === true };
    case "flash": return { at, fx, color: hex(r.color, "#ffffff"), opacity: num(r.opacity, 0.05, 0.6, 0.35), dur: num(r.dur, 80, 800, 250) };
    case "slices": return { at, fx, n: Math.round(num(r.n, 1, 14, 8)), dur: num(r.dur, 200, 1500, 900), color: hex(r.color, "#ffb347") };
    case "rgbSplit": return { at, fx, px: num(r.px, 1, 12, 6), dur: num(r.dur, 100, 1500, 600) };
    case "hueShift": return { at, fx, deg: num(r.deg, -180, 180, 40), dur: num(r.dur, 100, 1500, 500) };
    case "vignette": return { at, fx, color: hex(r.color, "#000000"), opacity: num(r.opacity, 0.1, 0.8, 0.5), dur: num(r.dur, 200, 3000, 900) };
    case "overlayText": {
      const text = typeof r.text === "string" ? r.text.replace(/[\u0000-\u001f]/g, "").slice(0, 24) : "";
      return { at, fx, style: r.style === "plain" ? "plain" : "nixie", text, roll: num(r.roll, 0, 1500, 1000), dur: num(r.dur, 400, 3000, 2400), color: hex(r.color, "#ffb347") };
    }
    case "divergence": {
      // nixie-tube meter: digits roll, then lock one tube at a time from left to right
      const values = (Array.isArray(r.values) ? r.values : []).filter((v): v is string => typeof v === "string" && /^[0-9.]{1,12}$/.test(v)).slice(0, 16);
      return { at, fx, values, color: hex(r.color, "#ff9a3c"), roll: num(r.roll, 0, 2500, 900), lock: num(r.lock, 0, 400, 90), dur: num(r.dur, 600, 4000, 2600), y: num(r.y, 0.05, 0.9, 0.16), size: num(r.size, 20, 72, 40) };
    }
    case "scanlines": return { at, fx, color: hex(r.color, "#000000"), opacity: num(r.opacity, 0.05, 0.6, 0.25), gap: num(r.gap, 2, 8, 3), dur: num(r.dur, 200, 3000, 900) };
    case "noise": return { at, fx, opacity: num(r.opacity, 0.03, 0.4, 0.12), dur: num(r.dur, 150, 3000, 700) };
    case "tint": return { at, fx, color: hex(r.color, "#ff9a3c"), opacity: num(r.opacity, 0.05, 0.6, 0.2), blend: BLENDS.includes(r.blend as string) ? r.blend : "color", dur: num(r.dur, 200, 3000, 900) };
    case "ring": return { at, fx, color: hex(r.color, "#ffb347"), x: num(r.x, 0, 1, 0.5), y: num(r.y, 0, 1, 0.5), size: num(r.size, 0.1, 2, 0.9), width: num(r.width, 1, 12, 3), dur: num(r.dur, 200, 2000, 800) };
    case "particles": {
      const shape = ["dot", "spark", "digit"].includes(r.shape as string) ? r.shape : "dot";
      return { at, fx, color: hex(r.color, "#ffb347"), n: Math.round(num(r.n, 1, 40, 18)), shape, dir: r.dir === "down" ? "down" : "up", dur: num(r.dur, 400, 3000, 1600) };
    }
  }
}

/** Validate an untrusted `globalFx` manifest block (and merge built-in presets). */
export function sanitizeGlobalFx(raw: unknown): GlobalFxConfig | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  // 引擎只提供原语；所有预设（包括"世界线"）都由皮肤包自己在 skin.json 里定义。
  const presets: Record<string, FxStep[]> = {};
  if (r.presets && typeof r.presets === "object") {
    for (const [name, steps] of Object.entries(r.presets as Record<string, unknown>).slice(0, MAX_PRESETS)) {
      if (!/^[a-z][a-z0-9_-]{0,31}$/.test(name) || !Array.isArray(steps)) continue;
      const clean = steps.slice(0, MAX_STEPS).map(sanitizeStep).filter((s): s is FxStep => !!s);
      if (clean.length) presets[name] = clean;
    }
  }
  const names = (v: unknown) => (Array.isArray(v) ? v : [v]).filter((n): n is string => typeof n === "string" && !!presets[n]).slice(0, 8);
  const on: GlobalFxConfig["on"] = {};
  if (r.on && typeof r.on === "object") {
    for (const [event, value] of Object.entries(r.on as Record<string, unknown>)) {
      const list = names(value);
      if (isSkinEvent(event) && list.length) on[event] = list;
    }
  }
  const amb = r.ambient as Record<string, unknown> | undefined;
  const ambPlay = amb ? names(amb.play) : [];
  const every = Array.isArray(amb?.every) ? [num(amb.every[0], 20, 3600, 90), num(amb.every[1], 20, 3600, 240)] as [number, number] : [90, 240] as [number, number];
  const ambient = ambPlay.length ? { every: [Math.min(...every), Math.max(...every)] as [number, number], play: ambPlay } : null;
  return Object.keys(on).length || ambient ? { presets, on, ambient } : null;
}

/** Runs timelines against the live window. One instance per active skin. */
export class GlobalFx {
  private layer: HTMLDivElement;
  private timers = new Set<number>();
  private intervals = new Set<number>();
  private anims = new Set<Animation>();
  private lastPlayed = new Map<string, number>();
  private flashes: number[] = [];

  constructor(private config: GlobalFxConfig, private motion: () => MotionLevel) {
    this.layer = document.createElement("div");
    this.layer.className = "skin-gfx-layer";
    this.layer.setAttribute("aria-hidden", "true");
    Object.assign(this.layer.style, { position: "fixed", inset: "0", zIndex: "350", pointerEvents: "none", overflow: "hidden" });
    document.body.appendChild(this.layer);
    this.scheduleAmbient();
  }

  handle(event: SkinEvent) {
    const list = this.config.on[event];
    if (list?.length) this.play(pick(list));
  }

  private ambientTimer = 0;
  private scheduleAmbient() {
    const a = this.config.ambient;
    if (!a) return;
    const [lo, hi] = a.every;
    this.ambientTimer = window.setTimeout(() => {
      if (document.hasFocus() && !document.hidden) this.play(pick(a.play));
      this.scheduleAmbient();
    }, (lo + Math.random() * (hi - lo)) * 1000);
  }

  play(name: string) {
    const steps = this.config.presets[name];
    const level = this.motion();
    if (!steps || level === "off" || document.hidden) return;
    const now = performance.now();
    if (now - (this.lastPlayed.get(name) ?? -Infinity) < COOLDOWN_MS) return;
    this.lastPlayed.set(name, now);
    for (const step of steps) {
      if (level === "lite" && !LITE_SAFE.has(step.fx)) continue;
      const id = window.setTimeout(() => { this.timers.delete(id); this.run(step); }, step.at);
      this.timers.add(id);
    }
  }

  dispose() {
    clearTimeout(this.ambientTimer);
    for (const id of this.timers) clearTimeout(id);
    for (const id of this.intervals) clearInterval(id);
    for (const a of this.anims) a.cancel();
    this.timers.clear(); this.intervals.clear(); this.anims.clear();
    this.layer.remove();
  }

  private track(anim: Animation, cleanup?: () => void) {
    this.anims.add(anim);
    const done = () => { this.anims.delete(anim); cleanup?.(); };
    anim.onfinish = done; anim.oncancel = done;
  }
  private root() { return document.querySelector<HTMLElement>(".root"); }
  private overlay(css: Partial<CSSStyleDeclaration>) {
    const el = document.createElement("div");
    Object.assign(el.style, { position: "absolute", inset: "0", pointerEvents: "none" }, css);
    this.layer.appendChild(el);
    return el;
  }

  private run(s: FxStep) {
    const R = (a: number, b: number) => a + Math.random() * (b - a);
    const dur = Number(s.dur);
    switch (s.fx) {
      case "shake": {
        const root = this.root(); if (!root) return;
        const p = Number(s.px);
        const frames: Keyframe[] = Array.from({ length: 10 }, (_, i) => ({ transform: `translate(${R(-p, p) * (1 - i / 10)}px, ${R(-p, p) * (1 - i / 10)}px)` }));
        frames.push({ transform: "none" });
        this.track(root.animate(frames, { duration: dur, easing: s.steps ? "steps(1)" : "linear" }));
        break;
      }
      case "flash": {
        // photosensitivity guard: at most 3 flashes per second
        const now = performance.now();
        this.flashes = this.flashes.filter((t) => now - t < 1000);
        if (this.flashes.length >= 3) return;
        this.flashes.push(now);
        const el = this.overlay({ background: String(s.color) });
        this.track(el.animate([{ opacity: Number(s.opacity) }, { opacity: 0 }], { duration: dur, fill: "forwards" }), () => el.remove());
        break;
      }
      case "slices": {
        for (let i = 0; i < Number(s.n); i++) {
          const el = this.overlay({ inset: "auto", left: "0", right: "0", top: `${R(0, 100)}%`, height: `${R(8, 50)}px`, background: `${s.color}22`, borderTop: `1px solid ${s.color}66`, opacity: "0" });
          const frames: Keyframe[] = Array.from({ length: 8 }, () => ({ opacity: Math.random() > 0.4 ? 1 : 0, transform: `translateX(${R(-60, 60)}px)`, backdropFilter: `hue-rotate(${R(0, 180)}deg) invert(${Math.random() > 0.7 ? 1 : 0})` }));
          frames.push({ opacity: 0 });
          this.track(el.animate(frames, { duration: dur, easing: "steps(1)", fill: "forwards" }), () => el.remove());
        }
        break;
      }
      case "rgbSplit": {
        const root = this.root(); if (!root) return;
        const p = Number(s.px);
        const frames: Keyframe[] = Array.from({ length: 10 }, () => ({ filter: `drop-shadow(${R(-p, p)}px 0 0 #f0f8) drop-shadow(${R(-p, p)}px 0 0 #0ff8)` }));
        frames.push({ filter: "none" });
        this.track(root.animate(frames, { duration: dur, easing: "steps(1)" }));
        break;
      }
      case "hueShift": {
        const root = this.root(); if (!root) return;
        this.track(root.animate([{ filter: "none" }, { filter: `hue-rotate(${s.deg}deg)` }, { filter: "none" }], { duration: dur }));
        break;
      }
      case "vignette": {
        const el = this.overlay({ background: `radial-gradient(ellipse at center, transparent 45%, ${s.color} 100%)`, opacity: "0" });
        this.track(el.animate([{ opacity: 0 }, { opacity: Number(s.opacity), offset: 0.3 }, { opacity: 0 }], { duration: dur, fill: "forwards" }), () => el.remove());
        break;
      }
      case "overlayText": {
        const nixie = s.style === "nixie";
        const color = String(s.color);
        const el = this.overlay({
          inset: "auto", left: "50%", top: "18%", transform: "translateX(-50%)", padding: "4px 16px", borderRadius: "6px",
          font: nixie ? "700 44px var(--mono, monospace)" : "700 28px var(--sans, sans-serif)", letterSpacing: nixie ? "6px" : "1px", color,
          textShadow: `0 0 6px ${color}, 0 0 18px ${color}, 0 0 40px ${color}`,
          background: nixie ? "#000a" : "transparent", border: nixie ? `1px solid ${color}66` : "none",
        });
        const final = String(s.text || "") || (Math.random() * 0.5 + 0.9).toFixed(6);
        const roll = Number(s.roll);
        const started = performance.now();
        el.textContent = final;
        if (roll > 0) {
          const id = window.setInterval(() => {
            if (performance.now() - started >= roll) { el.textContent = final; clearInterval(id); this.intervals.delete(id); return; }
            el.textContent = nixie ? (Math.random() * 2).toFixed(6) : final.split("").sort(() => Math.random() - 0.5).join("");
          }, 60);
          this.intervals.add(id);
        }
        this.track(el.animate([{ opacity: 1 }, { opacity: 1, offset: 0.75 }, { opacity: 0 }], { duration: dur, fill: "forwards" }), () => el.remove());
        break;
      }
      case "divergence": {
        const color = String(s.color), size = Number(s.size);
        const values = s.values as string[];
        const final = values.length ? pick(values) : (Math.random() < 0.5 ? "0." : "1.") + String(Math.floor(Math.random() * 1e6)).padStart(6, "0");
        const box = this.overlay({ inset: "auto", left: "50%", top: `${Number(s.y) * 100}%`, transform: "translateX(-50%)", display: "flex", gap: `${size * 0.12}px`, padding: `${size * 0.18}px ${size * 0.3}px`, background: "#050302d9", border: `1px solid ${color}55`, borderRadius: "6px", boxShadow: `0 0 24px ${color}33, inset 0 0 18px #000` });
        const tubes = final.split("").map((ch) => {
          const t = document.createElement("span");
          Object.assign(t.style, {
            display: "inline-block", width: ch === "." ? `${size * 0.28}px` : `${size * 0.62}px`, height: `${size * 1.25}px`, lineHeight: `${size * 1.25}px`, textAlign: "center",
            font: `400 ${size}px var(--mono, monospace)`, color, textShadow: `0 0 3px ${color}, 0 0 10px ${color}aa`,
            background: ch === "." ? "transparent" : "linear-gradient(#1a0f08, #0b0604)", borderRadius: `${size * 0.3}px ${size * 0.3}px ${size * 0.08}px ${size * 0.08}px`,
            boxShadow: ch === "." ? "none" : `inset 0 0 ${size * 0.25}px #000, 0 0 ${size * 0.3}px ${color}22`,
          });
          t.textContent = ch;
          box.appendChild(t);
          return t;
        });
        const roll = Number(s.roll), lock = Number(s.lock), started = performance.now();
        const id = window.setInterval(() => {
          const el = performance.now() - started;
          let busy = false;
          tubes.forEach((t, i) => {
            if (final[i] === ".") return;
            if (el < roll + i * lock) { t.textContent = String(Math.floor(Math.random() * 10)); busy = true; } else t.textContent = final[i];
          });
          if (!busy) { clearInterval(id); this.intervals.delete(id); }
        }, 55);
        this.intervals.add(id);
        this.track(box.animate([{ opacity: 0 }, { opacity: 1, offset: 0.06 }, { opacity: 1, offset: 0.8 }, { opacity: 0 }], { duration: dur, fill: "forwards" }), () => box.remove());
        break;
      }
      case "scanlines": {
        const gap = Number(s.gap);
        // drift via transform (compositor-only); animating background-position repaints the whole window
        const el = this.overlay({ inset: "auto", left: "0", right: "0", top: `${-gap * 20}px`, height: `calc(100% + ${gap * 40}px)`, background: `repeating-linear-gradient(0deg, ${s.color} 0 1px, transparent 1px ${gap}px)`, opacity: "0", willChange: "transform, opacity" });
        this.track(el.animate([{ opacity: 0, transform: "translateY(0)" }, { opacity: Number(s.opacity), offset: 0.2 }, { opacity: 0, transform: `translateY(${gap * 20}px)` }], { duration: dur, fill: "forwards" }), () => el.remove());
        break;
      }
      case "noise": {
        const el = this.overlay({ backgroundImage: `url(${noiseTile()})`, opacity: "0" });
        const frames: Keyframe[] = Array.from({ length: 8 }, (_, i) => ({ opacity: Number(s.opacity) * (i < 7 ? 1 : 0), backgroundPosition: `${Math.floor(R(0, 128))}px ${Math.floor(R(0, 128))}px` }));
        this.track(el.animate(frames, { duration: dur, easing: "steps(1)", fill: "forwards" }), () => el.remove());
        break;
      }
      case "tint": {
        const el = this.overlay({ background: String(s.color), mixBlendMode: String(s.blend) as any, opacity: "0" });
        this.track(el.animate([{ opacity: 0 }, { opacity: Number(s.opacity), offset: 0.25 }, { opacity: Number(s.opacity), offset: 0.6 }, { opacity: 0 }], { duration: dur, fill: "forwards" }), () => el.remove());
        break;
      }
      case "ring": {
        const d = Math.max(innerWidth, innerHeight) * Number(s.size);
        const el = this.overlay({ inset: "auto", left: `${Number(s.x) * 100}%`, top: `${Number(s.y) * 100}%`, width: `${d}px`, height: `${d}px`, marginLeft: `${-d / 2}px`, marginTop: `${-d / 2}px`, borderRadius: "50%", border: `${s.width}px solid ${s.color}`, boxShadow: `0 0 8px ${s.color}`, willChange: "transform, opacity" });
        this.track(el.animate([{ transform: "scale(0.05)", opacity: 0.9 }, { transform: "scale(1)", opacity: 0 }], { duration: dur, easing: "cubic-bezier(.2,.7,.3,1)", fill: "forwards" }), () => el.remove());
        break;
      }
      case "particles": {
        const n = Number(s.n), color = String(s.color), up = s.dir !== "down";
        for (let i = 0; i < n; i++) {
          const size = s.shape === "digit" ? R(12, 20) : R(2, 5);
          const el = this.overlay({ inset: "auto", left: `${R(0, 100)}%`, top: up ? "100%" : "-4%", width: s.shape === "digit" ? "auto" : `${size}px`, height: s.shape === "spark" ? `${size * 4}px` : s.shape === "digit" ? "auto" : `${size}px`,
            borderRadius: s.shape === "dot" ? "50%" : "2px", background: s.shape === "digit" ? "transparent" : color, color, font: `${size}px var(--mono, monospace)`, boxShadow: s.shape === "digit" ? "none" : `0 0 6px ${color}`, textShadow: s.shape === "digit" ? `0 0 4px ${color}` : "none", willChange: "transform, opacity" });
          if (s.shape === "digit") el.textContent = String(Math.floor(Math.random() * 10));
          const dy = (up ? -1 : 1) * innerHeight * R(0.35, 1.05), dx = R(-40, 40), d = dur * R(0.6, 1);
          this.track(el.animate([{ transform: "translate(0,0)", opacity: 0 }, { opacity: 1, offset: 0.15 }, { transform: `translate(${dx}px, ${dy}px)`, opacity: 0 }], { duration: d, delay: R(0, dur * 0.35), easing: "ease-out", fill: "both" }), () => el.remove());
        }
        break;
      }
    }
  }
}

let noiseUrl = "";
/** A 128px grain tile generated once (no network, no package assets). */
function noiseTile(): string {
  if (noiseUrl) return noiseUrl;
  const c = document.createElement("canvas");
  c.width = c.height = 128;
  const ctx = c.getContext("2d");
  if (!ctx) return "";
  const img = ctx.createImageData(128, 128);
  for (let i = 0; i < img.data.length; i += 4) { const v = Math.random() * 255; img.data[i] = img.data[i + 1] = img.data[i + 2] = v; img.data[i + 3] = 255; }
  ctx.putImageData(img, 0, 0);
  return (noiseUrl = c.toDataURL());
}
