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

// ---------- schedule input helpers ----------

/** `<input type="datetime-local">` only accepts `YYYY-MM-DDTHH:MM`; stored values use a space (see schedule.rs). */
export function onceToInput(at: string | undefined | null): string {
  const s = (at ?? "").trim();
  if (!s) return "";
  return s.replace(" ", "T").slice(0, 16);
}

/** Canonical stored form `YYYY-MM-DD HH:MM` (the backend accepts both, this keeps local and stored rows identical). */
export function onceFromInput(v: string): string {
  const s = (v ?? "").trim();
  return s ? s.replace("T", " ").slice(0, 16) : "";
}

/** Parse a one-shot time (`YYYY-MM-DD HH:MM[:SS]` or with `T`) as local time; null when invalid. */
export function parseOnce(at: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})(?::(\d{2}))?$/.exec((at ?? "").trim());
  if (!m) return null;
  const [y, mo, d, h, mi, s] = [m[1], m[2], m[3], m[4], m[5], m[6] ?? "0"].map(Number);
  const dt = new Date(y, mo - 1, d, h, mi, s);
  if (dt.getFullYear() !== y || dt.getMonth() !== mo - 1 || dt.getDate() !== d || dt.getHours() !== h || dt.getMinutes() !== mi) return null;
  return dt.getTime();
}

function isHHMM(s: string | undefined): boolean {
  return /^([01]\d|2[0-3]):[0-5]\d(:[0-5]\d)?$/.test((s ?? "").trim());
}

/**
 * Client-side mirror of the backend save validation (schedule.rs / db.update_workflow).
 * Returns the reason a save would be rejected, or null when the workflow can be saved.
 * The backend re-validates; this only exists so the UI can explain *why* an edit is not saved yet.
 */
export function workflowProblem(w: { name: string; enabled: boolean; trigger_type: string; schedule: ScheduleConfig | null }): string | null {
  if (!w.name.trim()) return t("工作流名称不能为空");
  if (w.trigger_type !== "schedule") return null;
  const s = w.schedule;
  if (!s) return t("请配置定时规则");
  switch (s.kind) {
    case "interval":
      return Number.isFinite(s.every_minutes) && s.every_minutes >= 1 ? null : t("间隔分钟数必须 ≥ 1");
    case "daily":
      return isHHMM(s.time) ? null : t("请填写每日执行时间");
    case "weekly":
      if (!s.weekdays?.length) return t("每周定时至少选择一个星期");
      return isHHMM(s.time) ? null : t("请填写每周执行时间");
    case "once": {
      if (!s.at?.trim()) return t("请填写单次执行时间");
      const ts = parseOnce(s.at);
      if (ts === null) return t("单次执行时间格式无效");
      if (w.enabled && ts <= Date.now()) return t("单次执行时间已过去，请重新选择一个未来的时间");
      return null;
    }
  }
}
