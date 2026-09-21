import type { ScheduleConfig } from "../types";
import { t } from "../i18n";

export function scheduleText(s: ScheduleConfig | null): string {
  if (!s) return t("定时");
  switch (s.kind) {
    case "interval":
      return t("每 {minutes} 分钟", { minutes: s.every_minutes });
    case "daily":
      return t("每天 {time}", { time: s.time });
    case "weekly": {
      const weekdays = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
      return t("每周 {days} {time}", { days: s.weekdays.map((d) => t(weekdays[d - 1] ?? String(d))).join(", "), time: s.time });
    }
    case "once":
      return t("单次 {at}", { at: s.at });
  }
}
