import { api } from "./ipc";
import { accentContrast } from "./theme";
import { onSkinEvent } from "./skin-fx/bus";
import { GlobalFx, sanitizeGlobalFx, type GlobalFxConfig } from "./skin-fx/global-fx";
import { effectiveMotion } from "./skin-fx/motion.svelte";
import { sanitizeScene, type SceneSpec } from "./skin-fx/scene";

export interface SkinManifest {
  id: string;
  name: string;
  dir: string;
  background?: string;
  character?: string;
  vars?: Record<string, string>;
  css?: string;
  /** Layered character scene (dynamic board); see docs/skin-guide.md「动态看板」. */
  scene?: unknown;
  /** Full-window effect timelines bound to app events. */
  globalFx?: unknown;
}
export const builtinSkins = [
  { id: "steins-gate", name: "命运石之门", en: "Steins;Gate", description: "琥珀暖光 · 复古实验室", descriptionEn: "Amber lab light · retro terminal", preview: "/skins/steins-gate/bg.png" },
  { id: "hell", name: "地狱乐", en: "Hell’s Paradise", description: "深红和风 · 山林薄雾", descriptionEn: "Crimson lacquer · mountain mist", preview: "/skins/hell/bg.png" },
];

/**
 * Dynamic board of the active skin. `SkinCharacter` renders it (WebGL) and falls back to the
 * static character when it is null, when motion is off, or when rendering fails.
 */
export const dynamicSkin = $state({
  id: "",
  scene: null as SceneSpec | null,
  urls: null as { body: string; mask: string; layers: string[] } | null,
});

let globalFx: GlobalFx | null = null;
onSkinEvent((event) => globalFx?.handle(event));
function startGlobalFx(config: GlobalFxConfig | null) {
  globalFx?.dispose();
  globalFx = config ? new GlobalFx(config, effectiveMotion) : null;
}

const colorVars = new Set(["--bg", "--bg-panel", "--bg-elev", "--bg-elev2", "--text", "--text-dim", "--text-faint", "--accent", "--accent-contrast", "--accent-soft", "--border", "--border-soft", "--code-bg", "--ok", "--warn", "--danger", "--scroll"]);
const managed = new Set<string>();
let generation = 0;

/** Whole-skin opacity (0.3–1). Applied to every themed surface via body style. */
export const skinOpacity = $state({ value: 1 });

export function setSkinOpacity(value: number) {
  const clamped = Math.min(1, Math.max(0.3, value || 1));
  skinOpacity.value = clamped;
  document.body.style.setProperty("--skin-opacity", String(clamped));
  void api.settingsSet("ui.skin.opacity", String(clamped)).catch(() => {});
}

export async function loadSkinOpacity() {
  const saved = await api.settingsGet("ui.skin.opacity").catch(() => null);
  const value = saved === null ? NaN : Number(saved);
  if (!Number.isNaN(value)) setSkinOpacity(Math.min(1, Math.max(0.3, value)));
  else document.body.style.setProperty("--skin-opacity", String(skinOpacity.value));
}

export async function listSkins(): Promise<SkinManifest[]> {
  const list = await api.skinsList();
  return list.filter((v) => typeof v.id === "string" && /^[a-z0-9][a-z0-9_-]{0,63}$/.test(v.id) && !["none", ...builtinSkins.map((s) => s.id)].includes(v.id))
    .map((v) => ({ ...v, id: String(v.id), name: String(v.name || v.id), dir: String(v.dir || v.id) }) as SkinManifest);
}

/** blob: URLs created for the active custom skin; revoked on every switch so the bitmaps are freed. */
const objectUrls: string[] = [];
function clearSkin() {
  for (const key of managed) document.body.style.removeProperty(key);
  managed.clear();
  document.getElementById("skin-custom-css")?.remove();
  delete document.body.dataset.skin;
  delete document.body.dataset.skinCharacter;
  startGlobalFx(null);
  dynamicSkin.id = ""; dynamicSkin.scene = null; dynamicSkin.urls = null;
  for (const url of objectUrls.splice(0)) URL.revokeObjectURL(url);
  // Keep --skin-opacity: the user preference outlives skin switches.
}

const IMAGE_MIME: Record<string, string> = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", webp: "image/webp", gif: "image/gif" };
/**
 * Load a declared skin image as a `blob:` URL. The previous implementation inlined the file as a
 * base64 `data:` URL inside a CSS custom property; Chromium (WebView2 included) treats any URL longer
 * than 2 MiB (`url::kMaxURLChars`) as invalid, so every image above ~1.5 MB imported successfully but
 * never rendered. Raw bytes + object URLs have no such limit and skip the base64 blow-up.
 */
async function assetUrl(dir: string, file: string): Promise<string> {
  const bytes = await api.skinAssetRead(dir, file);
  const type = IMAGE_MIME[file.split(".").pop()?.toLowerCase() ?? ""] ?? "application/octet-stream";
  const url = URL.createObjectURL(new Blob([bytes], { type }));
  objectUrls.push(url);
  return url;
}
function setToken(key: string, value: string) {
  document.body.style.setProperty(key, value);
  managed.add(key);
}

