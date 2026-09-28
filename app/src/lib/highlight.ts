// 文件查看器的轻量代码着色。
//
// 用 @speed-highlight/core（CC0）的「无注册表」分词器：它是个生成器，碰到需要的
// 语言就 yield 语言名，由调用方把语法交回去。语法在打开文件时按扩展名异步加载
// （每种一个 0.2–1KB 的小 chunk）；加载好之后着色完全同步，所以可以在每次按键的
// 同一帧里刷新（textarea 文字是透明的，着色层一旦落后就会看到错位的字）。
import { tokenizer, type ShjLanguageData } from "@speed-highlight/core/tokenize";

type Grammar = ShjLanguageData;

// 显式列出：vite 只能拆分静态可分析的 import()，也顺带限定了会被打包的语言
const LOADERS: Record<string, () => Promise<{ default: unknown }>> = {
  asm: () => import("@speed-highlight/core/languages/asm.js"),
  bash: () => import("@speed-highlight/core/languages/bash.js"),
  c: () => import("@speed-highlight/core/languages/c.js"),
  css: () => import("@speed-highlight/core/languages/css.js"),
  csv: () => import("@speed-highlight/core/languages/csv.js"),
  diff: () => import("@speed-highlight/core/languages/diff.js"),
  docker: () => import("@speed-highlight/core/languages/docker.js"),
  go: () => import("@speed-highlight/core/languages/go.js"),
  html: () => import("@speed-highlight/core/languages/html.js"),
  http: () => import("@speed-highlight/core/languages/http.js"),
  ini: () => import("@speed-highlight/core/languages/ini.js"),
  java: () => import("@speed-highlight/core/languages/java.js"),
  js: () => import("@speed-highlight/core/languages/js.js"),
  jsdoc: () => import("@speed-highlight/core/languages/jsdoc.js"),
  json: () => import("@speed-highlight/core/languages/json.js"),
  log: () => import("@speed-highlight/core/languages/log.js"),
  lua: () => import("@speed-highlight/core/languages/lua.js"),
  make: () => import("@speed-highlight/core/languages/make.js"),
  md: () => import("@speed-highlight/core/languages/md.js"),
  pl: () => import("@speed-highlight/core/languages/pl.js"),
  py: () => import("@speed-highlight/core/languages/py.js"),
  regex: () => import("@speed-highlight/core/languages/regex.js"),
  rs: () => import("@speed-highlight/core/languages/rs.js"),
  sql: () => import("@speed-highlight/core/languages/sql.js"),
  todo: () => import("@speed-highlight/core/languages/todo.js"),
  toml: () => import("@speed-highlight/core/languages/toml.js"),
  ts: () => import("@speed-highlight/core/languages/ts.js"),
  uri: () => import("@speed-highlight/core/languages/uri.js"),
  xml: () => import("@speed-highlight/core/languages/xml.js"),
  yaml: () => import("@speed-highlight/core/languages/yaml.js"),
};

/** 语言的显示名（工具栏上展示） */
export const LANG_LABEL: Record<string, string> = {
  asm: "Assembly", bash: "Shell", c: "C/C++", css: "CSS", csv: "CSV", diff: "Diff", docker: "Dockerfile",
  go: "Go", html: "HTML", http: "HTTP", ini: "INI", java: "Java", js: "JavaScript", json: "JSON", log: "Log",
  lua: "Lua", make: "Makefile", md: "Markdown", pl: "Perl", py: "Python", rs: "Rust", sql: "SQL",
  toml: "TOML", ts: "TypeScript", xml: "XML", yaml: "YAML",
};

const EXT: Record<string, string> = {
  js: "js", mjs: "js", cjs: "js", jsx: "js",
  ts: "ts", tsx: "ts", mts: "ts", cts: "ts",
  json: "json", jsonc: "json", json5: "json", webmanifest: "json", map: "json",
  html: "html", htm: "html", svelte: "html", vue: "html", astro: "html",
  xml: "xml", svg: "xml", xaml: "xml", csproj: "xml", vcxproj: "xml", plist: "xml", xsd: "xml", xsl: "xml", resx: "xml",
  css: "css", scss: "css", less: "css", pcss: "css",
  md: "md", markdown: "md", mdx: "md",
  py: "py", pyw: "py", pyi: "py",
  rs: "rs", go: "go", java: "java", lua: "lua", sql: "sql", pl: "pl", pm: "pl",
  c: "c", h: "c", cc: "c", cpp: "c", cxx: "c", hpp: "c", hh: "c", hxx: "c",
  sh: "bash", bash: "bash", zsh: "bash",
  yml: "yaml", yaml: "yaml", toml: "toml",
  ini: "ini", cfg: "ini", conf: "ini", properties: "ini", env: "ini", editorconfig: "ini", gitconfig: "ini", npmrc: "ini",
  diff: "diff", patch: "diff", log: "log", csv: "csv", http: "http", asm: "asm", s: "asm", mk: "make",
};
const NAMES: Record<string, string> = {
  dockerfile: "docker", makefile: "make", gnumakefile: "make", ".env": "ini", ".gitconfig": "ini",
  "cargo.lock": "toml", "pnpm-lock.yaml": "yaml",
};

