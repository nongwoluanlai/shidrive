<script lang="ts">
  // 待办：轻量的全局待办清单（标题栏「待办」）。回车快速添加，点文字直接改，一键复制；助手的主动关怀也会参考未完成的事项。
  import { tick } from "svelte";
  import { t } from "../i18n";
  import { app, toast, saveTodos, nextId } from "../state.svelte";
  import Icon from "./Icon.svelte";

  let draft = $state("");
  let editingId = $state<string | null>(null);
  let editText = $state("");
  let editEl = $state<HTMLTextAreaElement | null>(null);
  let showDone = $state(true);

  const pending = $derived(app.todos.filter((x) => !x.done));
  const done = $derived(app.todos.filter((x) => x.done));

  async function add() {
    const text = draft.trim();
    if (!text) return;
    // 一次粘贴多行：每行一条
    const lines = text.split(/\r?\n/).map((l) => l.replace(/^\s*(?:[-*•]|\d+[.)、]|\[[ xX]?\])\s*/, "").trim()).filter(Boolean);
    const now = Date.now();
    app.todos.unshift(...lines.map((l, i) => ({ id: nextId(), text: l, done: false, at: now - i })));
    draft = "";
    await saveTodos();
  }

  async function toggle(id: string) {
    const x = app.todos.find((y) => y.id === id);
    if (!x) return;
    x.done = !x.done;
    await saveTodos();
  }

  async function startEdit(id: string) {
    const x = app.todos.find((y) => y.id === id);
    if (!x) return;
    editingId = id;
    editText = x.text;
    await tick();
    editEl?.focus();
    editEl?.select();
  }

  async function commitEdit() {
    const id = editingId;
    if (!id) return;
    editingId = null;
    const text = editText.trim();
    const x = app.todos.find((y) => y.id === id);
    if (!x || text === x.text) return;
    if (!text) { await del(id); return; }
    x.text = text;
    await saveTodos();
  }

  async function del(id: string) {
    const i = app.todos.findIndex((x) => x.id === id);
    if (i >= 0) app.todos.splice(i, 1);
    await saveTodos();
  }

  async function clearDone() {
    app.todos = app.todos.filter((x) => !x.done);
    await saveTodos();
  }

  function copy(text: string) {
    navigator.clipboard.writeText(text).then(() => toast("ok", t("已复制"))).catch((e) => toast("error", String(e)));
  }

  const copyAll = () => copy(pending.map((x) => `- [ ] ${x.text}`).join("\n"));

  function insert(text: string) {
    app.insertPrompt = { text, at: Date.now() };
    app.overlay = null;
    if (app.tab !== "chat") app.tab = "chat";
  }

  function editKey(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void commitEdit(); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); editingId = null; }
  }

  function addKey(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void add(); }
  }

  const when = (ms: number) => {
    const d = new Date(ms);
    const same = new Date().toDateString() === d.toDateString();
    const p = (n: number) => String(n).padStart(2, "0");
    return same ? `${p(d.getHours())}:${p(d.getMinutes())}` : `${d.getMonth() + 1}/${d.getDate()}`;
  };
</script>

