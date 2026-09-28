<script lang="ts">
  import { t } from "../i18n";
  // Collapsible file preview/editor pane. Save is debounced.
  import { tick, untrack } from "svelte";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import Icon from "./Icon.svelte";
  import { LANG_LABEL, highlightToBlocks, langForPath, langLoaded, loadLang, loadLangDeep } from "../highlight";

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
  // 查找镜像（<mark>）与着色层（彩色文字）共用同一套对齐逻辑
  function syncMirrorStyle() {
    if (!taEl) return;
    const cs = getComputedStyle(taEl);
    for (const el of [mirrorEl, hlEl]) {
      if (!el) continue;
      for (const k of MIRROR_PROPS) (el.style as unknown as Record<string, string>)[k] = cs[k] as string;
      el.style.borderStyle = "solid";
      el.style.borderColor = "transparent";
    }
    syncScroll();
  }
  function syncScroll() {
    if (!taEl) return;
    for (const el of [mirrorEl, hlEl]) {
      if (!el) continue;
      el.scrollTop = taEl.scrollTop;
      el.scrollLeft = taEl.scrollLeft;
    }
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

  // ================= 代码着色 =================
  // 与查找同样的「镜像」思路：textarea 文字设为透明（光标、选区、输入照常），
  // 其下叠一层逐像素对齐的着色层显示彩色文字。着色层必须与 textarea 内容逐帧一致，
  // 否则会看到错位的字，所以：
  //  - 语法加载后同步着色，与按键在同一帧内完成。着色层按 40 行分块，每次只替换
  //    内容变了的块（通常就一块），DOM 与重排开销和文件大小基本无关；
  //  - 预计着色耗时超过 HL_SYNC_MS 时，编辑期间先隐藏着色层、露出 textarea 自身的文字，
  //    停手 HL_DEBOUNCE 后再着色——大文件打字不卡，只是暂时不带颜色。
  //    预计耗时 = 分词速率（毫秒/字符，滑动平均）× 当前长度 + 增量更新 DOM 的耗时（滑动平均）。
  //    分词与长度成正比、DOM 增量更新基本恒定，分开估计比「上一次总耗时」稳。
  //    计时噪声（冷启动 JIT、GC）只会让样本变慢，所以更快的样本立即采纳、更慢的只缓慢上调；
  //    打开文件时那次分词是冷的（慢 3–5 倍），预计值落在临界区间时空闲再测一次热的；
  //  - 超过 HL_MAX 字符不着色（工具栏提示）；
  //  - 输入法组字期间同样露出 textarea 文字，组字下划线等由浏览器原样绘制。
  const HL_PREF_KEY = "shidrive.editor.highlight";
  const HL_MAX = 300_000;
  const HL_SYNC_MS = 12;
  const HL_DEBOUNCE = 200;
  let hlEl: HTMLDivElement | undefined = $state();
  let hlEnabled = $state(readHlPref());
  let hlReady = $state(false); // 着色层有内容（hlBlocks 非空）
  let hlStale = $state(false);
  let composing = $state(false);
  let langVersion = $state(0); // 语法异步加载完成后 +1，触发重新着色

  function readHlPref(): boolean {
    try {
      return localStorage.getItem(HL_PREF_KEY) !== "0";
    } catch {
      return true;
    }
  }
  function toggleHl() {
    hlEnabled = !hlEnabled;
    try {
      localStorage.setItem(HL_PREF_KEY, hlEnabled ? "1" : "0");
    } catch {}
  }

  const lang = $derived(ed && !ed.binary ? langForPath(ed.path) : null);
  const hlTooBig = $derived(!!lang && (ed?.content.length ?? 0) > HL_MAX);
  const hlShown = $derived(hlReady && !hlStale && !composing);

  let hlTimer: ReturnType<typeof setTimeout> | null = null;
  let tokRate = 0; // 分词速率 ms/字符（0 = 尚未测得）
  let paintMs = 3; // 增量更新 DOM + 排版 ms
  const settle = (old: number, v: number) => (!old || v < old ? v : old * 0.8 + v * 0.2);
  const predictHlMs = (len: number) => tokRate * len + paintMs;
  function measureTokenize(src: string, l: string): number {
    const t0 = performance.now();
    const blocks = highlightToBlocks(src, l);
    const ms = performance.now() - t0;
    if (src.length > 4000) tokRate = settle(tokRate, ms / src.length); // 太短的样本计时噪声大
    return blocks.length;
  }
  // 打开文件后空闲时补测一次热的分词速率（只分词、不动 DOM）
  function scheduleWarmMeasure(path: string, l: string) {
    const run = () => {
      const e = app.editor;
      if (!e || e.path !== path || lang !== l || !hlEnabled) return;
      const p = predictHlMs(e.content.length);
      if (p >= HL_SYNC_MS && p < HL_SYNC_MS * 4) measureTokenize(e.content, l);
    };
    const w = window as Window & { requestIdleCallback?: (cb: () => void, o?: { timeout: number }) => number };
    if (w.requestIdleCallback) w.requestIdleCallback(run, { timeout: 1000 });
    else setTimeout(run, 300);
  }
  let hlKey = ""; // 当前着色内容对应的「路径 + 语言」
  let hlBlocks: string[] = []; // 最新着色结果（每块 HTML）
  let hlPainted: string[] = []; // 已写入 hlPaintedEl 的块
  let hlPaintedEl: HTMLDivElement | undefined;
  const requestedSubs = new Set<string>();

  function clearHl() {
    if (hlTimer) clearTimeout(hlTimer);
    hlTimer = null;
    hlBlocks = [];
    hlReady = false;
    hlStale = false;
    hlKey = "";
  }
  /** 把 hlBlocks 增量写入着色层：只替换变化的块，返回替换的块数。着色层节点换了（开关/切换文件）则全量重写 */
  function paintHl(): number {
    const el = hlEl;
    if (!el) return 0;
    if (el !== hlPaintedEl) {
      el.replaceChildren();
      hlPainted = [];
      hlPaintedEl = el;
    }
    const kids = el.children;
    const n = hlBlocks.length;
    let changed = 0;
    for (let i = 0; i < n; i++) {
      if (hlPainted[i] === hlBlocks[i]) continue;
      changed++;
      let blk = kids[i] as HTMLDivElement | undefined;
      if (!blk) {
        blk = document.createElement("div");
        el.appendChild(blk);
      }
      blk.innerHTML = hlBlocks[i]; // highlight.ts 已对文本做 HTML 转义，只含 <span class="tk-*">
    }
    while (kids.length > n) kids[kids.length - 1].remove();
    hlPainted = hlBlocks;
    return changed;
  }
  // Markdown 代码块等运行时才知道的子语言：加载一次，完成后重新着色
  function onMissingLang(name: string) {
    if (requestedSubs.has(name)) return;
    requestedSubs.add(name);
    void loadLang(name).then((ok) => {
      if (ok) langVersion++;
    });
  }
  function runHighlight() {
    if (hlTimer) clearTimeout(hlTimer);
    hlTimer = null;
    const e = app.editor;
    const l = lang;
    if (!e || e.binary || !l || !hlEnabled || e.content.length > HL_MAX || !langLoaded(l)) {
      clearHl();
      return;
    }
    const isNewKey = `${e.path}\u0000${l}` !== hlKey;
    const t0 = performance.now();
    // textarea 的值把 CRLF 规范成 LF（光标位置也按 LF 算），着色层跟它保持一致
    const src = e.content.includes("\r") ? e.content.replace(/\r\n?/g, "\n") : e.content;
    hlBlocks = highlightToBlocks(src, l, onMissingLang);
    const tokenizeMs = performance.now() - t0;
    if (src.length > 4000) tokRate = settle(tokRate, tokenizeMs / src.length); // 太短的样本计时噪声大
    hlKey = `${e.path}\u0000${l}`;
    hlStale = false;
    if (isNewKey) scheduleWarmMeasure(e.path, l);
    if (hlEl && hlReady) {
      // 常规路径：同步写入并在这里就完成排版，计入耗时（反正绘制前也要排）
      const changed = paintHl();
      void hlEl.scrollHeight;
      // 整层重写（切换文件、打开了块注释）的 DOM 耗时不代表常规按键，不计入 paintMs
      if (changed <= 2) paintMs = settle(paintMs, performance.now() - t0 - tokenizeMs);
      syncScroll();
    } else {
      // 着色层刚出现：等 {#if} 创建节点后再写入、对齐样式
      hlReady = true;
      void tick().then(() => {
        paintHl();
        syncMirrorStyle();
      });
    }
  }

  $effect(() => {
    const content = ed?.content ?? "";
    const path = ed?.path ?? "";
    const l = lang;
    void langVersion;
    if (!hlEnabled || !l || ed?.binary || content.length > HL_MAX) {
      clearHl();
      return;
    }
    if (!langLoaded(l)) {
      clearHl(); // 语法到位前显示纯文本（textarea 自身文字）
      void loadLangDeep(l).then((ok) => {
        if (ok) langVersion++;
      });
      return;
    }
    // 新打开/切换文件：总是立即着色；编辑中：够快就同步，否则防抖
    // runHighlight 读写 hlReady/hlEl，不能让它们成为本 effect 的依赖（依赖已在上面读取）
    if (`${path}\u0000${l}` !== hlKey || predictHlMs(content.length) < HL_SYNC_MS) {
      untrack(runHighlight);
      return;
    }
    hlStale = true;
    if (hlTimer) clearTimeout(hlTimer);
    hlTimer = setTimeout(runHighlight, HL_DEBOUNCE);
  });
  $effect(() => () => {
    if (hlTimer) clearTimeout(hlTimer);
  });

  // 配色明暗跟随编辑区的实际底色，而不是 data-theme：皮肤只给颜色变量、不声明明暗，
  // 深色主题配浅色皮肤（如薄荷汽水）时按 data-theme 选会得到浅底配浅字
  let areaEl: HTMLDivElement | undefined = $state();
  let hlScheme = $state<"dark" | "light">("dark");
  function detectScheme() {
    if (!areaEl) return;
    let light = document.documentElement.dataset.theme === "light";
    const m = getComputedStyle(areaEl).backgroundColor.match(/[\d.]+/g);
    if (m && m.length >= 3 && (m.length < 4 || +m[3] >= 0.5)) {
      const [r, g, b] = m.map(Number);
      light = 0.2126 * r + 0.7152 * g + 0.0722 * b > 140;
    }
    hlScheme = light ? "light" : "dark";
  }
  $effect(() => {
    if (!areaEl || !hlReady) return;
    detectScheme();
    // 切换主题/皮肤：html/body 属性或 <head> 里的皮肤样式变化时重新判断（按帧合并）
    let raf = 0;
    const mo = new MutationObserver(() => {
      if (!raf) raf = requestAnimationFrame(() => ((raf = 0), detectScheme()));
    });
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "style", "class"] });
    mo.observe(document.body, { attributes: true, attributeFilter: ["data-skin", "style", "class"] });
    mo.observe(document.head, { childList: true, subtree: true, characterData: true });
    return () => {
      mo.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  });

  const hlTitle = $derived(
    hlTooBig
      ? t("文件较大（超过 300 KB），已关闭代码着色")
      : hlEnabled
        ? t("代码着色：开（点击关闭）")
        : t("代码着色：关（点击开启）"),
  );
