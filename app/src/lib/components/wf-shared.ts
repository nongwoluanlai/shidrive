import type { ScheduleConfig } from "../types";

export function scheduleText(s: ScheduleConfig | null): string {
  if (!s) return "定时";
  switch (s.kind) {
    case "interval":
      return `每 ${s.every_minutes} 分钟`;
    case "daily":
      return `每天 ${s.time}`;
    case "weekly":
      return `每周 ${s.weekdays.map((d) => "一二三四五六日"[d - 1]).join("")} ${s.time}`;
    case "once":
      return `单次 ${s.at}`;
  }
}
