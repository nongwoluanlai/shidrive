// Global (full-window) skin effects: declarative timelines built from a small set of built-in
// primitives. Skin packages can only pick primitives and numbers — nothing is executed.
import type { SkinEvent } from "./bus";
import { isSkinEvent } from "./bus";
import type { MotionLevel } from "./motion.svelte";

export type FxName = "shake" | "flash" | "slices" | "rgbSplit" | "hueShift" | "vignette" | "overlayText";
export interface FxStep { at: number; fx: FxName; [param: string]: unknown }
export interface GlobalFxConfig { presets: Record<string, FxStep[]>; on: Partial<Record<SkinEvent, string>> }

const FX_NAMES: FxName[] = ["shake", "flash", "slices", "rgbSplit", "hueShift", "vignette", "overlayText"];
/** Primitives that neither flash nor move the whole UI: the only ones kept at "lite". */
const LITE_SAFE = new Set<FxName>(["overlayText", "vignette"]);
const MAX_PRESETS = 8, MAX_STEPS = 16, MAX_AT = 2000, COOLDOWN_MS = 1200;


const num = (v: unknown, min: number, max: number, def: number) => {
  const n = typeof v === "number" && Number.isFinite(v) ? v : def;
  return Math.min(max, Math.max(min, n));
};
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
  const on: GlobalFxConfig["on"] = {};
  if (r.on && typeof r.on === "object") {
    for (const [event, preset] of Object.entries(r.on as Record<string, unknown>)) {
      if (isSkinEvent(event) && typeof preset === "string" && presets[preset]) on[event] = preset;
    }
  }
  return Object.keys(on).length ? { presets, on } : null;
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
  }

  handle(event: SkinEvent) {
    const preset = this.config.on[event];
    if (preset) this.play(preset);
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
    }
  }
}
