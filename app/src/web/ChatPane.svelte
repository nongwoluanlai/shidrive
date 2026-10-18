<script lang="ts">
  import { tick } from "svelte";
  import { ui, currentChat, currentContext, chatKey, selectAgent, sendMessage, cancelTurn, newSession, syncHistory, openChat, setView, loadEarlier } from "./store.svelte";
  import MessageView from "./MessageView.svelte";
  import Icon from "./Icon.svelte";

  let text = $state("");
  let listEl: HTMLDivElement | undefined = $state();
  let innerEl: HTMLDivElement | undefined = $state();
  let taEl: HTMLTextAreaElement | undefined = $state();
  let stick = true;
  let sending = $state(false);

  const chat = $derived(currentChat());
  const ctx = $derived(currentContext());
  const key = $derived(chatKey(ui.contextId, ui.agent));
  const agentName = $derived(ui.agents.find((a) => a.id === ui.agent)?.name ?? ui.agent);
  const DRAFT = "shidrive.web.draft.";

  // 长会话只渲染最近一段（DOM 越少，打字/滚动/流式输出越流畅），上滑到顶自动展开更早的
  const PAGE = 40;
  let shown = $state(PAGE);
  const items = $derived(chat?.items ?? []);
  const hiddenLocal = $derived(Math.max(0, items.length - shown));
  const visible = $derived(hiddenLocal ? items.slice(hiddenLocal) : items);
  let expanding = false;

  async function showEarlier() {
    if (expanding || !listEl) return;
    const omitted = chat?.omitted ?? 0;
    if (!hiddenLocal && !omitted) return;
    expanding = true;
    const before = listEl.scrollHeight - listEl.scrollTop;
    try {
      if (hiddenLocal) shown += PAGE;
      else {
        const prevLen = items.length;
        if (await loadEarlier(key)) shown = Math.max(PAGE, items.length - prevLen + shown);
      }
      await tick();
      // 保持当前阅读位置不跳动
      if (listEl) listEl.scrollTop = listEl.scrollHeight - before;
    } finally {
      expanding = false;
    }
  }

  // 草稿按会话保存，切换上下文 / Agent 不丢
  let draftKey = "";
  $effect(() => {
    const k = key;
    if (k === draftKey) return;
    draftKey = k;
    try { text = localStorage.getItem(DRAFT + k) ?? ""; } catch { text = ""; }
    shown = PAGE;
    stick = true;
    void tick().then(() => { autosize(); scrollBottom(); });
  });
  function saveDraftNow() {
    if (draftTimer) { clearTimeout(draftTimer); draftTimer = null; }
    try { if (text) localStorage.setItem(DRAFT + key, text); else localStorage.removeItem(DRAFT + key); } catch { /* ignore */ }
  }
  // 打字时节流写 localStorage（同步 IO，长草稿每键都写会卡）
  let draftTimer: ReturnType<typeof setTimeout> | null = null;
  function saveDraft() {
    if (draftTimer) clearTimeout(draftTimer);
    draftTimer = setTimeout(saveDraftNow, 400);
  }

  function scrollBottom() { if (listEl) listEl.scrollTop = listEl.scrollHeight; }
  function onScroll() {
    if (!listEl) return;
    stick = listEl.scrollHeight - listEl.scrollTop - listEl.clientHeight < 80;
    if (listEl.scrollTop < 120 && hiddenLocal) void showEarlier();
  }
  // 内容高度变化（流式输出 / Markdown 渲染）时，贴底则保持贴底
  $effect(() => {
    if (!innerEl) return;
    const ro = new ResizeObserver(() => { if (stick) scrollBottom(); });
    ro.observe(innerEl);
    return () => ro.disconnect();
  });

  // 浏览器原生支持按内容自适应高度时（field-sizing）不再用 JS 测量；否则合并到下一帧测量一次，
  // 避免每个按键都强制整页重排
  const nativeSize = typeof CSS !== "undefined" && CSS.supports?.("field-sizing", "content");
  let sizeRaf = 0;
  function autosize() {
    if (nativeSize || !taEl || sizeRaf) return;
    sizeRaf = requestAnimationFrame(() => {
      sizeRaf = 0;
      if (!taEl) return;
      const max = Math.round(window.innerHeight * 0.4);
      taEl.style.height = "auto";
      taEl.style.height = Math.min(taEl.scrollHeight, max) + "px";
    });
  }

  const coarse = typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;
  function onKey(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.isComposing) return;
    // 桌面浏览器：Enter 发送、Shift+Enter 换行；触屏：Enter 换行，点按钮发送；Ctrl/⌘+Enter 总是发送
    if (e.ctrlKey || e.metaKey || (!coarse && !e.shiftKey)) {
      e.preventDefault();
      void send();
    }
  }

  async function send() {
    const t = text.trim();
    if (!t || sending || chat?.running) return;
    sending = true;
    stick = true;
    text = "";
    saveDraftNow();
    await tick();
    autosize();
    const ok = await sendMessage(t);
    if (!ok) { text = t; saveDraftNow(); await tick(); autosize(); }
    sending = false;
  }

  function onListClick(e: MouseEvent) {
    const el = (e.target as HTMLElement).closest(".md-file") as HTMLElement | null;
    if (!el) return;
    e.preventDefault();
    const p = el.dataset.path;
    if (p) { ui.openFile = p; setView("files"); }
  }

  function confirmNew() {
    if (confirm(`为「${ctx?.name ?? ""}」创建新的 ${agentName} 会话？\n当前会话不会删除，可在电脑端「会话管理」中重新绑定。`)) void newSession();
  }
