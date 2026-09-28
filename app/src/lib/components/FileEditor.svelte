<script lang="ts">
  import { t } from "../i18n";
  // Collapsible file preview/editor pane. Save is debounced.
  import { tick } from "svelte";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import Icon from "./Icon.svelte";

  const ed = $derived(app.editor!);
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  function queueSave() {
    if (!app.editor) return;
    app.editor.dirty = true;
    if (saveTimer) clearTimeout(saveTimer);
    // snapshot at queue time: if the user opens another file before the timer
    // fires, these edits must land on the file they were typed into
    const snap = { path: app.editor.path, content: app.editor.content };
    saveTimer = setTimeout(async () => {
      try {
        await api.fsWrite(snap.path, snap.content);
        if (app.editor?.path === snap.path) app.editor.dirty = false;
      } catch (e) {
        toast("error", String(e));
      }
    }, 500);
  }

  function relPath(p: string): string {
    const projRoot = app.projects.find((x) => x.id === app.projectId)?.root_path ?? "";
    return projRoot && p.startsWith(projRoot) ? p.slice(projRoot.length + 1) : p;
  }

  function collapse() {
    if (app.editor) app.editor.collapsed = true;
  }

  // ================= Ctrl+F 查找 =================
  // 轻量实现：不引入编辑器库。textarea 背后叠一层与之逐像素对齐的「镜像」div
  // （同字体/内边距/换行规则，随 textarea 同步滚动），在镜像里用 <mark> 标出所有命中；
  // textarea 背景透明，文字照常可编辑。当前命中项的像素位置也直接从镜像里取，用来滚动定位。
  //
  // Ctrl+F 归属：主会话在 window 上监听 Ctrl+F。这里在编辑器根节点上先处理并
  // stopPropagation，所以焦点在编辑器内（含点击工具栏空白处，根节点 tabindex=-1 可聚焦）
  // 时打开的是文件查找；焦点在会话区时仍是会话搜索。
  const MAX_HITS = 5000;
  let taEl: HTMLTextAreaElement | undefined = $state();
  let mirrorEl: HTMLDivElement | undefined = $state();
  let findInput: HTMLInputElement | undefined = $state();
  let findOpen = $state(false);
  let query = $state("");
  let caseSensitive = $state(false);
  let cur = $state(-1);

  // 性能：镜像要把全文再排版一遍，大文件上每次重建有几百毫秒。所以
  //  - 编辑内容时先隐藏镜像（stale），停手 EDIT_DEBOUNCE 后再按新内容重建——打字不逐键卡顿；
  //  - 查找词输入对大文件（> BIG_TEXT 字符）防抖；
  //  - 上一个/下一个只切换 class + 读已排版位置，不重建，任何大小都即时。
  const EDIT_DEBOUNCE = 250;
  const BIG_TEXT = 300_000;
  let findText = $state(""); // 参与查找的文本快照（与 textarea 内容一致时 stale=false）
  let stale = $state(false);
  let appliedQuery = $state("");
  let editTimer: ReturnType<typeof setTimeout> | null = null;
  let queryTimer: ReturnType<typeof setTimeout> | null = null;
  function syncFindText() {
    if (editTimer) clearTimeout(editTimer);
    editTimer = null;
    findText = !ed || ed.binary ? "" : ed.content;
    stale = false;
  }
  // 内容变化：查找打开时标记过期并防抖重建（打开/切换文件时立即同步）
  let lastSyncedPath = "";
  $effect(() => {
    const content = ed?.content ?? "";
    const path = ed?.path ?? "";
    if (!findOpen) return;
    if (path !== lastSyncedPath || !findText) {
      lastSyncedPath = path;
      syncFindText();
      return;
    }
    if (content === findText) return;
    stale = true;
    if (editTimer) clearTimeout(editTimer);
    editTimer = setTimeout(syncFindText, EDIT_DEBOUNCE);
  });
  // 查找词：小文件即时生效，大文件防抖
  $effect(() => {
    const q = query;
    if (queryTimer) clearTimeout(queryTimer);
    if (!q || (ed?.content.length ?? 0) < BIG_TEXT) {
      appliedQuery = q;
      return;
    }
    queryTimer = setTimeout(() => (appliedQuery = q), EDIT_DEBOUNCE);
  });

  const escRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const escHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  const findRe = $derived(findOpen && appliedQuery ? new RegExp(escRe(appliedQuery), caseSensitive ? "g" : "gi") : null);
  // 所有命中的起止位置（正则逐个扫描，偏移与原文严格一致；超过上限只标前 MAX_HITS 个）
  const hits = $derived.by(() => {
    const re = findRe;
    const text = findText;
    const out: { s: number; e: number }[] = [];
    if (!re || !text) return { list: out, more: false };
    re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text))) {
      out.push({ s: m.index, e: m.index + m[0].length });
      if (out.length >= MAX_HITS) return { list: out, more: true };
    }
    return { list: out, more: false };
  });
  // 镜像 HTML：只在有命中时生成（没有命中就不渲染镜像，零开销）
  const mirrorHtml = $derived.by(() => {
    const list = hits.list;
    if (!list.length) return "";
    const text = findText;
    let out = "";
    let pos = 0;
    for (let i = 0; i < list.length; i++) {
      const h = list[i];
      out += escHtml(text.slice(pos, h.s)) + `<mark data-i="${i}">` + escHtml(text.slice(h.s, h.e)) + "</mark>";
      pos = h.e;
    }
    // 文本以换行结尾时 textarea 会多显示一个空行，而 div 不会：补一个空格撑出这一行
    return out + escHtml(text.slice(pos)) + (text.endsWith("\n") ? " " : "");
  });
  const curShown = $derived(cur >= 0 && cur < hits.list.length ? cur : -1);

  // 镜像的排版必须与 textarea 完全一致（全局样式/皮肤可能改字体），直接拷贝计算样式
  const MIRROR_PROPS = [
    "fontFamily", "fontSize", "fontWeight", "fontStyle", "lineHeight", "letterSpacing", "wordSpacing",
    "tabSize", "textIndent", "textTransform", "whiteSpace", "wordBreak", "overflowWrap",
    "paddingTop", "paddingRight", "paddingBottom", "paddingLeft",
    "borderTopWidth", "borderRightWidth", "borderBottomWidth", "borderLeftWidth", "boxSizing",
  ] as const;
  function syncMirrorStyle() {
    if (!taEl || !mirrorEl) return;
    const cs = getComputedStyle(taEl);
    for (const k of MIRROR_PROPS) (mirrorEl.style as unknown as Record<string, string>)[k] = cs[k] as string;
    mirrorEl.style.borderStyle = "solid";
    mirrorEl.style.borderColor = "transparent";
    syncScroll();
  }
  function syncScroll() {
    if (!taEl || !mirrorEl) return;
    mirrorEl.scrollTop = taEl.scrollTop;
    mirrorEl.scrollLeft = taEl.scrollLeft;
  }
  // 镜像出现/内容变化后：对齐样式与滚动，并重新标出当前项
  // 注意要等 tick()：{@html} 的 DOM 替换可能晚于本 effect，先标上的 .cur 会被新 HTML 整个冲掉
  function applyCur() {
    if (!mirrorEl) return;
    syncMirrorStyle();
    mirrorEl.querySelector("mark.cur")?.classList.remove("cur");
    const i = curShown;
    if (i >= 0) mirrorEl.querySelector(`mark[data-i="${i}"]`)?.classList.add("cur");
  }
  $effect(() => {
    void mirrorHtml;
    void curShown;
    if (!mirrorEl) return;
    void tick().then(applyCur);
  });

  /** 把第 i 个命中滚动到可视区域，并在 textarea 里选中它（关闭查找后光标就停在那里） */
  async function reveal(i: number) {
    if (stale) return; // 位置基于旧文本：等重建后再定位
    const h = hits.list[i];
    if (!h || !taEl) return;
    taEl.setSelectionRange(h.s, h.e);
    await tick();
    const m = mirrorEl?.querySelector<HTMLElement>(`mark[data-i="${i}"]`);
    if (!m || !taEl) return;
    const top = m.offsetTop;
    const view = taEl.clientHeight;
    const margin = Math.min(80, view / 4);
    if (top < taEl.scrollTop + margin || top + m.offsetHeight > taEl.scrollTop + view - margin) {
      taEl.scrollTop = Math.max(0, top - view / 3);
      syncScroll();
    }
    const left = m.offsetLeft;
    if (left < taEl.scrollLeft || left > taEl.scrollLeft + taEl.clientWidth - 40) {
      taEl.scrollLeft = Math.max(0, left - taEl.clientWidth / 2);
      syncScroll();
    }
  }

  // 查询/选项/文件变化：从光标处向后找第一个命中（与常见编辑器一致）；
  // 编辑内容本身不触发跳转，只刷新高亮
  let lastFindKey = "";
  $effect(() => {
    const key = `${findRe?.source ?? ""}\u0000${caseSensitive}\u0000${ed?.path ?? ""}`;
    const list = hits.list;
    if (key === lastFindKey) return;
    lastFindKey = key;
    if (!list.length) {
      cur = -1;
      return;
    }
    const caret = taEl ? Math.min(taEl.selectionStart, taEl.selectionEnd) : 0;
    let i = list.findIndex((h) => h.s >= caret);
    if (i < 0) i = 0;
    cur = i;
    void reveal(i);
  });

  async function step(dir: 1 | -1) {
    if (stale) {
      syncFindText(); // 正在编辑中按了 Enter/F3：先按最新内容重建
      await tick();
    }
    const n = hits.list.length;
    if (!n) return;
    cur = ((curShown < 0 ? (dir > 0 ? -1 : 0) : curShown) + dir + n) % n;
    void reveal(cur);
  }

  async function openFind() {
    if (!ed || ed.binary) return;
    // 选中了单行短文本：作为查找词（与 VS Code 一致）
    if (taEl && document.activeElement === taEl) {
      const sel = taEl.value.slice(taEl.selectionStart, taEl.selectionEnd);
      if (sel && sel.length <= 200 && !sel.includes("\n")) {
        lastFindKey = ""; // 查找词相同也重新定位
        query = sel;
        taEl.setSelectionRange(taEl.selectionStart, taEl.selectionStart); // 从选区起点开始找
      }
    }
    findOpen = true;
    appliedQuery = query; // 重新打开时不等防抖
    syncFindText();
    lastSyncedPath = ed.path;
    await tick();
    findInput?.focus();
    findInput?.select();
  }

  function closeFind(focusEditor = true) {
    findOpen = false;
    findText = ""; // 释放快照（大文件时是一整份副本）
    stale = false;
    cur = -1;
    lastFindKey = "";
    if (focusEditor && taEl) {
      const keep = { s: taEl.selectionStart, e: taEl.selectionEnd, top: taEl.scrollTop };
      taEl.focus();
      // 聚焦可能让浏览器把视口滚到光标处之外的位置：还原到查找时的视口
      taEl.setSelectionRange(keep.s, keep.e);
      taEl.scrollTop = keep.top;
    }
  }

  function onRootKeydown(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") {
      e.preventDefault();
      e.stopPropagation(); // 不让主会话的全局 Ctrl+F 再处理
      void openFind();
    } else if (e.key === "F3" && findOpen) {
      e.preventDefault();
      e.stopPropagation();
      step(e.shiftKey ? -1 : 1);
    } else if (e.key === "Escape" && findOpen) {
      e.preventDefault();
      e.stopPropagation();
      closeFind();
    }
  }

  function onFindKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      step(e.shiftKey ? -1 : 1);
    }
  }

  // 切换文件时保留查找框，但不沿用上一个文件的当前项
  let lastPath = "";
  $effect(() => {
    const p = ed?.path ?? "";
    if (p !== lastPath) {
      lastPath = p;
      cur = -1;
    }
  });