/** 按文件名/扩展名判断语言；不认识的返回 null（保持纯文本） */
export function langForPath(path: string): string | null {
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (NAMES[name]) return NAMES[name];
  if (name.startsWith("dockerfile")) return "docker";
  if (name.startsWith(".env")) return "ini";
  const dot = name.lastIndexOf(".");
  if (dot < 0) return null;
  // package-lock.json / yarn.lock 这类超大锁文件按扩展名处理即可（会被大小上限拦下）
  return EXT[name.slice(dot + 1)] ?? null;
}

const grammars = new Map<string, Grammar>();
const pending = new Map<string, Promise<boolean>>();

export const langLoaded = (name: string) => grammars.has(name);

/** 异步加载语法（重复调用复用同一个 Promise）；未知语言返回 false */
export function loadLang(name: string): Promise<boolean> {
  if (grammars.has(name)) return Promise.resolve(true);
  const loader = LOADERS[name];
  if (!loader) return Promise.resolve(false);
  let p = pending.get(name);
  if (!p) {
    p = loader()
      .then((m) => {
        grammars.set(name, m.default as Grammar);
        return true;
      })
      .catch(() => false)
      .finally(() => pending.delete(name));
    pending.set(name, p);
  }
  return p;
}

// 各语法固定引用的子语言（从语法源码里的 sub:"xx" 整理）。和主语法一起加载，
// 打开文件时就一次性着色完整，不会先出一版缺注释 TODO/正则颜色的再刷新。
// （Markdown 代码块的语言是运行时识别的，仍走 highlightToBlocks 的 onMissing。）
const SUBS: Record<string, string[]> = {
  js: ["jsdoc", "regex", "todo"],
  ts: ["js", "jsdoc", "regex", "todo"],
  html: ["css", "js", "jsdoc", "regex", "todo"],
  c: ["asm", "todo"],
  make: ["bash", "todo"],
};

/** 加载语言及其固定子语言；主语言未知时返回 false */
export async function loadLangDeep(name: string): Promise<boolean> {
  const subs = SUBS[name] ?? (name in LOADERS ? ["todo"] : []);
  const [ok] = await Promise.all([loadLang(name), ...subs.map(loadLang)]);
  return ok;
}

const NEEDS_ESC = /[&<>]/;
const escHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/**
 * 同步着色，按行分块输出 HTML（每块 linesPerBlock 行，<span class="tk-类型">，文本已转义）。
 * 分块是为了增量更新 DOM：每次按键全文重新分词（很快），但只替换内容变了的块，
 * 浏览器也只需重排这一块——整层 innerHTML 重建的排版开销是分词的 3–4 倍。
 * 跨行的 token（块注释、模板字符串）在换行处拆成多个 span，所以块边界总落在行尾；
 * 除最后一块外，每块都以 "\n" 结尾（块末的换行在 pre-wrap 下不会多出空行）。
 *
 * 语法里引用的子语言（如 Markdown 代码块的语言）若尚未加载，该段先按纯文本输出，
 * 并通过 onMissing 报告，调用方加载后重新着色即可。
 */
export function highlightToBlocks(
  src: string,
  lang: string,
  onMissing?: (name: string) => void,
  linesPerBlock = 40,
): string[] {
  const blocks: string[] = [];
  let parts: string[] = [];
  let lines = 0;
  const push = (piece: string, type?: string) => {
    const html = NEEDS_ESC.test(piece) ? escHtml(piece) : piece;
    parts.push(type ? `<span class="tk-${type}">${html}</span>` : html);
  };
  const onToken = (text: string, type?: string) => {
    if (!text) return;
    let nl = text.indexOf("\n");
    if (nl < 0) return push(text, type); // 绝大多数 token 不含换行
    let start = 0;
    while (nl >= 0) {
      push(text.slice(start, nl + 1), type);
      start = nl + 1;
      if (++lines >= linesPerBlock) {
        blocks.push(parts.join(""));
        parts = [];
        lines = 0;
      }
      nl = text.indexOf("\n", start);
    }
    if (start < text.length) push(text.slice(start), type);
  };
  const it = tokenizer(src, lang, onToken);
  let res = it.next();
  while (!res.done) {
    const name = res.value;
    const g = grammars.get(name);
    if (!g) onMissing?.(name);
    res = it.next(g);
  }
  // 文本以换行结尾时 textarea 会多显示一个空行，而 div 不会：补一个空格撑出这一行
  if (src.endsWith("\n")) parts.push(" ");
  blocks.push(parts.join(""));
  return blocks;
}
