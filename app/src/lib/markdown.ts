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
  const html = marked.parse(src, { async: false }) as string;
  return DOMPurify.sanitize(html, {
    ADD_ATTR: ["target"],
    FORBID_TAGS: ["style"],
  });
}
