<script lang="ts">
  // 全局助手：无皮肤 → 左下角简易助手；有皮肤人物 → 人物右上方的对话气泡
  // （牧濑红莉栖·动态 用 Amadeus 主题，其它皮肤用通用轻量气泡）。
  import { onMount, tick } from "svelte";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { md } from "../markdown";
  import { t } from "../i18n";
  import { confirmDialog } from "../dialog.svelte";
  import { onSkinEvent } from "../skin-fx/bus";
  import { ga, enabled, persona, initAssistant, send, cancel, resetSession, hideBubble, loadMemories, summarize, visibleText, isThinking, openComposer, memoNote, clearMessages, settingsRequest } from "../assistant/store.svelte";

  const p = $derived(persona());
  const style = $derived(p.style);
  const title = $derived(p.title || p.name);

  // ---------------- 位置：贴着人物舞台 ----------------
  let stage = $state<{ left: number; top: number; width: number; height: number } | null>(null);
  let winH = $state(innerHeight);
  function measure() {
    winH = innerHeight;
    const el = document.querySelector(".skin-character-stage");
    const r = el?.getBoundingClientRect();
    stage = r && r.width > 40 && r.height > 40 ? { left: r.left, top: r.top, width: r.width, height: r.height } : null;
  }
  $effect(() => {
    void app.skin; void style;
    let ro: ResizeObserver | null = null;
    const id = requestAnimationFrame(() => {
      measure();
      const el = document.querySelector(".skin-character-stage");
      if (el) { ro = new ResizeObserver(measure); ro.observe(el); }
    });
    return () => { cancelAnimationFrame(id); ro?.disconnect(); };
  });
  /** 实际呈现样式：皮肤人物被隐藏（窗口太矮）时退回左下角简易助手 */
  const mode = $derived(style === "plain" || !stage ? "plain" : style);
  const pos = $derived.by(() => {
    if (mode === "plain" || !stage) return { left: 14, bottom: 62 };
    const [ax, ay] = p.anchor ?? [62, 78];
    const left = Math.round(stage.left + (stage.width * ax) / 100);
    const bottom = Math.round(winH - (stage.top + stage.height) + (stage.height * ay) / 100);
    return { left: Math.max(8, left), bottom: Math.min(Math.max(8, bottom), winH - 160) };
  });

  // ---------------- 卡片内容 ----------------
  const cardOpen = $derived(ga.bubble.visible || ga.composer || ga.panel !== null);
  const streamingText = $derived(visibleText(ga.streaming));
  const page = $derived(ga.bubble.pages[ga.bubble.index] ?? "");
  const pageCount = $derived(ga.bubble.pages.length);
  const busy = $derived(ga.busy === "send" || ga.busy === "greet" || ga.busy === "reset");
  const divergence = $derived.by(() => {
    // Amadeus 头部的世界线变动率：随消息数轻微跳动，只是装饰
    const n = ga.messages.length;
    return (1.048596 + ((n * 7919) % 1000) / 1e6).toFixed(6);
  });

  let input = $state("");
  let inputEl = $state<HTMLTextAreaElement | null>(null);
  let bodyEl = $state<HTMLDivElement | null>(null);
  let hovered = $state(false);

  $effect(() => {
    void ga.focusRev;
    if (ga.composer) void tick().then(() => inputEl?.focus());
  });

  // 流式时贴底
  $effect(() => {
    void streamingText;
    if (bodyEl && ga.streaming) bodyEl.scrollTop = bodyEl.scrollHeight;
  });

  // 自动翻页（悬停暂停）与自动收起
  $effect(() => {
    void ga.bubble.rev;
    const idx = ga.bubble.index, n = pageCount, text = page;
    if (!ga.bubble.visible || busy || hovered || ga.composer || ga.panel) return;
    if (idx < n - 1) {
      const ms = Math.min(12000, Math.max(3500, 1200 + text.length * 70));
      const id = setTimeout(() => { if (ga.bubble.index === idx) ga.bubble.index = idx + 1; }, ms);
      return () => clearTimeout(id);
    }
    const secs = ga.view?.config.bubble_seconds ?? 30;
    if (!secs || ga.bubble.error) return;
    const id = setTimeout(() => hideBubble(), secs * 1000 + Math.min(8000, text.length * 40));
    return () => clearTimeout(id);
  });

  // 记忆提示：显示 4 秒
  let memoShown = $state("");
  $effect(() => {
    void memoNote.rev;
    if (!memoNote.text) return;
    memoShown = memoNote.text;
    const id = setTimeout(() => (memoShown = ""), 4000);
    return () => clearTimeout(id);
  });

  function prev() { if (ga.bubble.index > 0) ga.bubble.index--; }
  function next() { if (ga.bubble.index < pageCount - 1) ga.bubble.index++; }

  function close() {
    ga.composer = false;
    ga.panel = null;
    hideBubble();
  }

  async function submit() {
    const text = input.trim();
    if (!text || busy) return;
    input = "";
    ga.panel = ga.panel === "chat" ? "chat" : null;
    await send(text);
  }

  function onKey(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void submit(); }
    else if (e.key === "Escape") { e.preventDefault(); ga.composer = false; if (!ga.panel && !ga.bubble.visible) close(); }
  }

  function openPanel(tab: "chat" | "memory") {
    ga.panel = ga.panel === tab ? null : tab;
    if (ga.panel === "memory") void loadMemories();
    if (ga.panel === "chat") void tick().then(() => { if (listEl) listEl.scrollTop = listEl.scrollHeight; });
  }

  function onLauncher() {
    if (cardOpen) close();
    else onChip();
  }

  function onChip() {
    if (ga.bubble.pages.length || ga.bubble.error || busy) ga.bubble.visible = true;
    openComposer();
  }

  async function newSession() {
    const ok = await confirmDialog({
      title: t("新建长期会话"),
      message: t("将为助手新开一个长期会话（对话记录和长期记忆会保留，新会话会带上记忆与最近对话回顾）。继续吗？"),
      confirmText: t("新建"),
    });
    if (ok) await resetSession();
  }

  function openSettings() {
    settingsRequest.tab = "assistant";
    app.settingsOpen = true;
  }

  // 链接与本地路径：交给系统打开，不在应用窗口里跳转
  function onMdClick(e: MouseEvent) {
    const a = (e.target as Element).closest("a");
    if (!a) return;
    e.preventDefault();
    const path = a.getAttribute("data-path");
    const href = a.getAttribute("href") ?? "";
    const target = path || (/^https?:\/\//i.test(href) ? href : "");
    if (target) void api.fsOpenDefault(target).catch((err) => toast("error", String(err)));
  }

  // ---------------- 记忆 ----------------
  let listEl = $state<HTMLDivElement | null>(null);
  let memDraft = $state("");
  let editing = $state<{ id: string; content: string } | null>(null);
  let memBusy = $state(false);

  async function addMemory() {
    const c = memDraft.trim();
    if (!c) return;
    try {
      const ok = await api.assistantMemoryAdd(c, false);
      if (!ok) toast("info", t("已有相同的记忆"));
      memDraft = "";
      await loadMemories();
    } catch (e) { toast("error", String(e)); }
  }
  async function saveEdit() {
    if (!editing) return;
    const m = ga.memories.find((x) => x.id === editing!.id);
    try {
      await api.assistantMemoryUpdate(editing.id, editing.content, m?.pinned ?? false);
      editing = null;
      await loadMemories();
    } catch (e) { toast("error", String(e)); }
  }
  async function togglePin(id: string, content: string, pinned: boolean) {
    await api.assistantMemoryUpdate(id, content, !pinned).catch((e) => toast("error", String(e)));
    await loadMemories();
  }
  async function delMemory(id: string) {
    await api.assistantMemoryDelete(id).catch((e) => toast("error", String(e)));
    await loadMemories();
  }
  async function doSummarize(mode: "consolidate" | "extract") {
    if (mode === "consolidate") {
      const ok = await confirmDialog({
        title: t("整理记忆"),
        message: t("让助手合并重复、删除过时的记忆（置顶的记忆不会改动）。整理结果会替换现有的非置顶记忆，继续吗？"),
        confirmText: t("整理"),
      });
      if (!ok) return;
    }
    memBusy = true;
    const n = await summarize(mode);
    memBusy = false;
    if (n !== null) toast("ok", mode === "consolidate" ? t("整理完成：现有 {n} 条记忆", { n }) : n ? t("提炼出 {n} 条新记忆", { n }) : t("没有发现新的值得记住的内容"));
  }
  async function doClear() {
    const ok = await confirmDialog({ title: t("清空对话记录"), message: t("清空助手的全部对话记录？长期记忆不受影响。"), danger: true, confirmText: t("清空") });
    if (ok) await clearMessages().catch((e) => toast("error", String(e)));
  }

  const srcLabel: Record<string, string> = { auto: "自动", manual: "手动", summary: "整理" };
  const fmtTime = (s: string) => s.replace("T", " ").slice(5, 16);

  onMount(() => {
    void initAssistant();
    const off = onSkinEvent((ev) => { if (ev === "character.click" && enabled()) onChip(); });
    const onResize = () => measure();
    addEventListener("resize", onResize);
    return () => { off(); removeEventListener("resize", onResize); };
  });
</script>

{#if enabled()}
  <div
    class="ga-root ga-{mode}"
    style="--ga-left:{pos.left}px; --ga-bottom:{pos.bottom}px"
    role="complementary"
    aria-label={t("全局助手")}
  >
    {#if mode === "plain"}
      <button class="ga-launcher" class:on={cardOpen} class:busy title={t("全局助手（Alt+A）")} onclick={onLauncher}>
        <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true"><path fill="currentColor" d="M12 3C6.9 3 3 6.4 3 10.6c0 2.3 1.2 4.4 3.1 5.8L5.4 20l4-2.1c.8.2 1.7.3 2.6.3 5.1 0 9-3.4 9-7.6S17.1 3 12 3Zm-4 8.8a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4Zm4 0a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4Zm4 0a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4Z"/></svg>
      </button>
    {/if}

    {#if cardOpen}
      <div
        class="ga-card"
        class:wide={ga.panel !== null}
        role="dialog"
        tabindex="-1"
        aria-label={title}
        onmouseenter={() => (hovered = true)}
        onmouseleave={() => (hovered = false)}
      >
        {#if mode === "amadeus"}<span class="ga-corner tl"></span><span class="ga-corner tr"></span><span class="ga-corner bl"></span><span class="ga-corner br"></span>{/if}
        <header class="ga-head">
          {#if mode === "amadeus"}
            <span class="ga-led" class:busy></span>
            <span class="ga-title">{title}</span>
            <span class="ga-nixie" title={t("世界线变动率")}>{divergence}</span>
          {:else}
            <span class="ga-title">{title}</span>
          {/if}
          <span class="ga-grow"></span>
          <button class="ga-icon" class:on={ga.panel === "chat"} title={t("对话记录")} onclick={() => openPanel("chat")}>☰</button>
          <button class="ga-icon" class:on={ga.panel === "memory"} title={t("长期记忆")} onclick={() => openPanel("memory")}>✦</button>
          <button class="ga-icon" title={t("关闭")} onclick={close}>✕</button>
        </header>

        {#if ga.panel === "chat"}
          <div class="ga-list" bind:this={listEl}>
            {#each ga.messages as m (m.id)}
              <div class="ga-msg {m.role}">
                <div class="ga-msg-meta">{m.role === "user" ? t("你") : p.name} · {fmtTime(m.created_at)}{m.interrupted ? ` · ${t("已停止")}` : ""}</div>
                <!-- svelte-ignore a11y_click_events_have_key_events -->
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div class="ga-md" onclick={onMdClick}>{@html md(m.content)}</div>
              </div>
            {:else}
              <div class="ga-empty">{t("还没有对话。")}</div>
            {/each}
            {#if busy}
              <div class="ga-msg assistant"><div class="ga-msg-meta">{p.name}</div><div class="ga-md">{#if streamingText}{@html md(streamingText)}{:else}<span class="ga-dots"><i></i><i></i><i></i></span>{/if}</div></div>
            {/if}
          </div>
          <div class="ga-tools">
            <button class="ga-btn" disabled={!!ga.busy} onclick={newSession}>{t("新建长期会话")}</button>
            <button class="ga-btn" disabled={!!ga.busy || !ga.messages.length} onclick={doClear}>{t("清空记录")}</button>
            <span class="ga-grow"></span>
            <button class="ga-btn" onclick={openSettings}>{t("设置")}</button>
          </div>
        {:else if ga.panel === "memory"}
          <div class="ga-list">
            <p class="ga-hint">{t("长期记忆独立于各客户端会话保存，切换客户端或新建会话后仍然有效。")}</p>
            {#each ga.memories as m (m.id)}
              <div class="ga-mem" class:pinned={m.pinned}>
                {#if editing?.id === m.id}
                  <textarea class="ga-input" rows="2" bind:value={editing.content} onkeydown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); void saveEdit(); } else if (e.key === "Escape") editing = null; }}></textarea>
                  <div class="ga-mem-acts"><button class="ga-btn" onclick={saveEdit}>{t("保存")}</button><button class="ga-btn" onclick={() => (editing = null)}>{t("取消")}</button></div>
                {:else}
                  <div class="ga-mem-text">{m.content}</div>
                  <div class="ga-mem-acts">
                    <span class="ga-mem-src">{t(srcLabel[m.source] ?? m.source)} · {fmtTime(m.updated_at)}</span>
                    <span class="ga-grow"></span>
                    <button class="ga-icon sm" class:on={m.pinned} title={m.pinned ? t("取消置顶") : t("置顶（总是带上，整理时不改动）")} onclick={() => togglePin(m.id, m.content, m.pinned)}>📌</button>
                    <button class="ga-icon sm" title={t("编辑")} onclick={() => (editing = { id: m.id, content: m.content })}>✎</button>
                    <button class="ga-icon sm" title={t("删除")} onclick={() => delMemory(m.id)}>✕</button>
                  </div>
                {/if}
              </div>
            {:else}
              <div class="ga-empty">{t("还没有记忆。聊天中提到的偏好、约定等会被自动记住，也可以手动添加。")}</div>
            {/each}
          </div>
          <div class="ga-tools">
            <input class="ga-input grow" placeholder={t("手动添加一条记忆…")} bind:value={memDraft} onkeydown={(e) => { if (e.key === "Enter" && !e.isComposing) void addMemory(); }} />
            <button class="ga-btn" disabled={!memDraft.trim()} onclick={addMemory}>{t("添加")}</button>
          </div>
          <div class="ga-tools">
            <button class="ga-btn" disabled={!!ga.busy || memBusy} onclick={() => doSummarize("extract")}>{memBusy && ga.busy === "summarize" ? t("处理中…") : t("从最近对话提炼")}</button>
            <button class="ga-btn" disabled={!!ga.busy || memBusy || ga.memories.filter((m) => !m.pinned).length < 2} onclick={() => doSummarize("consolidate")}>{t("整理记忆")}</button>
          </div>
        {:else}
          <div class="ga-body" bind:this={bodyEl}>
            {#if busy && !streamingText}
              <span class="ga-dots" aria-label={t("思考中")}><i></i><i></i><i></i></span>
              {#if ga.busy === "reset"}<span class="ga-sub">{t("正在新建会话…")}</span>{/if}
            {:else if busy}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div class="ga-md" onclick={onMdClick}>{@html md(streamingText)}<span class="ga-caret"></span></div>
            {:else}
              {#if page}
                {#key ga.bubble.index + ga.bubble.rev * 1000}
                  <!-- svelte-ignore a11y_click_events_have_key_events -->
                  <!-- svelte-ignore a11y_no_static_element_interactions -->
                  <div class="ga-md ga-page" onclick={onMdClick}>{@html md(page)}</div>
                {/key}
              {:else if !ga.bubble.error}
                <div class="ga-sub">{t("想聊点什么？")}</div>
              {/if}
              {#if ga.bubble.error}<div class="ga-error">{ga.bubble.error}</div>{/if}
            {/if}
          </div>
          {#if pageCount > 1 && !busy}
            <div class="ga-pager">
              <button class="ga-icon sm" disabled={ga.bubble.index === 0} onclick={prev} aria-label={t("上一页")}>‹</button>
              <span>{ga.bubble.index + 1} / {pageCount}</span>
              <button class="ga-icon sm" disabled={ga.bubble.index >= pageCount - 1} onclick={next} aria-label={t("下一页")}>›</button>
            </div>
          {/if}
        {/if}

        {#if memoShown}<div class="ga-memo">✦ {memoShown}</div>{/if}

        {#if ga.composer}
          <div class="ga-compose">
            {#if mode === "amadeus"}<span class="ga-prompt">&gt;</span>{/if}
            <textarea
              class="ga-input grow"
              rows="1"
              bind:this={inputEl}
              bind:value={input}
              placeholder={busy ? t("回复中…") : t("和{name}说点什么（Enter 发送，Shift+Enter 换行）", { name: p.name })}
              onkeydown={onKey}
            ></textarea>
            {#if busy}
              <button class="ga-btn" onclick={cancel}>{mode === "amadeus" ? "[ STOP ]" : t("停止")}</button>
            {:else}
              <button class="ga-btn primary" disabled={!input.trim()} onclick={submit}>{mode === "amadeus" ? "[ SEND ]" : t("发送")}</button>
            {/if}
          </div>
        {:else}
          <div class="ga-foot">
            {#if busy}
              <button class="ga-btn" onclick={cancel}>{t("停止")}</button>
            {/if}
            <span class="ga-grow"></span>
            <button class="ga-btn" onclick={openComposer}>{t("回复")}</button>
          </div>
        {/if}
      </div>
    {:else if mode !== "plain"}
      <button class="ga-chip" class:busy title={t("和{name}聊天（Alt+A）", { name: p.name })} onclick={onChip}>
        {#if mode === "amadeus"}<span class="ga-led" class:busy></span>AMADEUS{:else}<span class="ga-chip-dot"></span>{p.name}{/if}
      </button>
    {/if}
  </div>
{/if}

<style>
  .ga-root {
    position: fixed;
    left: var(--ga-left);
    bottom: var(--ga-bottom);
    z-index: 320;
    pointer-events: none;
    font-size: 13px;
    max-width: calc(100vw - var(--ga-left) - 12px);
  }
  .ga-root > * { pointer-events: auto; }
  .ga-root.ga-plain { left: 14px; bottom: 14px; }

  /* ---------- 通用卡片 ---------- */
  .ga-card {
    position: relative;
    width: 300px;
    max-width: 100%;
    max-height: min(46vh, 420px, calc(100vh - var(--ga-bottom) - 44px));
    display: flex;
    flex-direction: column;
    background: var(--bg-elev);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 14px;
    box-shadow: 0 10px 28px #0005;
    overflow: hidden;
    animation: ga-pop .16s ease-out;
  }
  .ga-card.wide { width: 360px; max-height: min(64vh, 560px, calc(100vh - var(--ga-bottom) - 44px)); }
  .ga-plain .ga-card { position: absolute; left: 0; bottom: 48px; border-radius: 12px; max-width: calc(100vw - 28px); max-height: min(46vh, 420px, calc(100vh - 110px)); }
  .ga-plain .ga-card.wide { max-height: min(64vh, 560px, calc(100vh - 110px)); }
  .ga-generic .ga-card { border-bottom-left-radius: 4px; }
  @keyframes ga-pop { from { opacity: 0; transform: translateY(6px) scale(.98); } to { opacity: 1; transform: none; } }

  .ga-head { display: flex; align-items: center; gap: 6px; padding: 7px 8px 5px 12px; border-bottom: 1px solid var(--border-soft); flex: none; }
  .ga-title { font-weight: 600; font-size: 12.5px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .ga-grow { flex: 1; min-width: 0; }
  .ga-icon {
    border: 0; background: transparent; color: var(--text-dim); cursor: pointer;
    width: 24px; height: 22px; border-radius: 6px; font-size: 12px; line-height: 1; flex: none;
  }
  .ga-icon:hover:not(:disabled), .ga-icon.on { background: var(--accent-soft); color: var(--text); }
  .ga-icon:disabled { opacity: .35; cursor: default; }
  .ga-icon.sm { width: 22px; height: 20px; font-size: 11px; }

  .ga-body { padding: 10px 12px 6px; overflow: auto; min-height: 34px; flex: 1 1 auto; line-height: 1.6; }
  .ga-md :global(p) { margin: 0 0 .45em; }
  .ga-md :global(p:last-child) { margin-bottom: 0; }
  .ga-md :global(pre) { font-size: 12px; max-height: 220px; overflow: auto; background: var(--code-bg); padding: 6px 8px; border-radius: 6px; margin: 4px 0; }
  .ga-md :global(code) { font-family: var(--mono); font-size: .92em; }
  .ga-md :global(ul), .ga-md :global(ol) { margin: 0 0 .4em; padding-left: 1.3em; }
  .ga-md :global(a) { color: var(--accent); }
  .ga-page { animation: ga-fade .2s ease-out; }
  @keyframes ga-fade { from { opacity: 0; } to { opacity: 1; } }
  .ga-sub { color: var(--text-faint); font-size: 12px; }
  .ga-error { color: var(--danger); font-size: 12px; margin-top: 4px; white-space: pre-wrap; word-break: break-word; }
  .ga-caret { display: inline-block; width: 6px; height: 1em; vertical-align: -2px; margin-left: 2px; background: currentColor; opacity: .6; animation: ga-blink 1s steps(2) infinite; }
  @keyframes ga-blink { 50% { opacity: 0; } }
  .ga-dots { display: inline-flex; gap: 4px; padding: 4px 0; }
  .ga-dots i { width: 6px; height: 6px; border-radius: 50%; background: var(--text-dim); animation: ga-dot 1.1s ease-in-out infinite; }
  .ga-dots i:nth-child(2) { animation-delay: .15s; }
  .ga-dots i:nth-child(3) { animation-delay: .3s; }
  @keyframes ga-dot { 0%, 80%, 100% { opacity: .25; transform: none; } 40% { opacity: 1; transform: translateY(-2px); } }

  .ga-pager { display: flex; align-items: center; justify-content: center; gap: 6px; font-size: 11px; color: var(--text-faint); padding: 0 0 2px; flex: none; }
  .ga-foot, .ga-tools { display: flex; align-items: center; gap: 6px; padding: 4px 8px 7px; flex: none; }
  .ga-tools { border-top: 1px solid var(--border-soft); padding-top: 6px; }
  .ga-tools + .ga-tools { border-top: 0; padding-top: 0; }
  .ga-btn {
    border: 1px solid var(--border); background: var(--bg-elev2); color: var(--text);
    border-radius: 7px; padding: 2px 9px; font-size: 12px; cursor: pointer; white-space: nowrap; height: 24px;
  }
  .ga-btn:hover:not(:disabled) { border-color: var(--accent); }
  .ga-btn:disabled { opacity: .45; cursor: default; }
  .ga-btn.primary { background: var(--accent); color: var(--accent-contrast); border-color: var(--accent); }
  .ga-compose { display: flex; align-items: flex-end; gap: 6px; padding: 6px 8px 8px; border-top: 1px solid var(--border-soft); flex: none; }
  .ga-input {
    border: 1px solid var(--border); background: var(--bg); color: var(--text); border-radius: 8px;
    padding: 4px 8px; font: inherit; font-size: 12.5px; resize: none; min-width: 0; max-height: 96px; field-sizing: content;
  }
  .ga-input:focus { outline: none; border-color: var(--accent); }
  .grow { flex: 1; }
  .ga-memo { font-size: 11px; color: var(--accent); padding: 0 12px 4px; flex: none; }

  .ga-list { overflow: auto; padding: 8px 10px; flex: 1 1 auto; min-height: 120px; display: flex; flex-direction: column; gap: 8px; }
  .ga-msg { max-width: 92%; }
  .ga-msg.user { align-self: flex-end; }
  .ga-msg .ga-md { padding: 5px 9px; border-radius: 10px; background: var(--bg-elev2); line-height: 1.55; word-break: break-word; }
  .ga-msg.user .ga-md { background: var(--accent-soft); }
  .ga-msg-meta { font-size: 10.5px; color: var(--text-faint); margin: 0 2px 2px; }
  .ga-msg.user .ga-msg-meta { text-align: right; }
  .ga-empty, .ga-hint { color: var(--text-faint); font-size: 12px; margin: 0; line-height: 1.5; }
  .ga-mem { border: 1px solid var(--border-soft); border-radius: 8px; padding: 6px 8px; background: var(--bg-elev2); }
  .ga-mem.pinned { border-color: var(--accent); }
  .ga-mem-text { line-height: 1.5; word-break: break-word; }
  .ga-mem-acts { display: flex; align-items: center; gap: 4px; margin-top: 3px; }
  .ga-mem-src { font-size: 10.5px; color: var(--text-faint); }
  .ga-mem textarea { width: 100%; box-sizing: border-box; }

  /* ---------- 简易助手（无皮肤） ---------- */
  .ga-launcher {
    width: 38px; height: 38px; border-radius: 50%; border: 1px solid var(--border);
    background: var(--bg-elev); color: var(--accent); cursor: pointer; display: grid; place-items: center;
    box-shadow: 0 4px 14px #0003; opacity: .82; transition: opacity .15s, transform .15s;
  }
  .ga-launcher:hover, .ga-launcher.on { opacity: 1; transform: translateY(-1px); }
  .ga-launcher.busy { animation: ga-pulse 1.4s ease-in-out infinite; }
  @keyframes ga-pulse { 50% { box-shadow: 0 0 0 5px var(--accent-soft); } }

  /* ---------- 通用气泡的收起态 ---------- */
  .ga-chip {
    display: inline-flex; align-items: center; gap: 6px; height: 24px; padding: 0 10px;
    border-radius: 12px 12px 12px 3px; border: 1px solid var(--border); background: var(--bg-elev);
    color: var(--text-dim); font-size: 12px; cursor: pointer; box-shadow: 0 4px 12px #0003; opacity: .88;
  }
  .ga-chip:hover { opacity: 1; color: var(--text); border-color: var(--accent); }
  .ga-chip-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); }
  .ga-chip.busy .ga-chip-dot { animation: ga-blink .9s steps(2) infinite; }

  /* ---------- Amadeus（牧濑红莉栖·动态） ---------- */
  .ga-amadeus {
    --am: #d99a2b;
    --am-soft: #d99a2b33;
    --am-text: #f3e2c0;
    --am-bg: rgba(14, 10, 6, .86);
    font-family: "Cascadia Mono", Consolas, "Courier New", monospace;
  }
  .ga-amadeus .ga-card {
    background:
      repeating-linear-gradient(0deg, #ffffff06 0 1px, transparent 1px 3px),
      linear-gradient(160deg, #2a1c0c66, transparent 60%),
      var(--am-bg);
    color: var(--am-text);
    border: 1px solid #d99a2b88;
    border-radius: 2px;
    box-shadow: 0 0 0 1px #000a, 0 0 18px #d99a2b2e, 0 12px 30px #000a;
    backdrop-filter: blur(6px);
  }
  .ga-corner { position: absolute; width: 10px; height: 10px; border: 0 solid var(--am); pointer-events: none; z-index: 1; }
  .ga-corner.tl { left: 3px; top: 3px; border-left-width: 2px; border-top-width: 2px; }
  .ga-corner.tr { right: 3px; top: 3px; border-right-width: 2px; border-top-width: 2px; }
  .ga-corner.bl { left: 3px; bottom: 3px; border-left-width: 2px; border-bottom-width: 2px; }
  .ga-corner.br { right: 3px; bottom: 3px; border-right-width: 2px; border-bottom-width: 2px; }
  .ga-amadeus .ga-head { border-bottom: 1px solid #d99a2b55; padding: 8px 20px 6px 12px; background: linear-gradient(90deg, #d99a2b1f, transparent 70%); }
  .ga-amadeus .ga-card { width: 356px; }
  .ga-amadeus .ga-card.wide { width: 390px; }
  .ga-amadeus .ga-title { color: var(--am); letter-spacing: .04em; font-size: 11.5px; }
  .ga-nixie { flex: none; }
  .ga-nixie {
    font-size: 11.5px; letter-spacing: .1em; color: #ffb347;
    text-shadow: 0 0 4px #ff8a00, 0 0 9px #ff6a0088; font-variant-numeric: tabular-nums;
  }
  .ga-led { width: 7px; height: 7px; border-radius: 50%; background: #5a3c12; box-shadow: inset 0 0 2px #000; flex: none; }
  .ga-led.busy, .ga-chip.busy .ga-led { background: #ffb347; box-shadow: 0 0 6px #ff8a00; animation: ga-blink .8s steps(2) infinite; }
  .ga-amadeus .ga-icon { color: #d99a2bcc; border-radius: 0; }
  .ga-amadeus .ga-icon:hover:not(:disabled), .ga-amadeus .ga-icon.on { background: var(--am-soft); color: #ffd28a; }
  .ga-amadeus .ga-body { line-height: 1.65; text-shadow: 0 0 1px #0008; }
  .ga-amadeus .ga-md :global(a) { color: #ffc067; }
  .ga-amadeus .ga-md :global(pre) { background: #0008; border: 1px solid #d99a2b33; border-radius: 0; }
  .ga-amadeus .ga-sub, .ga-amadeus .ga-pager, .ga-amadeus .ga-msg-meta, .ga-amadeus .ga-mem-src, .ga-amadeus .ga-empty, .ga-amadeus .ga-hint { color: #c9a874aa; }
  .ga-amadeus .ga-dots i { background: var(--am); }
  .ga-amadeus .ga-caret { background: var(--am); opacity: .9; }
  .ga-amadeus .ga-btn {
    background: transparent; color: var(--am); border: 1px solid #d99a2b88; border-radius: 0;
    font-family: inherit; letter-spacing: .04em;
  }
  .ga-amadeus .ga-btn:hover:not(:disabled) { background: var(--am-soft); color: #ffd28a; border-color: var(--am); }
  .ga-amadeus .ga-btn.primary { background: #d99a2b26; color: #ffd28a; }
  .ga-amadeus .ga-compose { border-top: 1px dashed #d99a2b55; align-items: center; }
  .ga-prompt { color: var(--am); font-weight: 700; text-shadow: 0 0 5px #ff8a00aa; }
  .ga-amadeus .ga-input { background: #0007; color: var(--am-text); border: 1px solid #d99a2b55; border-radius: 0; font-family: inherit; caret-color: var(--am); }
  .ga-amadeus .ga-input:focus { border-color: var(--am); box-shadow: 0 0 0 1px #d99a2b44; }
  .ga-amadeus .ga-input::placeholder { color: #c9a87466; }
  .ga-amadeus .ga-tools { border-top-color: #d99a2b40; }
  .ga-amadeus .ga-msg .ga-md { background: #ffffff0a; border: 1px solid #d99a2b2a; border-radius: 0; }
  .ga-amadeus .ga-msg.user .ga-md { background: #d99a2b1c; border-color: #d99a2b55; }
  .ga-amadeus .ga-mem { background: #ffffff08; border-color: #d99a2b33; border-radius: 0; }
  .ga-amadeus .ga-mem.pinned { border-color: var(--am); }
  .ga-amadeus .ga-memo { color: #ffc067; }
  .ga-amadeus .ga-error { color: #ff7a6b; }
  .ga-amadeus .ga-chip {
    font-family: inherit; letter-spacing: .14em; font-size: 11px; border-radius: 0;
    background: var(--am-bg); color: var(--am); border: 1px solid #d99a2b88; box-shadow: 0 0 10px #d99a2b33;
  }
  .ga-amadeus .ga-chip:hover { color: #ffd28a; border-color: var(--am); }
  @media (prefers-reduced-motion: reduce) {
    .ga-card, .ga-page { animation: none; }
  }
</style>
