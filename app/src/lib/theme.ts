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
