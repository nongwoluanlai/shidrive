// 聊天输入框聚焦时，左下角人物与聊天区重叠的部分变透明（失焦恢复），免得挡住正在看的消息。
// hole 是舞台坐标系（相对 .skin-character-stage 左上角）下的重叠矩形；client 是同一块的视口坐标，
// 供点击判定用：透明区里的点击属于聊天区，不算「戳人物」。

export interface Rect { x: number; y: number; w: number; h: number }

export const see = $state({
  active: false,
  hole: null as Rect | null,
  client: null as Rect | null,
});

const CHAT_INPUT = ".chat .input-row textarea";

function measure() {
  const stage = document.querySelector(".skin-character-stage")?.getBoundingClientRect();
  const chat = document.querySelector(".chat")?.getBoundingClientRect();
  if (!stage || !chat || stage.width < 2 || stage.height < 2) { see.hole = see.client = null; return; }
  const x1 = Math.max(stage.left, chat.left), y1 = Math.max(stage.top, chat.top);
  const x2 = Math.min(stage.right, chat.right), y2 = Math.min(stage.bottom, chat.bottom);
  if (x2 - x1 < 4 || y2 - y1 < 4) { see.hole = see.client = null; return; }
  see.client = { x: x1, y: y1, w: x2 - x1, h: y2 - y1 };
  see.hole = { x: Math.round(x1 - stage.left), y: Math.round(y1 - stage.top), w: Math.round(x2 - x1), h: Math.round(y2 - y1) };
}

/** 点（视口坐标）是否落在当前生效的透明区里 */
export function inSeeThrough(x: number, y: number): boolean {
  const c = see.client;
  return see.active && !!c && x >= c.x && x < c.x + c.w && y >= c.y && y < c.y + c.h;
}

const isChatInput = (el: EventTarget | null) => el instanceof HTMLTextAreaElement && el.matches(CHAT_INPUT);

/** 监听聊天输入框焦点；返回取消函数 */
export function watchSeeThrough(): () => void {
  const onIn = (e: FocusEvent) => {
    if (!isChatInput(e.target)) return;
    measure();
    see.active = true;
  };
  const onOut = (e: FocusEvent) => {
    if (!isChatInput(e.target)) return;
    see.active = false;
  };
  const onResize = () => { if (see.active) measure(); };
  document.addEventListener("focusin", onIn);
  document.addEventListener("focusout", onOut);
  addEventListener("resize", onResize);
  if (isChatInput(document.activeElement)) { measure(); see.active = true; }
  return () => {
    document.removeEventListener("focusin", onIn);
    document.removeEventListener("focusout", onOut);
    removeEventListener("resize", onResize);
    see.active = false;
  };
}

/** 舞台的 mask：重叠区用 --see 透明度，其余四条带保持不透明 */
export function seeMask(hole: Rect, W: number, H: number): string {
  const { x, y, w, h } = hole;
  const solid = "linear-gradient(#000,#000)";
  const layers: [string, number, number, number, number][] = [
    ["linear-gradient(rgba(0,0,0,var(--see)),rgba(0,0,0,var(--see)))", x, y, w, h],
    [solid, 0, 0, x, H],
    [solid, x + w, 0, Math.max(0, W - x - w), H],
    [solid, x, 0, w, y],
    [solid, x, y + h, w, Math.max(0, H - y - h)],
  ];
  const img = layers.map((l) => l[0]).join(",");
  const pos = layers.map((l) => `${l[1]}px ${l[2]}px`).join(",");
  const size = layers.map((l) => `${l[3]}px ${l[4]}px`).join(",");
  return `-webkit-mask-image:${img};mask-image:${img};-webkit-mask-position:${pos};mask-position:${pos};-webkit-mask-size:${size};mask-size:${size};-webkit-mask-repeat:no-repeat;mask-repeat:no-repeat`;
}