</script>

<!-- 根节点可聚焦 + 键盘监听：让 Ctrl+F/F3/Esc 在焦点位于编辑器任意位置时都归文件查找 -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div class="editor" data-find-scope="editor" tabindex="-1" role="region" aria-label={t("文件查看器")} onkeydown={onRootKeydown}>
  <div class="pbar">
    <Icon name="file" size={13} />
    <span class="ppath" title={ed.path}>{relPath(ed.path)}</span>
    {#if ed.dirty}<span class="badge warn">{t("未保存")}</span>{/if}
    <span class="spacer"></span>
    {#if !ed.binary}
      <button class="btn ghost sm" title={t("查找 (Ctrl+F)")} onclick={() => void openFind()}>⌕</button>
    {/if}
    <button class="btn ghost sm" title={t("在资源管理器中显示")} onclick={() => api.fsOpenExplorer(ed.path).catch((e) => toast("error", String(e)))}><Icon name="folder" size={13} /></button>
    <button class="btn ghost sm" title={t("收起（文件树中可再次打开）")} onclick={collapse}><Icon name="close" size={13} /></button>
  </div>
  {#if findOpen && !ed.binary}
    <div class="findbar" role="search">
      <input
        bind:this={findInput}
        bind:value={query}
        onkeydown={onFindKeydown}
        placeholder={t("在文件中查找…")}
        spellcheck="false"
      />
      <span class="fcount" class:none={!!query && !hits.list.length}>
        {#if !query}
          &nbsp;
        {:else if !hits.list.length}
          {t("无结果")}
        {:else}
          {curShown + 1}/{hits.list.length}{hits.more ? "+" : ""}
        {/if}
      </span>
      <button class="btn ghost sm" class:on={caseSensitive} title={t("区分大小写")} aria-pressed={caseSensitive} onclick={() => (caseSensitive = !caseSensitive)}>Aa</button>
      <button class="btn ghost sm" title={t("上一个 (Shift+Enter)")} disabled={!hits.list.length} onclick={() => step(-1)}>↑</button>
      <button class="btn ghost sm" title={t("下一个 (Enter)")} disabled={!hits.list.length} onclick={() => step(1)}>↓</button>
      <button class="btn ghost sm" title={t("关闭 (Esc)")} onclick={() => closeFind()}><Icon name="close" size={12} /></button>
    </div>
  {/if}
  {#if ed.binary}
    <div class="empty">{t("无法预览二进制文件")}</div>
  {:else}
    <div class="area">
      {#if mirrorHtml}
        <!-- 高亮镜像：内容已逐段 HTML 转义，只插入 <mark> -->
        <div class="mirror" class:stale bind:this={mirrorEl} aria-hidden="true">{@html mirrorHtml}</div>
      {/if}
      <textarea
        class="editor-area"
        bind:this={taEl}
        bind:value={ed.content}
        oninput={queueSave}
        onscroll={syncScroll}
        spellcheck="false"
      ></textarea>
    </div>
  {/if}
</div>

<style>
  .editor {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    background: var(--bg);
    outline: none;
  }
  .pbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border-soft);
    color: var(--text-dim);
  }
  .ppath {
    font-size: 0.85em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spacer {
    flex: 1;
  }
  .findbar {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 5px 10px;
    border-bottom: 1px solid var(--border-soft);
    background: var(--bg-panel);
  }
  .findbar input {
    flex: 1;
    min-width: 80px;
    max-width: 320px;
    padding: 3px 8px;
    font-size: 0.85em;
  }
  .fcount {
    min-width: 64px;
    padding: 0 6px;
    color: var(--text-faint);
    font-size: 0.8em;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .fcount.none {
    color: var(--danger, #e5534b);
  }
  .findbar .on {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    color: var(--accent);
  }
  .area {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    background: var(--bg);
  }
  .editor-area {
    position: relative;
    z-index: 1;
    flex: 1;
    border: none;
    border-radius: 0;
    background: transparent;
    font-family: var(--mono);
    font-size: 0.86em;
    line-height: 1.6;
    padding: 12px 16px;
    resize: none;
    user-select: text;
    /* 镜像也预留同宽滚动条槽：有无滚动条时两边的换行宽度都一致 */
    scrollbar-gutter: stable;
  }
  .mirror {
    position: absolute;
    inset: 0;
    overflow: hidden;
    scrollbar-gutter: stable;
    white-space: pre-wrap;
    overflow-wrap: break-word;
    color: transparent;
    pointer-events: none;
    user-select: none;
  }
  .mirror.stale {
    visibility: hidden;
  }
  .mirror :global(mark) {
    color: transparent;
    background: color-mix(in srgb, var(--warn, #d4a72c) 38%, transparent);
    border-radius: 2px;
  }
  .mirror :global(mark.cur) {
    background: color-mix(in srgb, var(--accent) 60%, transparent);
    box-shadow: 0 0 0 1px var(--accent);
  }
</style>
