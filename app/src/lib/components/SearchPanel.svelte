<script lang="ts">
  import { t } from "../i18n";
  // 全局搜索（Alt+F）：文件名快速打开 + 文件内容搜索。
  // 搜索在后端并行执行（遵循 .gitignore、跳过二进制/大文件），新请求会取消进行中的旧请求；
  // 前端只做防抖 + 序号校验（丢弃过期结果），列表分批渲染，结果多时也不卡。
  import { tick } from "svelte";
  import { app, currentProject, openEditor, toast } from "../state.svelte";
  import { api } from "../ipc";
  import type { SearchContentHit, SearchFileHit, SearchLineMatch, SearchResult } from "../types";
  import Icon from "./Icon.svelte";

  const OPTS_KEY = "shidrive.search.opts";
  interface Persist { caseSensitive: boolean; wholeWord: boolean; regex: boolean; useIgnore: boolean; include: string; exclude: string; showFilters: boolean }
  function loadOpts(): Persist {
    const d: Persist = { caseSensitive: false, wholeWord: false, regex: false, useIgnore: true, include: "", exclude: "", showFilters: false };
    try {
      return { ...d, ...JSON.parse(localStorage.getItem(OPTS_KEY) || "{}") };
    } catch {
      return d;
    }
  }
  const opts = $state(loadOpts());
  $effect(() => {
    localStorage.setItem(OPTS_KEY, JSON.stringify(opts));
  });

  const root = $derived(currentProject()?.root_path ?? "");
  let query = $state("");
  let result = $state<SearchResult | null>(null);
  let error = $state("");
  let busy = $state(false);
  let sel = $state(0);
  let limit = $state(300);
  let inputEl = $state<HTMLInputElement | null>(null);
  let listEl = $state<HTMLDivElement | null>(null);
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  type Row =
    | { kind: "file"; hit: SearchFileHit }
    | { kind: "group"; hit: SearchContentHit }
    | { kind: "match"; hit: SearchContentHit; m: SearchLineMatch };
  const rows = $derived.by<Row[]>(() => {
    const r = result;
    if (!r) return [];
    if (app.searchMode === "files") return r.files.map((hit) => ({ kind: "file", hit }) as Row);
    const out: Row[] = [];
    for (const hit of r.content) {
      out.push({ kind: "group", hit });
      for (const m of hit.matches) out.push({ kind: "match", hit, m });
    }
    return out;
  });
  const shown = $derived(rows.slice(0, limit));

  // 打开时：聚焦并全选上次的查询词
  $effect(() => {
    if (!app.searchOpen) return;
    void tick().then(() => {
      inputEl?.focus();
      inputEl?.select();
    });
  });

  // 查询词/模式/选项变化 → 防抖搜索（文件名几乎即时；内容稍长，避免每个按键都扫一遍）
  $effect(() => {
    const q = query;
    const mode = app.searchMode;
    const r = root;
    const o = { ...opts };
    if (!app.searchOpen) return;
    if (timer) clearTimeout(timer);
    const my = ++seq;
    if (!q.trim() || !r) {
      result = null;
      error = "";
      busy = false;
      return;
    }
    timer = setTimeout(() => void run(my, q, mode, r, o), mode === "files" ? 60 : 220);
  });

  async function run(my: number, q: string, mode: "files" | "content", r: string, o: Persist) {
    busy = true;
    try {
      const res = await api.fsSearch({
        root: r,
        query: mode === "files" ? q.trim() : q,
        mode,
        caseSensitive: o.caseSensitive,
        wholeWord: o.wholeWord,
        regex: o.regex,
        include: o.include,
        exclude: o.exclude,
        useIgnore: o.useIgnore,
      });
      if (my !== seq || res.cancelled) return;
      result = res;
      error = "";
      sel = 0;
      limit = 300;
      if (listEl) listEl.scrollTop = 0;
    } catch (e) {
      if (my !== seq) return;
      result = null;
      error = String(e);
    } finally {
      if (my === seq) busy = false;
    }
  }

  function close() {
    app.searchOpen = false;
    if (timer) clearTimeout(timer);
    seq++;
    busy = false;
    void api.fsSearchCancel().catch(() => {});
  }

  function setMode(m: "files" | "content") {
    app.searchMode = m;
    void tick().then(() => inputEl?.focus());
  }

  async function openAt(path: string, m?: SearchLineMatch) {
    try {
      if (app.editor?.path !== path) {
        const [content, binary] = await api.fsRead(path);
        openEditor(path, content, binary);
      } else {
        app.editor.collapsed = false;
      }
      app.tab = "chat";
      if (m) app.editorJump = { path, line: m.line, col: m.col, len: m.len, seq: (app.editorJump?.seq ?? 0) + 1 };
      close();
    } catch (e) {
      toast("error", String(e));
    }
  }

  function activate(row: Row | undefined) {
    if (!row) return;
    if (row.kind === "file") void openAt(row.hit.path);
    else if (row.kind === "group") void openAt(row.hit.path, row.hit.matches[0]);
    else void openAt(row.hit.path, row.m);
  }

  async function moveSel(d: number) {
    const n = shown.length;
    if (!n) return;
    sel = Math.max(0, Math.min(n - 1, sel + d));
    if (sel >= limit - 5 && limit < rows.length) limit += 300;
    await tick();
    listEl?.querySelector(".row.sel")?.scrollIntoView({ block: "nearest" });
  }

  function onKey(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      void moveSel(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      void moveSel(-1);
    } else if (e.key === "PageDown") {
      e.preventDefault();
      void moveSel(10);
    } else if (e.key === "PageUp") {
      e.preventDefault();
      void moveSel(-10);
    } else if (e.key === "Enter") {
      e.preventDefault();
      activate(shown[sel]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === "Tab" && !e.shiftKey && !e.ctrlKey && document.activeElement === inputEl) {
      e.preventDefault();
      setMode(app.searchMode === "files" ? "content" : "files");
    }
  }

  /** 文件名高亮：indices 是 rel 的 char 下标 */
  function fileSegs(h: SearchFileHit): { s: string; on: boolean }[] {
    const chars = Array.from(h.rel);
    const set = new Set(h.indices);
    const out: { s: string; on: boolean }[] = [];
    chars.forEach((c, i) => {
      const on = set.has(i);
      const last = out[out.length - 1];
      if (last && last.on === on) last.s += c;
      else out.push({ s: c, on });
    });
    return out;
  }
  function baseName(rel: string) {
    const i = rel.lastIndexOf("\\");
    return i < 0 ? rel : rel.slice(i + 1);
  }
  function dirName(rel: string) {
    const i = rel.lastIndexOf("\\");
    return i < 0 ? "" : rel.slice(0, i);
  }

  const status = $derived.by(() => {
    if (error) return "";
    const r = result;
    if (!r) return "";
    if (app.searchMode === "files")
      return t("{n} 个文件", { n: r.files.length }) + (r.truncated ? t("（仅显示前 {n} 个）", { n: r.files.length }) : "") + ` · ${r.elapsedMs}ms`;
    return (
      t("{n} 处匹配 · {f} 个文件", { n: r.totalMatches, f: r.content.length }) +
      (r.truncated ? t("（结果过多已截断）") : "") +
      " · " +
      t("扫描 {n} 个文件", { n: r.searchedFiles }) +
      ` · ${r.elapsedMs}ms`
    );
  });
</script>

{#if app.searchOpen}
  <div class="backdrop" role="presentation" onclick={close}></div>
  <div class="panel" role="dialog" aria-label={t("全局搜索")} tabindex="-1" onkeydown={onKey}>
    <div class="tabs">
      <button class:on={app.searchMode === "files"} onclick={() => setMode("files")}>{t("文件")}</button>
      <button class:on={app.searchMode === "content"} onclick={() => setMode("content")}>{t("内容")}</button>
      <span class="hint">{t("Alt+F / Tab 切换 · ↑↓ 选择 · Enter 打开 · Esc 关闭")}</span>
      <span class="spacer"></span>
      <button class="btn ghost sm" title={t("关闭 (Esc)")} onclick={close}><Icon name="close" size={12} /></button>
    </div>
    <div class="qrow">
      <Icon name="search" size={14} />
      <input
        class="q"
        bind:this={inputEl}
        bind:value={query}
        spellcheck="false"
        placeholder={app.searchMode === "files" ? t("按文件名查找（支持模糊匹配，含 / 时匹配路径）") : t("搜索文件内容")}
      />
      {#if busy}<span class="spin"></span>{/if}
      {#if app.searchMode === "content"}
        <button class="tg" class:on={opts.caseSensitive} title={t("区分大小写")} onclick={() => (opts.caseSensitive = !opts.caseSensitive)}>Aa</button>
        <button class="tg" class:on={opts.wholeWord} title={t("全词匹配")} onclick={() => (opts.wholeWord = !opts.wholeWord)}><u>ab</u></button>
        <button class="tg" class:on={opts.regex} title={t("正则表达式")} onclick={() => (opts.regex = !opts.regex)}>.*</button>
      {/if}
      <button class="tg" class:on={opts.useIgnore} title={t("遵循 .gitignore（并跳过 node_modules 等）")} onclick={() => (opts.useIgnore = !opts.useIgnore)}>.git</button>
      <button class="tg" class:on={opts.showFilters || !!opts.include || !!opts.exclude} title={t("包含/排除的文件")} onclick={() => (opts.showFilters = !opts.showFilters)}>⋯</button>
    </div>
    {#if opts.showFilters}
      <div class="frow">
        <input class="input sm" bind:value={opts.include} placeholder={t("包含的文件，例如 *.ts, src/")} spellcheck="false" />
        <input class="input sm" bind:value={opts.exclude} placeholder={t("排除的文件，例如 *.min.js, dist/")} spellcheck="false" />
      </div>
    {/if}
    <div class="list" bind:this={listEl}>
      {#if !root}
        <p class="none">{t("当前项目未绑定目录")}</p>
      {:else if error}
        <p class="none err">{error}</p>
      {:else if !query.trim()}
        <p class="none">{app.searchMode === "files" ? t("输入文件名快速打开") : t("输入关键词搜索项目内所有文本文件")}</p>
      {:else if result && !rows.length}
        <p class="none">{t("没有结果")}</p>
      {/if}
      {#each shown as row, i (i)}
        {#if row.kind === "file"}
          <button class="row file" class:sel={i === sel} onclick={() => activate(row)} title={row.hit.path}>
            <Icon name="file" size={13} />
            <span class="name">{baseName(row.hit.rel)}</span>
            <span class="rel">{#each fileSegs(row.hit) as seg, k (k)}{#if seg.on}<b>{seg.s}</b>{:else}{seg.s}{/if}{/each}</span>
          </button>
        {:else if row.kind === "group"}
          <button class="row group" class:sel={i === sel} onclick={() => activate(row)} title={row.hit.path}>
            <Icon name="file" size={13} />
            <span class="name">{baseName(row.hit.rel)}</span>
            <span class="dir">{dirName(row.hit.rel)}</span>
            <span class="cnt">{row.hit.matches.length}{row.hit.more ? "+" : ""}</span>
          </button>
        {:else}
          <button class="row match" class:sel={i === sel} onclick={() => activate(row)}>
            <span class="ln">{row.m.line}</span>
            <span class="pv">{row.m.preview.slice(0, row.m.pstart)}<mark>{row.m.preview.slice(row.m.pstart, row.m.pstart + row.m.plen)}</mark>{row.m.preview.slice(row.m.pstart + row.m.plen)}</span>
          </button>
        {/if}
      {/each}
      {#if rows.length > limit}
        <button class="more" onclick={() => (limit += 500)}>{t("显示更多（还有 {n} 条）", { n: rows.length - limit })}</button>
      {/if}
    </div>
    <div class="status">
      <span>{status}</span>
      <span class="spacer"></span>
      <span class="root" title={root}>{root}</span>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 95;
    background: rgba(0, 0, 0, 0.18);
  }
  .panel {
    position: fixed;
    top: 56px;
    left: 50%;
    transform: translateX(-50%);
    width: 760px;
    max-width: calc(100vw - 24px);
    max-height: 74vh;
    z-index: 96;
    display: flex;
    flex-direction: column;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    outline: none;
    overflow: hidden;
  }
  .tabs {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 6px 8px 0;
  }
  .tabs > button:not(.btn) {
    border: none;
    background: none;
    color: var(--text-dim);
    padding: 4px 10px;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    font: inherit;
  }
  .tabs > button.on {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .hint {
    color: var(--text-faint);
    font-size: 11px;
    margin-left: 8px;
  }
  .spacer {
    flex: 1;
  }
  .qrow {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 6px 8px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--text-dim);
  }
  .q {
    flex: 1;
    min-width: 0;
    border: none;
    outline: none;
    background: transparent;
    color: var(--text);
    font: inherit;
    padding: 7px 0;
  }
  .tg {
    border: 1px solid transparent;
    background: none;
    color: var(--text-dim);
    border-radius: 4px;
    padding: 1px 5px;
    font-size: 12px;
    font-family: var(--mono);
    cursor: pointer;
  }
  .tg:hover {
    background: var(--bg-elev);
  }
  .tg.on {
    color: var(--accent);
    border-color: var(--accent);
  }
  .spin {
    width: 11px;
    height: 11px;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .frow {
    display: flex;
    gap: 6px;
    margin: 0 8px 6px;
  }
  .frow input {
    flex: 1;
    min-width: 0;
  }
  .list {
    flex: 1;
    min-height: 60px;
    overflow: auto;
    border-top: 1px solid var(--border-soft);
    padding: 4px 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    border: none;
    background: none;
    color: var(--text);
    text-align: left;
    font: inherit;
    padding: 3px 12px;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
  }
  .row:hover {
    background: var(--bg-elev);
  }
  .row.sel {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
  }
  .row .name {
    flex-shrink: 0;
  }
  .row .rel,
  .row .dir {
    color: var(--text-faint);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .row .rel b {
    color: var(--accent);
    font-weight: 600;
  }
  .row.group {
    margin-top: 2px;
  }
  .row.group .name {
    font-weight: 600;
  }
  .row .cnt {
    margin-left: auto;
    flex-shrink: 0;
    font-size: 11px;
    color: var(--text-dim);
    background: var(--bg-elev);
    border-radius: 8px;
    padding: 0 6px;
  }
  .row.match {
    padding-left: 30px;
    font-family: var(--mono);
    font-size: 12px;
  }
  .row .ln {
    flex-shrink: 0;
    min-width: 34px;
    text-align: right;
    color: var(--text-faint);
  }
  .row .pv {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
    min-width: 0;
  }
  .row .pv mark {
    background: color-mix(in srgb, var(--warn) 45%, transparent);
    color: inherit;
    border-radius: 2px;
  }
  .none {
    color: var(--text-faint);
    padding: 10px 14px;
    margin: 0;
  }
  .none.err {
    color: var(--danger);
    white-space: pre-wrap;
  }
  .more {
    display: block;
    margin: 6px auto;
    border: 1px solid var(--border);
    background: none;
    color: var(--text-dim);
    border-radius: var(--radius);
    padding: 3px 12px;
    cursor: pointer;
    font: inherit;
    font-size: 12px;
  }
  .status {
    display: flex;
    gap: 8px;
    padding: 4px 10px;
    border-top: 1px solid var(--border-soft);
    font-size: 11px;
    color: var(--text-faint);
  }
  .status .root {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 50%;
  }
</style>
