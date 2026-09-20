import type { ThemeConfig } from "./types";

export function applyTheme(t: ThemeConfig) {
  const html = document.documentElement;
  html.dataset.theme = t.preset === "light" ? "light" : "dark";
  html.style.setProperty("--accent", t.accent);
  html.style.setProperty("--radius", `${t.radius}px`);
  html.style.setProperty("--font-size", `${t.font_size}px`);
}

export const ACCENTS = [
  { name: "星蓝", value: "#4da3ff" },
  { name: "青柠", value: "#37c9a0" },
  { name: "橙焰", value: "#ef8f4e" },
  { name: "绛紫", value: "#a58cf2" },
  { name: "玫红", value: "#ef6b9c" },
  { name: "石墨", value: "#8b95a9" },
];
