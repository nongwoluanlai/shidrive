// 使驾通知弹窗页（toast-* 窗口）：取文案与主题 → 显示 → 点击 / 关闭 / 超时后回报后端
import { invoke } from "@tauri-apps/api/core";
import logo from "../assets/logo.png";
import "./toast.css";

interface ToastInfo {
  title: string;
  body: string;
  timeoutMs: number;
  clickable: boolean;
  theme: Record<string, string>;
}

const root = document.getElementById("sd-toast") as HTMLDivElement;
const bar = root.querySelector(".bar") as HTMLElement;
let done = false;

function finish(action: "click" | "close") {
  if (done) return;
  done = true;
  root.classList.add("out");
  window.setTimeout(() => void invoke("toast_action", { action }).catch(() => window.close()), action === "click" ? 0 : 160);
}

function applyTheme(vars: Record<string, string>) {
  const s = document.documentElement.style;
  for (const [k, v] of Object.entries(vars ?? {})) {
    if (/^--[a-z0-9-]+$/.test(k) && typeof v === "string" && v.length < 120) s.setProperty(k, v);
  }
  if (vars?.preset) document.documentElement.dataset.theme = vars.preset;
}

async function boot() {
  let info: ToastInfo;
  try {
    info = await invoke<ToastInfo>("toast_info");
  } catch {
    finish("close");
    return;
  }
  applyTheme(info.theme);
  (root.querySelector(".logo") as HTMLImageElement).src = logo;
  (root.querySelector(".title") as HTMLElement).textContent = info.title || "ShiDrive";
  (root.querySelector(".body") as HTMLElement).textContent = info.body.trim();
  root.title = info.body.trim();
  root.classList.toggle("clickable", info.clickable);
  root.hidden = false;

  // 倒计时：鼠标悬停时暂停，移开后继续
  const total = Math.max(2000, info.timeoutMs || 8000);
  let left = total;
  let last = performance.now();
  let hover = false;
  root.addEventListener("mouseenter", () => (hover = true));
  root.addEventListener("mouseleave", () => { hover = false; last = performance.now(); });
  const tick = () => {
    if (done) return;
    const now = performance.now();
    if (!hover) left -= now - last;
    last = now;
    bar.style.setProperty("--p", String(Math.max(0, left / total)));
    if (left <= 0) finish("close");
    else requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);

  root.querySelector(".x")!.addEventListener("click", (e) => { e.stopPropagation(); finish("close"); });
  root.addEventListener("click", () => finish(info.clickable ? "click" : "close"));
}

void boot();
