import { api } from "../ipc";

/**
 * 动效强度（用户偏好，跨皮肤保留）：
 * - full：看板待机动画 + 表情 + 全局特效
 * - lite：看板只保留眨眼与表情（无飘动/呼吸），全局特效只保留不闪烁、不震动的叠加层
 * - off ：看板显示静态立绘，不播放任何全局特效
 * 系统开启「减少动态效果」时，full 自动按 lite 处理。
 */
export type MotionLevel = "full" | "lite" | "off";
export const MOTION_LEVELS: MotionLevel[] = ["full", "lite", "off"];

export const skinMotion = $state({ level: "full" as MotionLevel, reduced: false });

const media = typeof matchMedia === "function" ? matchMedia("(prefers-reduced-motion: reduce)") : null;
if (media) {
  skinMotion.reduced = media.matches;
  media.addEventListener?.("change", (e) => { skinMotion.reduced = e.matches; });
}

/** Level after applying the OS reduced-motion preference. */
export function effectiveMotion(): MotionLevel {
  if (skinMotion.level === "full" && skinMotion.reduced) return "lite";
  return skinMotion.level;
}

export function setSkinMotion(level: MotionLevel) {
  if (!MOTION_LEVELS.includes(level)) return;
  skinMotion.level = level;
  void api.settingsSet("ui.skin.motion", level).catch(() => {});
}

export async function loadSkinMotion() {
  const saved = await api.settingsGet("ui.skin.motion").catch(() => null);
  if (saved && MOTION_LEVELS.includes(saved as MotionLevel)) skinMotion.level = saved as MotionLevel;
}
