import type { ThemeConfig } from "./types";

/** Choose a readable foreground for filled controls, including user accents. */
export function accentContrast(color: string): string {
  const hex = color.replace("#", "");
  const full = hex.length === 3 ? [...hex].map((c) => c + c).join("") : hex;
  if (!/^[\da-f]{6}$/i.test(full)) return "#ffffff";
  const rgb = [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16) / 255)
    .map((v) => v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
  const luminance = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
  return luminance > 0.179 ? "#101510" : "#ffffff";
}

export function applyTheme(t: ThemeConfig) {
  const html = document.documentElement;
  html.dataset.theme = t.preset === "dark" ? "dark" : "light";
  html.style.colorScheme = t.preset === "dark" ? "dark" : "light";
  const accent = /^#[\da-f]{6}$/i.test(t.accent) ? t.accent : "#4da3ff";
  html.style.setProperty("--accent", accent);
  html.style.setProperty("--accent-contrast", accentContrast(accent));
  html.style.setProperty("--radius", `${Math.min(18, Math.max(0, Number(t.radius) || 0))}px`);
  html.style.setProperty("--font-size", `${Math.min(18, Math.max(12, Number(t.font_size) || 14))}px`);
}

export const ACCENTS = [
  { name: "星蓝", value: "#4da3ff" },
  { name: "青柠", value: "#37c9a0" },
  { name: "橙焰", value: "#ef8f4e" },
  { name: "绛紫", value: "#a58cf2" },
  { name: "玫红", value: "#ef6b9c" },
  { name: "石墨", value: "#8b95a9" },
];

// ---------- 主题色共享：网页端 / 通知弹窗沿用桌面端当前配色 ----------

/** 共享给网页端与通知弹窗的 CSS 变量（取计算值，皮肤覆盖后的颜色也一并带上）。 */
const SHARED_VARS = [
  "--bg", "--bg-panel", "--bg-elev", "--bg-elev2", "--border", "--border-soft",
  "--text", "--text-dim", "--text-faint", "--accent", "--accent-contrast", "--accent-soft",
  "--ok", "--warn", "--danger", "--code-bg", "--scroll", "--radius", "--user-bubble",
];

export function exportThemeVars(): Record<string, string> {
  const cs = getComputedStyle(document.body);
  const out: Record<string, string> = { preset: document.documentElement.dataset.theme === "light" ? "light" : "dark" };
  for (const k of SHARED_VARS) {
    const v = cs.getPropertyValue(k).trim();
    if (v && v.length < 120 && !/url\(|expression|[;{}<>]/i.test(v)) out[k] = v;
  }
  return out;
}

/** 监听主题 / 皮肤变化，去抖后把计算后的配色写入设置 ui.theme_vars（内容未变不写）。 */
export function watchThemeVars(save: (json: string) => Promise<unknown>): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let last = "";
  const flush = () => {
    const json = JSON.stringify(exportThemeVars());
    if (json === last) return;
    last = json;
    void save(json).catch(() => { last = ""; });
  };
  const schedule = () => { clearTimeout(timer); timer = setTimeout(flush, 800); };
  const mo = new MutationObserver(schedule);
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ["style", "data-theme", "class"] });
  mo.observe(document.body, { attributes: true, attributeFilter: ["style", "data-skin", "class"] });
  mo.observe(document.head, { childList: true, subtree: true, characterData: true });
  timer = setTimeout(flush, 1500);
  return () => { mo.disconnect(); clearTimeout(timer); };
}