/** Packs may style controls, but cannot fetch resources, escape the skin scope or hide UI. */
export function scopeSkinCss(css: string, id: string): string {
  if (css.length > 65536 || /[\\@]|url\s*\(|expression\s*\(|image-set\s*\(/i.test(css)) throw new Error("Skin CSS: external resources, escapes and at-rules are not supported");
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(css);
  const allowed = /^(color|background(-color|-image|-size|-position|-repeat)?|border(-[a-z-]+)?|box-shadow|text-shadow|font(-family|-size|-weight|-style)?|letter-spacing|line-height|text-transform|text-decoration(-[a-z-]+)?|outline(-[a-z-]+)?|fill|stroke(-width)?|accent-color)$/;
  const prefix = `body[data-skin="${id}"]`;
  return Array.from(sheet.cssRules).map((rule) => {
    if (!(rule instanceof CSSStyleRule)) throw new Error("Skin CSS: only style rules are supported");
    const selectors = rule.selectorText.split(",").map((s) => {
      s = s.trim();
      // Root styling is handled through manifest vars, not global selectors.
      if (!s || /\b(html|body)\b|:root|:has\(|\||[{}]/i.test(s)) throw new Error("Skin CSS: use component selectors such as .btn");
      return `${prefix} ${s}`;
    });
    const properties = Array.from(rule.style).filter((key) => allowed.test(key)).map((key) => `${key}:${rule.style.getPropertyValue(key)};`).join("");
    return `${selectors.join(",")} {${properties}}`;
  }).join("\n");
}

/** The effect owns cleanup. Generation checks prevent slow assets reviving an old skin. */
export function activateSkin(id: string, onError: (message: string) => void): () => void {
  const version = ++generation;
  clearSkin();
  if (!id) return () => {};
  const builtin = builtinSkins.find((s) => s.id === id);
  if (builtin) {
    document.body.dataset.skin = id;
    document.body.dataset.skinCharacter = "true";
  } else {
    void (async () => {
      try {
        const manifest = (await listSkins()).find((s) => s.id === id);
        if (!manifest) throw new Error("Skin is no longer installed");
        const css = manifest.css ? scopeSkinCss(manifest.css, id) : "";
        const scene = manifest.scene === undefined ? null : sanitizeScene(manifest.scene);
        if (manifest.scene !== undefined && !scene) console.warn("[skin] scene ignored: invalid description");
        const fx = manifest.globalFx === undefined ? null : sanitizeGlobalFx(manifest.globalFx);
        const [background, character, sceneUrls] = await Promise.all([
          manifest.background ? assetUrl(manifest.dir, manifest.background) : "",
          manifest.character ? assetUrl(manifest.dir, manifest.character) : "",
          scene ? Promise.all([
            assetUrl(manifest.dir, scene.body.src),
            assetUrl(manifest.dir, scene.mask),
            Promise.all(scene.layers.map((l) => assetUrl(manifest.dir, l.src))),
          ]) : null,
        ]);
        if (version !== generation) {
          // a newer activation won: free the bitmaps we just created
          for (const url of [background, character, ...(sceneUrls ? [sceneUrls[0], sceneUrls[1], ...sceneUrls[2]] : [])]) {
            if (!url) continue;
            URL.revokeObjectURL(url);
            const i = objectUrls.indexOf(url);
            if (i >= 0) objectUrls.splice(i, 1);
          }
          return;
        }
        for (const [key, value] of Object.entries(manifest.vars ?? {})) {
          if (typeof value !== "string") continue;
          if (colorVars.has(key) && /^#[\da-f]{3}([\da-f]{3})?$/i.test(value)) setToken(key, value);
          if (key === "--radius" && /^\d{1,2}px$/.test(value)) setToken(key, `${Math.min(18, parseInt(value))}px`);
        }
        if (manifest.vars?.["--accent"] && !manifest.vars?.["--accent-contrast"]) setToken("--accent-contrast", accentContrast(manifest.vars["--accent"]));
        if (background) setToken("--skin-bg", `url("${background}")`);
        if (character) { setToken("--skin-character", `url("${character}")`); document.body.dataset.skinCharacter = "true"; }
        if (css) {
          const style = document.createElement("style");
          style.id = "skin-custom-css";
          style.textContent = css;
          document.head.appendChild(style);
        }
        if (scene && sceneUrls) {
          dynamicSkin.urls = { body: sceneUrls[0], mask: sceneUrls[1], layers: sceneUrls[2] };
          dynamicSkin.scene = scene;
          dynamicSkin.id = id;
          document.body.dataset.skinCharacter = "true";
        }
        startGlobalFx(fx);
        document.body.dataset.skin = id;
      } catch (error) {
        if (version === generation) { clearSkin(); onError(String(error)); }
      }
    })();
  }
  return () => { if (version === generation) { generation++; clearSkin(); } };
}
