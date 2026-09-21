import { api } from "./ipc";
import { accentContrast } from "./theme";

export interface SkinManifest {
  id: string;
  name: string;
  dir: string;
  background?: string;
  character?: string;
  vars?: Record<string, string>;
  css?: string;
}
export const builtinSkins = [
  { id: "steins-gate", name: "命运石之门", en: "Steins;Gate", description: "琥珀暖光 · 复古实验室", descriptionEn: "Amber lab light · retro terminal", preview: "/skins/steins-gate/bg.png" },
  { id: "hell", name: "地狱乐", en: "Hell’s Paradise", description: "深红和风 · 山林薄雾", descriptionEn: "Crimson lacquer · mountain mist", preview: "/skins/hell/bg.png" },
];
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

function clearSkin() {
  for (const key of managed) document.body.style.removeProperty(key);
  managed.clear();
  document.getElementById("skin-custom-css")?.remove();
  delete document.body.dataset.skin;
  delete document.body.dataset.skinCharacter;
  // Keep --skin-opacity: the user preference outlives skin switches.
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
        const [background, character] = await Promise.all([
          manifest.background ? api.skinAssetData(manifest.dir, manifest.background) : "",
          manifest.character ? api.skinAssetData(manifest.dir, manifest.character) : "",
        ]);
        if (version !== generation) return;
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
        document.body.dataset.skin = id;
      } catch (error) {
        if (version === generation) { clearSkin(); onError(String(error)); }
      }
    })();
  }
  return () => { if (version === generation) { generation++; clearSkin(); } };
}
