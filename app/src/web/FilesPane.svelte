<script lang="ts">
  import { api, type DirEntry, type FileHit, type ContentHit } from "./api";
  import { ui, currentProject, toast, errText } from "./store.svelte";
  import { langForPath, loadLangDeep, highlightToBlocks, loadLang } from "../lib/highlight";
  import { md } from "../lib/markdown";
  import Icon from "./Icon.svelte";

  interface DirState { entries: DirEntry[]; loading: boolean; error: string }

  let root = $state("");
  let dirs = $state<Record<string, DirState>>({});
  let open = $state<Record<string, boolean>>({});
  let selected = $state<string | null>(null);
  let preview = $state<{ path: string; kind: "text" | "image" | "binary" | "large" | "error"; content: string; size: number; lang: string | null; html: string } | null>(null);
  let previewLoading = $state(false);
  let mdMode = $state(true);
  let wrap = $state(false);
  let query = $state("");
  let searchMode = $state<"files" | "content">("files");
  let searching = $state(false);
  let hits = $state<{ files: FileHit[]; content: ContentHit[] } | null>(null);
  let loadedFor = "";

  const project = $derived(currentProject());
  const sep = $derived(root.includes("\\") ? "\\" : "/");

  async function loadDir(path: string) {
    const pid = project?.id;
    if (!pid) return;
    dirs[path] = { entries: dirs[path]?.entries ?? [], loading: true, error: "" };
    try {
      const r = await api.list(pid, path);
      if (!path) root = r.root;
      const entries = r.entries.slice().sort((a, b) => (a.is_dir === b.is_dir ? a.name.localeCompare(b.name, "zh-CN", { numeric: true }) : a.is_dir ? -1 : 1));
      dirs[path] = { entries, loading: false, error: "" };
    } catch (e) {
      dirs[path] = { entries: [], loading: false, error: errText(e) };
    }
  }

  $effect(() => {
    const pid = project?.id ?? "";
    if (pid === loadedFor) return;
    loadedFor = pid;
    root = ""; dirs = {}; open = {}; selected = null; preview = null; hits = null; query = "";
    if (pid) void loadDir("");
  });

  // 聊天里点击的文件路径
  $effect(() => {
    const p = ui.openFile;
    if (!p || !project) return;
    ui.openFile = null;
    const abs = /^([a-zA-Z]:[\\/]|\\\\|\/)/.test(p) ? p : (root || project.root_path).replace(/[\\/]+$/, "") + sep + p.replace(/^\.?[\\/]/, "");
    void openFile(abs);
  });

  type Row = { e: DirEntry; depth: number };
  const rows = $derived.by(() => {
    const out: Row[] = [];
    const walk = (path: string, depth: number) => {
      for (const e of dirs[path]?.entries ?? []) {
        out.push({ e, depth });
        if (e.is_dir && open[e.path]) walk(e.path, depth + 1);
      }
    };
    walk("", 0);
    return out;
  });

  function toggle(e: DirEntry) {
    if (!e.is_dir) { void openFile(e.path); return; }
    open[e.path] = !open[e.path];
    if (open[e.path] && !dirs[e.path]) void loadDir(e.path);
  }

  const IMG = /\.(png|jpe?g|gif|webp|bmp|ico)$/i;
  async function openFile(path: string) {
    if (!project) return;
    selected = path;
    previewLoading = true;
    try {
      if (IMG.test(path)) {
        preview = { path, kind: "image", content: api.rawUrl(project.id, path), size: 0, lang: null, html: "" };
        return;
      }
      const r = await api.read(project.id, path);
      if (r.tooLarge) { preview = { path, kind: "large", content: "", size: r.size, lang: null, html: "" }; return; }
      if (r.binary) { preview = { path, kind: "binary", content: "", size: r.size, lang: null, html: "" }; return; }
      const content = r.content ?? "";
      const lang = langForPath(path);
      preview = { path, kind: "text", content, size: r.size, lang, html: "" };
      if (lang && content.length < 400_000) {
        await loadLangDeep(lang);
        if (preview?.path === path) preview.html = renderHl(content, lang, path);
      }
    } catch (e) {
      preview = { path, kind: "error", content: errText(e), size: 0, lang: null, html: "" };
    } finally {
      previewLoading = false;
    }
  }

  function renderHl(content: string, lang: string, path: string): string {
    const missing = new Set<string>();
    const html = highlightToBlocks(content, lang, (n) => missing.add(n), 200).join("");
    if (missing.size) {
      void Promise.all([...missing].map(loadLang)).then(() => {
        if (preview?.path === path && preview.lang) preview.html = highlightToBlocks(content, lang, undefined, 200).join("");
      });
    }
    return html;
  }

  const isMd = $derived(!!preview && /\.(md|markdown)$/i.test(preview.path));
  const lineCount = $derived(preview?.kind === "text" ? preview.content.split("\n").length : 0);
  const rel = (p: string) => (root && p.startsWith(root) ? p.slice(root.length).replace(/^[\\/]/, "") || "." : p);

  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  function onQuery() {
    if (searchTimer) clearTimeout(searchTimer);
    if (!query.trim()) { hits = null; return; }
    searchTimer = setTimeout(() => void runSearch(), searchMode === "files" ? 250 : 500);
  }
  async function runSearch() {
    if (!project || !query.trim()) return;
    searching = true;
    try {
      const r = await api.search(project.id, query.trim(), searchMode);
      hits = { files: r.files ?? [], content: r.content ?? [] };
    } catch (e) {
      toast("error", errText(e));
    } finally {
      searching = false;
    }
  }

  function fmtSize(n: number) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(1)} MB`;
  }
  async function copy(text: string) {
    try { await navigator.clipboard.writeText(text); toast("ok", "已复制"); } catch { toast("warn", "当前浏览器不允许复制（需 HTTPS）"); }
  }
</script>

<div class="files" class:has-preview={!!selected}>
  <div class="tree-pane">
    <div class="bar">
      <div class="search">
        <Icon name="search" size={16} />
        <input placeholder={searchMode === "files" ? "搜索文件名" : "搜索文件内容"} bind:value={query} oninput={onQuery} />
        {#if searching}<span class="spin"></span>{/if}
        {#if query}<button class="clr" onclick={() => { query = ""; hits = null; }} aria-label="清除"><Icon name="x" size={14} /></button>{/if}
      </div>
      <button class="mode" onclick={() => { searchMode = searchMode === "files" ? "content" : "files"; onQuery(); }} title="切换搜索方式">{searchMode === "files" ? "文件名" : "内容"}</button>
      <button class="icon-btn" title="刷新" onclick={() => { dirs = {}; void loadDir(""); for (const p of Object.keys(open)) if (open[p]) void loadDir(p); }}><Icon name="refresh" size={17} /></button>
    </div>
    <div class="tree scroll">
      {#if !project}
        <div class="empty">请选择项目</div>
      {:else if hits}
        {#if searchMode === "files"}
          {#each hits.files as h (h.path)}
            <button class="node" class:sel={selected === h.path} onclick={() => void openFile(h.path)}><Icon name="file" size={15} /><span class="nm">{h.rel}</span></button>
          {:else}<div class="empty">没有匹配的文件</div>{/each}
        {:else}
          {#each hits.content as h (h.path)}
            <div class="hit-file">{h.rel}</div>
            {#each h.matches as m (m.line + ":" + m.col)}
              <button class="hit" onclick={() => void openFile(h.path)}><span class="ln">{m.line}</span><span class="pv">{m.preview}</span></button>
            {/each}
          {:else}<div class="empty">没有匹配的内容</div>{/each}
        {/if}
      {:else if dirs[""]?.loading && !rows.length}
        <div class="empty"><span class="spin"></span></div>
      {:else if dirs[""]?.error}
        <div class="empty">{dirs[""].error}</div>
      {:else}
        <div class="root-path mono" title={root}>{root}</div>
        {#each rows as r (r.e.path)}
          <button class="node" class:sel={selected === r.e.path} style="padding-left:{10 + r.depth * 16}px" onclick={() => toggle(r.e)}>
            {#if r.e.is_dir}
              <span class="chev" class:open={open[r.e.path]}><Icon name="chevron" size={13} /></span><Icon name="folder" size={15} />
            {:else}
              <span class="chev"></span><Icon name={IMG.test(r.e.name) ? "image" : "file"} size={15} />
            {/if}
            <span class="nm">{r.e.name}</span>
            {#if r.e.is_dir && dirs[r.e.path]?.loading}<span class="spin sm"></span>{/if}
          </button>
        {:else}
          <div class="empty">空目录</div>
        {/each}
      {/if}
    </div>
  </div>

  <div class="preview-pane">
    {#if !selected}
      <div class="empty"><Icon name="file" size={30} /><div>选择文件以预览</div><div class="faint">只读预览 · 支持代码着色、Markdown 与图片</div></div>
    {:else}
      <div class="pbar">
        <button class="icon-btn back" onclick={() => { selected = null; preview = null; }} aria-label="返回"><Icon name="back" /></button>
        <div class="pname"><b>{selected.split(/[\\/]/).pop()}</b><small class="mono">{rel(selected)}</small></div>
        {#if preview?.kind === "text"}
          {#if isMd}<button class="btn sm ghost" onclick={() => (mdMode = !mdMode)}>{mdMode ? "源码" : "渲染"}</button>{/if}
          <button class="btn sm ghost" onclick={() => (wrap = !wrap)}>{wrap ? "不换行" : "换行"}</button>
        {/if}
        <button class="icon-btn" title="复制路径" onclick={() => void copy(selected!)}><Icon name="copy" size={16} /></button>
      </div>
      <div class="pbody scroll">
        {#if previewLoading && !preview}
          <div class="empty"><span class="spin"></span></div>
        {:else if preview?.kind === "image"}
          <div class="img-wrap"><img src={preview.content} alt={preview.path} /></div>
        {:else if preview?.kind === "binary"}
          <div class="empty">二进制文件（{fmtSize(preview.size)}），不支持预览</div>
        {:else if preview?.kind === "large"}
          <div class="empty">文件较大（{fmtSize(preview.size)}），网页端只预览 2MB 以内的文件</div>
        {:else if preview?.kind === "error"}
          <div class="empty">{preview.content}</div>
        {:else if preview?.kind === "text"}
          {#if isMd && mdMode}
            <div class="md md-pv">{@html md(preview.content)}</div>
          {:else}
            <div class="code" class:wrap>
              <div class="gutter mono" aria-hidden="true">{#each { length: lineCount } as _, i}<span>{i + 1}</span>{/each}</div>
              {#if preview.html}<pre class="src hl mono">{@html preview.html}</pre>{:else}<pre class="src mono">{preview.content}</pre>{/if}
            </div>
          {/if}
        {/if}
      </div>
      {#if preview?.kind === "text"}<div class="pfoot faint">{lineCount} 行 · {fmtSize(preview.size)}{preview.lang ? ` · ${preview.lang}` : ""}</div>{/if}
    {/if}
  </div>
</div>

<style>
  .files { flex: 1; min-height: 0; display: grid; grid-template-columns: minmax(260px, 340px) 1fr; }
  .tree-pane { display: flex; flex-direction: column; min-height: 0; border-right: 1px solid var(--border-soft); background: var(--bg-panel); }
  .bar { display: flex; align-items: center; gap: 6px; padding: 8px; border-bottom: 1px solid var(--border-soft); }
  .search { flex: 1; min-width: 0; display: flex; align-items: center; gap: 6px; background: var(--bg-elev); border: 1px solid var(--border); border-radius: 10px; padding: 0 8px; height: 36px; color: var(--text-faint); }
  .search:focus-within { border-color: var(--accent); }
  .search input { flex: 1; min-width: 0; border: none; outline: none; background: transparent; color: var(--text); }
  .clr { color: var(--text-faint); display: inline-flex; }
  .mode { font-size: 12px; padding: 4px 8px; border-radius: 8px; border: 1px solid var(--border); color: var(--text-dim); white-space: nowrap; }
  .tree { flex: 1; min-height: 0; padding: 4px 0 12px; }
  .root-path { font-size: 11.5px; color: var(--text-faint); padding: 4px 12px 6px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .node { display: flex; align-items: center; gap: 6px; width: 100%; padding: 6px 10px; text-align: left; color: var(--text-dim); font-size: 14px; }
  .node:hover { background: var(--bg-elev); color: var(--text); }
  .node.sel { background: var(--accent-soft); color: var(--accent); }
  .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0; }
  .chev { width: 13px; display: inline-flex; transition: transform .12s; flex: none; }
  .chev.open { transform: rotate(90deg); }
  .spin.sm { width: 11px; height: 11px; border-width: 1.5px; }
  .hit-file { font-size: 12.5px; color: var(--text); padding: 8px 12px 2px; font-weight: 600; word-break: break-all; }
  .hit { display: flex; gap: 8px; width: 100%; text-align: left; padding: 3px 12px 3px 18px; font-size: 12.5px; color: var(--text-dim); }
  .hit:hover { background: var(--bg-elev); }
  .ln { color: var(--text-faint); font-family: var(--mono); min-width: 2.5em; text-align: right; }
  .pv { font-family: var(--mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .preview-pane { display: flex; flex-direction: column; min-width: 0; min-height: 0; }
  .pbar { display: flex; align-items: center; gap: 6px; padding: 6px 8px; border-bottom: 1px solid var(--border-soft); background: var(--bg-panel); }
  .back { display: none; }
  .pname { flex: 1; min-width: 0; display: flex; flex-direction: column; line-height: 1.25; padding-left: 4px; }
  .pname b, .pname small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pname small { font-size: 11.5px; color: var(--text-faint); }
  .pbody { flex: 1; min-height: 0; }
  .pfoot { font-size: 12px; padding: 4px 12px calc(4px + var(--safe-b)); border-top: 1px solid var(--border-soft); }
  .code { display: flex; min-width: max-content; font-size: 13px; line-height: 1.55; }
  .code.wrap { min-width: 0; }
  .gutter { display: flex; flex-direction: column; text-align: right; padding: 10px 10px 10px 12px; color: var(--text-faint); user-select: none; border-right: 1px solid var(--border-soft); position: sticky; left: 0; background: var(--bg); }
  .src { margin: 0; padding: 10px 14px; white-space: pre; tab-size: 4; user-select: text; flex: 1; }
  .code.wrap .src { white-space: pre-wrap; word-break: break-all; }
  .code.wrap .gutter { display: none; }
  .md-pv { max-width: 860px; margin: 0 auto; padding: 16px 18px 40px; user-select: text; }
  .img-wrap { padding: 16px; display: grid; place-items: center; min-height: 100%; background: repeating-conic-gradient(var(--bg-elev) 0 25%, transparent 0 50%) 0 0 / 16px 16px; }
  .img-wrap img { max-width: 100%; height: auto; box-shadow: var(--shadow); }

  @media (max-width: 899px) {
    .files { grid-template-columns: 1fr; }
    .tree-pane { border-right: none; }
    .preview-pane { display: none; }
    .has-preview .tree-pane { display: none; }
    .has-preview .preview-pane { display: flex; }
    .back { display: inline-flex; }
    .code { font-size: 12.5px; }
  }
</style>
