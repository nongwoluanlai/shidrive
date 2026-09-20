import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({ gfm: true, breaks: true });

// Force every link to open safely in a new tab (runs inside sanitize,
// so the attributes we set here are kept on the output node).
let hookInstalled = false;
function ensureLinkHook() {
  if (hookInstalled) return;
  hookInstalled = true;
  DOMPurify.addHook("afterSanitizeAttributes", (node) => {
    if (node.tagName === "A" && node.getAttribute("href")) {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }
  });
}

/** Render markdown to sanitized HTML. Raw HTML is escaped by DOMPurify. */
export function md(src: string): string {
  if (!src) return "";
  ensureLinkHook();
  // 磁盘路径（C:\xx\yy 与 \\unc）包成可交互的 md-file span：反斜杠以 HTML
  // 实体携带，避免被 marked 当转义符吃掉；行尾标点不归入路径
  const withPaths = src.replace(/(?:[A-Za-z]:\\|\\\\)[^\s<>"'`|*?]+/g, (m) => {
    const clean = m.replace(/[.,;:)\]】」]+$/, "");
    const tail = m.slice(clean.length);
    const esc = clean.replace(/\\/g, "&#92;").replace(/"/g, "&quot;");
    return `<span class="md-file" data-path="${esc}">${esc}</span>${tail}`;
  });
  const html = marked.parse(withPaths, { async: false }) as string;
  return DOMPurify.sanitize(html, {
    ADD_ATTR: ["target", "data-path"],
    FORBID_TAGS: ["style"],
  });
}
