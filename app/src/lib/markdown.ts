import { marked } from "marked";
import DOMPurify from "dompurify";

const escapeAttribute = (value: string) => value.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

export function localLinkPath(value: string): string | null {
  if (!value || value.startsWith("#") || /^(?:https?|mailto|tel|data|javascript):/i.test(value)) return null;
  let path = value;
  if (/^file:\/\//i.test(path)) {
    try { const url = new URL(path); path = url.host ? `\\\\${url.host}${decodeURIComponent(url.pathname)}` : decodeURIComponent(url.pathname).replace(/^\/(?=[a-z]:)/i, ""); } catch { return null; }
  } else if (/^[a-z][a-z\d+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path)) return null;
  try { return decodeURIComponent(path).replace(/:(\d+)(?::\d+)?$/, ""); } catch { return path; }
}

marked.use({ gfm: true, breaks: true, renderer: {
  link({ href, title, tokens }) {
    const label = this.parser.parseInline(tokens);
    const path = localLinkPath(href);
    const hint = title ? ` title="${escapeAttribute(title)}"` : "";
    if (path) return `<a href="#" class="md-file" data-path="${escapeAttribute(path)}"${hint}>${label}</a>`;
    return `<a href="${escapeAttribute(href)}" rel="noopener noreferrer"${hint}>${label}</a>`;
  },
} });

/** Annotate rendered text, never rewrite Markdown source or code-fence contents. */
export function md(src: string): string {
  if (!src) return "";
  const clean = DOMPurify.sanitize(marked.parse(src, { async: false }) as string, { ADD_ATTR: ["data-path"], FORBID_TAGS: ["style"] });
  const container = document.createElement("div");
  container.innerHTML = clean;
  const walker = document.createTreeWalker(container, NodeFilter.SHOW_TEXT);
  const nodes: Text[] = [];
  while (walker.nextNode()) {
    const node = walker.currentNode as Text;
    if (!node.parentElement?.closest("a, pre, .md-file")) nodes.push(node);
  }
  for (const node of nodes) {
    const pattern = /(?:[A-Za-z]:[\\/]|\\\\)[^\s<>"'`|*?]+/g;
    const matches = [...node.data.matchAll(pattern)];
    if (!matches.length) continue;
    const fragment = document.createDocumentFragment();
    let offset = 0;
    for (const match of matches) {
      const start = match.index!;
      const path = match[0].replace(/[.,;:)\]】」]+$/, "");
      fragment.append(node.data.slice(offset, start));
      const span = document.createElement("span");
      span.className = "md-file";
      span.dataset.path = localLinkPath(path) ?? path;
      span.textContent = path;
      fragment.append(span);
      offset = start + path.length;
    }
    fragment.append(node.data.slice(offset));
    node.replaceWith(fragment);
  }
  return container.innerHTML;
}