</script>

<!-- 根节点可聚焦 + 键盘监听：让 Ctrl+F/F3/Esc 在焦点位于编辑器任意位置时都归文件查找 -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div class="editor" data-find-scope="editor" tabindex="-1" role="region" aria-label={t("文件查看器")} onkeydown={onRootKeydown}>
  <div class="pbar">
    <Icon name="file" size={13} />
    <span class="ppath" title={ed.path}>{relPath(ed.path)}</span>
    {#if ed.dirty}<span class="badge warn">{t("未保存")}</span>{/if}
    <span class="spacer"></span>
    {#if !ed.binary && lang}
      <!-- 语言名 + 着色开关（偏好存 localStorage，默认开） -->
      <button
        class="btn ghost sm lang"
        class:off={!hlEnabled || hlTooBig}
        title={hlTitle}
        aria-pressed={hlEnabled}
        onclick={toggleHl}
      >{LANG_LABEL[lang] ?? lang}</button>
    {/if}
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
    <div class="area" bind:this={areaEl}>
      {#if mirrorHtml}
        <!-- 高亮镜像：内容已逐段 HTML 转义，只插入 <mark> -->
        <div class="mirror" class:stale bind:this={mirrorEl} aria-hidden="true">{@html mirrorHtml}</div>
      {/if}
      {#if hlReady}
        <!-- 着色层：内容由 paintHl() 按块增量写入 -->
        <div class="hl scheme-{hlScheme}" class:hidden={!hlShown} bind:this={hlEl} aria-hidden="true"></div>
      {/if}
      <textarea
        class="editor-area"
        class:hl-on={hlShown}
        bind:this={taEl}
        bind:value={ed.content}
        oninput={queueSave}
        onscroll={syncScroll}
        oncompositionstart={() => (composing = true)}
        oncompositionend={() => (composing = false)}
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
  /* 查找镜像与着色层都在 textarea 下面，textarea 必须透明。皮肤的全局规则
     body[data-skin] :is(textarea) { background-color: var(--bg-elev) } 优先级更高，
     会把下面两层整个盖住——这里用更高的优先级保持透明，把皮肤给 textarea 的底色挪到容器上，外观不变 */
  .area > textarea.editor-area {
    background-color: transparent;
  }
  :global(body[data-skin]:not([data-skin=""])) .area {
    background-color: var(--bg-elev);
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
  .lang {
    font-size: 0.78em;
    color: var(--text-dim);
  }
  .lang.off {
    color: var(--text-faint);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--text-faint) 60%, transparent);
  }
  /* 着色层：位于查找镜像之上、textarea 之下；背景透明，查找的 <mark> 底色能透上来 */
  .hl {
    position: absolute;
    inset: 0;
    overflow: hidden;
    scrollbar-gutter: stable;
    white-space: pre-wrap;
    overflow-wrap: break-word;
    color: var(--text);
    pointer-events: none;
    user-select: none;
  }
  .hl.hidden {
    visibility: hidden;
  }
  .editor-area.hl-on {
    color: transparent;
    caret-color: var(--text);
  }
  .editor-area.hl-on::selection {
    color: transparent;
    background: color-mix(in srgb, var(--accent) 32%, transparent);
  }
  /* 默认配色：深底 One Dark 风格、浅底 GitHub 风格。皮肤可定义 --hl-kwd 等变量覆盖 */
  .hl.scheme-dark {
    --hlx-kwd: #c678dd;
    --hlx-str: #98c379;
    --hlx-cmnt: #7f848e;
    --hlx-num: #d19a66;
    --hlx-func: #61afef;
    --hlx-type: #e5c07b;
    --hlx-class: #e5c07b;
    --hlx-var: #e06c75;
    --hlx-oper: #56b6c2;
    --hlx-bool: #d19a66;
    --hlx-esc: #56b6c2;
    --hlx-section: #e06c75;
    --hlx-insert: #98c379;
    --hlx-deleted: #e06c75;
    --hlx-err: #f44747;
  }
  .hl.scheme-light {
    --hlx-kwd: #cf222e;
    --hlx-str: #0a3069;
    --hlx-cmnt: #6e7781;
    --hlx-num: #0550ae;
    --hlx-func: #8250df;
    --hlx-type: #953800;
    --hlx-class: #953800;
    --hlx-var: #116329;
    --hlx-oper: #0550ae;
    --hlx-bool: #0550ae;
    --hlx-esc: #0a3069;
    --hlx-section: #0550ae;
    --hlx-insert: #116329;
    --hlx-deleted: #82071e;
    --hlx-err: #cf222e;
  }
  .hl :global(.tk-kwd) { color: var(--hl-kwd, var(--hlx-kwd)); }
  .hl :global(.tk-str) { color: var(--hl-str, var(--hlx-str)); }
  .hl :global(.tk-cmnt) { color: var(--hl-cmnt, var(--hlx-cmnt)); }
  .hl :global(.tk-num) { color: var(--hl-num, var(--hlx-num)); }
  .hl :global(.tk-func) { color: var(--hl-func, var(--hlx-func)); }
  .hl :global(.tk-type) { color: var(--hl-type, var(--hlx-type)); }
  .hl :global(.tk-class) { color: var(--hl-class, var(--hlx-class)); }
  .hl :global(.tk-var) { color: var(--hl-var, var(--hlx-var)); }
  .hl :global(.tk-oper) { color: var(--hl-oper, var(--hlx-oper)); }
  .hl :global(.tk-bool) { color: var(--hl-bool, var(--hlx-bool)); }
  .hl :global(.tk-esc) { color: var(--hl-esc, var(--hlx-esc)); }
  .hl :global(.tk-section) { color: var(--hl-section, var(--hlx-section)); }
  .hl :global(.tk-insert) { color: var(--hl-insert, var(--hlx-insert)); }
  .hl :global(.tk-deleted) { color: var(--hl-deleted, var(--hlx-deleted)); }
  .hl :global(.tk-err) { color: var(--hl-err, var(--hlx-err)); }
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