{#snippet item(x: (typeof app.todos)[number])}
  <div class="row" class:done={x.done}>
    <button class="tick" class:on={x.done} title={x.done ? t("标记为未完成") : t("标记为已完成")} aria-label={x.done ? t("标记为未完成") : t("标记为已完成")} onclick={() => toggle(x.id)}>
      {#if x.done}<svg viewBox="0 0 24 24" width="11" height="11" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" /></svg>{/if}
    </button>
    {#if editingId === x.id}
      <textarea class="edit" rows="1" bind:this={editEl} bind:value={editText} onkeydown={editKey} onblur={commitEdit}></textarea>
    {:else}
      <button class="text" title={t("点击编辑")} onclick={() => startEdit(x.id)}>{x.text}</button>
    {/if}
    <span class="at">{when(x.at)}</span>
    <div class="acts">
      <button class="btn ghost sm" title={t("复制")} aria-label={t("复制")} onclick={() => copy(x.text)}><Icon name="copy" size={13} /></button>
      {#if !x.done}<button class="btn ghost sm" title={t("插入到聊天输入框")} aria-label={t("插入到聊天输入框")} onclick={() => insert(x.text)}>↵</button>{/if}
      <button class="btn ghost sm danger" title={t("删除")} aria-label={t("删除")} onclick={() => del(x.id)}>✕</button>
    </div>
  </div>
{/snippet}

{#if app.overlay === "todos"}
  <div class="panel" role="dialog" aria-label={t("待办")}>
    <div class="head">
      <Icon name="todos" size={15} /> {t("待办")}
      {#if pending.length}<span class="count">{pending.length}</span>{/if}
      <span class="spacer"></span>
      {#if pending.length}<button class="btn ghost sm" title={t("复制全部未完成（Markdown 清单）")} onclick={copyAll}><Icon name="copy" size={12} /> {t("复制全部")}</button>{/if}
      <button class="btn ghost sm" onclick={() => (app.overlay = null)}>✕</button>
    </div>
    <div class="add">
      <textarea rows="1" placeholder={t("记一件要做的事，回车添加（粘贴多行 = 多条）")} bind:value={draft} onkeydown={addKey}></textarea>
      <button class="btn sm primary" disabled={!draft.trim()} onclick={add}><Icon name="plus" size={12} /></button>
    </div>
    <div class="list">
      {#each pending as x (x.id)}{@render item(x)}{/each}
      {#if !app.todos.length}
        <p class="none">{t("还没有待办。随手记下要做的事，做完勾掉就好")}</p>
      {:else if !pending.length}
        <p class="none">{t("都做完了 🎉")}</p>
      {/if}
      {#if done.length}
        <div class="done-head">
          <button class="fold" onclick={() => (showDone = !showDone)}>{showDone ? "▾" : "▸"} {t("已完成")} {done.length}</button>
          <span class="spacer"></span>
          <button class="btn ghost sm" onclick={clearDone}>{t("清除已完成")}</button>
        </div>
        {#if showDone}{#each done as x (x.id)}{@render item(x)}{/each}{/if}
      {/if}
    </div>
  </div>
{/if}

<style>
  .panel {
    position: fixed;
    top: 50px;
    right: 12px;
    width: 420px;
    max-width: calc(100vw - 24px);
    max-height: 70vh;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    z-index: 90;
    display: flex;
    flex-direction: column;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    font-weight: 600;
    border-bottom: 1px solid var(--border-soft);
  }
  .count {
    font-size: 0.75em;
    font-weight: 600;
    padding: 0 6px;
    line-height: 16px;
    border-radius: 8px;
    color: var(--accent-contrast, #fff);
    background: var(--accent);
  }
  .spacer { flex: 1; }
  .add {
    display: flex;
    gap: 6px;
    align-items: flex-end;
    padding: 10px 12px 6px;
  }
  .add textarea, .edit {
    flex: 1;
    min-width: 0;
    resize: none;
    font: inherit;
    font-size: 0.9em;
    field-sizing: content;
    max-height: 120px;
  }
  .list {
    overflow: auto;
    padding: 4px 12px 12px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 5px 6px;
    border-radius: 6px;
  }
  .row:hover { background: var(--bg-hover, var(--bg-elev2)); }
  .tick {
    flex: none;
    width: 16px;
    height: 16px;
    margin-top: 2px;
    padding: 0;
    border: 1.5px solid var(--text-faint);
    border-radius: 4px;
    background: transparent;
    color: var(--accent-contrast, #fff);
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  .tick:hover { border-color: var(--accent); }
  .tick.on { background: var(--accent); border-color: var(--accent); }
  .text {
    flex: 1;
    min-width: 0;
    text-align: left;
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    font-size: 0.9em;
    line-height: 1.45;
    color: var(--text);
    white-space: pre-wrap;
    word-break: break-word;
    cursor: text;
  }
  .row.done .text { color: var(--text-faint); text-decoration: line-through; }
  .at { flex: none; font-size: 0.75em; color: var(--text-faint); margin-top: 3px; }
  .acts {
    position: absolute;
    right: 4px;
    top: 3px;
    display: none;
    gap: 2px;
    padding-left: 6px;
    background: var(--bg-panel);
    border-radius: 6px;
  }
  .row:hover .acts, .row:focus-within .acts { display: flex; }
  .acts .btn { padding: 0 6px; }
  .done-head {
    display: flex;
    align-items: center;
    margin-top: 8px;
    padding-top: 6px;
    border-top: 1px solid var(--border-soft);
  }
  .fold {
    background: none;
    border: 0;
    padding: 2px 4px;
    color: var(--text-dim);
    font: inherit;
    font-size: 0.82em;
    cursor: pointer;
  }
  .none {
    color: var(--text-faint);
    text-align: center;
    padding: 16px;
    font-size: 0.88em;
  }
</style>
