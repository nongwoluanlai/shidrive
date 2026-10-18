// 全局日志（标题栏「日志」面板）：后端 glog.rs 的环形缓冲 + glog://entry 实时推送。
// 前端功能（如定时发送）通过 logGlobal() 写入后端，再经事件回到这里，保证单一来源。
import { api } from "./ipc";
import type { GlobalLogEntry } from "./types";

const MAX = 500;

export const glog = $state({
  items: [] as GlobalLogEntry[],
  /** 面板关闭期间新增的条数 / 其中警告与错误数（徽标） */
  unread: 0,
  unreadAlert: 0,
  open: false,
});

export function pushGlobalLog(e: GlobalLogEntry) {
  if (glog.items.some((x) => x.id === e.id)) return;
  glog.items.push(e);
  if (glog.items.length > MAX) glog.items.splice(0, glog.items.length - MAX);
  if (!glog.open && e.category !== "app") {
    glog.unread++;
    if (e.level === "warn" || e.level === "error") glog.unreadAlert++;
  }
}

export async function loadGlobalLog() {
  const list = await api.globalLogList().catch(() => [] as GlobalLogEntry[]);
  const known = new Set(glog.items.map((x) => x.id));
  const merged = [...list.filter((x) => !known.has(x.id)), ...glog.items].sort((a, b) => a.id - b.id);
  glog.items = merged.slice(-MAX);
  // 启动前（上次运行以来）积累的警告也要能被注意到
  glog.unreadAlert = glog.items.filter((x) => x.level === "warn" || x.level === "error").length;
  glog.unread = glog.items.filter((x) => x.category !== "app").length;
}

export function markGlobalLogRead() {
  glog.unread = 0;
  glog.unreadAlert = 0;
}

export async function clearGlobalLog() {
  await api.globalLogClear().catch(() => {});
  glog.items = [];
  markGlobalLogRead();
}

export function logGlobal(level: "info" | "warn" | "error" | "success", category: string, message: string) {
  void api.globalLogPush(level, category, message).catch(() => {});
}