</script>

<div class="chat">
  <div class="head">
    <div class="agents scroll">
      {#each ui.agents as a (a.id)}
        <button class="agent" class:on={a.id === ui.agent} onclick={() => selectAgent(a.id)} title={a.status}>
          <span class="st {a.status}"></span>{a.name}
        </button>
      {:else}
        <span class="faint">电脑端未启用任何 Agent</span>
      {/each}
    </div>
    <div class="tools">
      {#if chat?.sessionId}
        <button class="icon-btn" title="从 Agent 会话同步历史" disabled={chat.syncing || chat.running} onclick={() => void syncHistory(key)}>
          {#if chat.syncing}<span class="spin"></span>{:else}<Icon name="refresh" />{/if}
        </button>
      {/if}
      <button class="icon-btn" title="新会话" disabled={!ctx || chat?.running} onclick={confirmNew}><Icon name="plus" /></button>
    </div>
  </div>

  {#if chat?.title || chat?.fromSnapshot}
    <div class="sub">
      {#if chat?.title}<span class="title">{chat.title}</span>{/if}
      {#if chat?.fromSnapshot && !chat.syncing}<button class="link" onclick={() => void syncHistory(key)}>显示的是电脑端缓存，点此同步最新历史</button>{/if}
      {#if chat?.syncing}<span class="faint">正在同步历史…</span>{/if}
    </div>
  {/if}

  <div class="list scroll" bind:this={listEl} onscroll={onScroll} onclick={onListClick} role="presentation">
    <div class="inner" bind:this={innerEl}>
      {#if !ctx}
        <div class="empty"><Icon name="book" size={30} /><div>请选择一个上下文</div></div>
      {:else if !ui.agent}
        <div class="empty"><Icon name="chat" size={30} /><div>电脑端未启用 Agent</div></div>
      {:else if chat?.loading && !chat.items.length}
        <div class="empty"><span class="spin"></span><div>加载会话…</div></div>
      {:else if chat?.error}
        <div class="empty"><Icon name="alert" size={28} /><div>{chat.error}</div><button class="btn sm" onclick={() => void openChat(true)}>重试</button></div>
      {:else if !chat?.items.length && !chat?.syncing}
        <div class="empty">
          <Icon name="chat" size={30} />
          <div>{chat?.sessionId ? "会话暂无内容" : "尚未创建会话，发送第一条消息即可开始"}</div>
          <div class="faint">{agentName} · {ctx.name}</div>
        </div>
      {:else}
        {#if hiddenLocal || chat?.omitted}
          <div class="earlier">
            <button class="btn sm" disabled={chat?.loading || chat?.syncing} onclick={() => void showEarlier()}>
              {#if chat?.loading || chat?.syncing}<span class="spin"></span>{/if}
              显示更早的消息（{hiddenLocal + (chat?.omitted ?? 0)} 条）
            </button>
          </div>
        {/if}
        {#each visible as it (it.id)}<MessageView item={it} />{/each}
        {#if chat?.running && !chat.items[chat.items.length - 1]?.streaming}
          <div class="working"><span class="spin"></span><span>{agentName} 正在处理…</span></div>
        {/if}
      {/if}
    </div>
  </div>

  <div class="composer">
    <div class="box" class:disabled={!ctx || !ui.agent}>
      <textarea
        bind:this={taEl}
        bind:value={text}
        rows="1"
        placeholder={ctx ? `给 ${agentName} 发消息${coarse ? "" : "（Enter 发送，Shift+Enter 换行）"}` : "请先选择上下文"}
        disabled={!ctx || !ui.agent}
        oninput={() => { autosize(); saveDraft(); }}
        onblur={saveDraftNow}
        onkeydown={onKey}
      ></textarea>
      {#if chat?.running}
        <button class="send stop" onclick={() => void cancelTurn()} title="停止"><Icon name="stop" /></button>
      {:else}
        <button class="send" disabled={!text.trim() || sending || !ctx || !ui.agent} onclick={() => void send()} title="发送">
          {#if sending}<span class="spin"></span>{:else}<Icon name="send" />{/if}
        </button>
      {/if}
    </div>
  </div>
</div>

<style>
  .chat { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .head { display: flex; align-items: center; gap: 6px; padding: 8px 10px; border-bottom: 1px solid var(--border-soft); background: var(--bg-panel); flex: none; }
  .agents { display: flex; gap: 6px; flex: 1; min-width: 0; overflow-x: auto; scrollbar-width: none; }
  .agents::-webkit-scrollbar { display: none; }
  .agent { display: inline-flex; align-items: center; gap: 6px; padding: 5px 12px; border-radius: 999px; border: 1px solid var(--border); color: var(--text-dim); font-size: 13.5px; white-space: nowrap; flex: none; }
  .agent.on { background: var(--accent-soft); color: var(--accent); border-color: transparent; font-weight: 600; }
  .st { width: 7px; height: 7px; border-radius: 50%; background: var(--text-faint); }
  .st.connected, .st.ready { background: var(--ok); }
  .st.connecting { background: var(--warn); }
  .st.error { background: var(--danger); }
  .tools { display: flex; gap: 2px; }
  .sub { display: flex; align-items: center; gap: 10px; padding: 6px 14px; font-size: 12.5px; color: var(--text-dim); border-bottom: 1px solid var(--border-soft); flex-wrap: wrap; flex: none; }
  .title { font-weight: 600; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 60%; }
  .link { color: var(--accent); font-size: 12.5px; }
  .list { flex: 1; min-height: 0; }
  .inner { max-width: 860px; margin: 0 auto; padding: 12px 14px 20px; }
  .working { display: flex; align-items: center; gap: 8px; color: var(--text-dim); font-size: 13.5px; padding: 8px 2px; }
  .composer { flex: none; padding: 8px 10px 10px; background: linear-gradient(transparent, var(--bg) 30%); }
  .box {
    max-width: 860px; margin: 0 auto; display: flex; align-items: flex-end; gap: 8px; background: var(--bg-panel);
    border: 1px solid var(--border); border-radius: 18px; padding: 6px 6px 6px 14px; box-shadow: var(--shadow);
  }
  .box:focus-within { border-color: var(--accent); }
  .box.disabled { opacity: .6; }
  .earlier { display: flex; justify-content: center; padding: 4px 0 10px; }
  textarea { field-sizing: content; flex: 1; border: none; outline: none; background: transparent; resize: none; max-height: 40vh; padding: 7px 0; line-height: 1.5; font-size: 15px; min-height: 36px; }
  .send { width: 38px; height: 38px; border-radius: 50%; background: var(--accent); color: var(--accent-contrast); display: grid; place-items: center; flex: none; }
  .send:disabled { opacity: .4; cursor: default; }
  .send.stop { background: var(--danger); color: #fff; }
  @media (max-width: 899px) { textarea { font-size: 16px; } }
</style>
