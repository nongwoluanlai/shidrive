// 完整朗读：从流式正文里逐步取出「可以读出来」的文字（跳过代码块、表格、指令标记、链接和路径），
// 只追加不回退，配合 feedSpeech 的偏移续读。纯函数，方便单测。

export type BodyCursor = { cur: number; mid: boolean; fence: boolean };
export const newBodyCursor = (): BodyCursor => ({ cur: 0, mid: false, fence: false });

const ENDERS = "。！？!?；;…";
const CLOSERS = "。！？!?…”」』）)\"'";

/** 一行（或半行）正文 → 适合朗读的文字 */
export function proseSpeech(s: string): string {
  return s
    .replace(/\[\[[^\]]*\]\]/g, "")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/[^\s，。；！？、）)]+/g, "")
    .replace(/`([^`]*)`/g, (_, c: string) => (c.length <= 16 && !/[\\/]/.test(c) ? c : ""))
    .replace(/\b[A-Za-z]:\\[\w.\\~ -]*/g, "")
    .replace(/(?:[\w.~-]+)?(?:\/[\w.~-]+){2,}\/?/g, "")
    .replace(/^\s*#{1,6}\s+/, "")
    .replace(/^\s*>\s?/, "")
    .replace(/^\s*([-+*]|\d{1,2}[.)、])\s+/, "")
    .replace(/[*_~]{1,3}/g, "")
    .replace(/[ \t]+/g, " ")
    .replace(/\s+([，。、,.!?！？；;：:])/g, "$1")
    .replace(/[，,、：:]+(?=[，。,.!?！？；;])/g, "")
    .replace(/^[，。,.、；;：:\s]+/, "")
    .trim();
}

const norm = (s: string) => s.replace(/[\s\p{P}\p{S}]/gu, "").toLowerCase();

/** 半行里最后一个句末标点之后的位置（不含未闭合的行内代码 / 指令标记）；没有返回 -1 */
function lastCut(seg: string): number {
  let lim = seg.length;
  const open = seg.lastIndexOf("[[");
  if (open >= 0 && seg.indexOf("]]", open) < 0) lim = open;
  let cut = -1;
  for (let i = 0; i < lim; i++) {
    if (!ENDERS.includes(seg[i])) continue;
    let j = i + 1;
    while (j < lim && CLOSERS.includes(seg[j])) j++;
    if ((seg.slice(0, j).match(/`/g)?.length ?? 0) % 2 === 0) cut = j;
    i = j - 1;
  }
  return cut;
}

/**
 * 从 raw[c.cur..] 取出新的可朗读文字（推进游标）。没写完的行只取到最后一个句末标点，
 * 代码块 / 表格 / 指令标记行等写完整再判断。subs = 已有字幕（正文里重复字幕的句子不再读）。
 */
export function takeBodySpeech(raw: string, c: BodyCursor, final: boolean, subs = ""): string {
  const subN = norm(subs);
  const pick = (s: string) => {
    const x = proseSpeech(s);
    const n = norm(x);
    if (!n) return "";
    if (n.length >= 4 && subN.includes(n)) return "";
    return x;
  };
  let out = "";
  while (c.cur < raw.length) {
    const nl = raw.indexOf("\n", c.cur);
    const complete = nl >= 0 || final;
    const end = nl >= 0 ? nl : raw.length;
    const seg = raw.slice(c.cur, end);
    if (!c.mid) {
      const t = seg.trim();
      if (!complete && (t.length < 4 || /^(`|~|\||\[\[|#|>|[-+*]\s|\d)/.test(t) || c.fence)) break;
      if (/^(```|~~~)/.test(t)) {
        c.fence = !c.fence;
        c.cur = end + 1;
        continue;
      }
      if (c.fence || /^\|/.test(t) || /^([-*_]\s*){3,}$/.test(t)) {
        c.cur = end + 1;
        continue;
      }
    }
    if (complete) {
      const x = pick(seg);
      if (x || c.mid) out += `${x}\n`;
      c.cur = end + 1;
      c.mid = false;
      continue;
    }
    const cut = lastCut(seg);
    if (cut <= 0) break;
    const x = pick(seg.slice(0, cut));
    if (x) out += x;
    c.cur += cut;
    c.mid = true;
    break;
  }
  if (c.cur > raw.length) c.cur = raw.length;
  return out;
}
