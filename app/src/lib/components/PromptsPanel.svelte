<script lang="ts">
  import { t } from "../i18n";
  // 常用提示词: CRUD + insert into chat composer.
  import { app, toast, savePrompts, nextId } from "../state.svelte";
  import Icon from "./Icon.svelte";

  let editingId = $state<string | null>(null);
  let editTitle = $state("");
  let editContent = $state("");

  function add() {
    editingId = "new";
    editTitle = "";
    editContent = "";
  }

  function startEdit(id: string) {
    const p = app.prompts.find((x) => x.id === id);
    if (!p) return;
    editingId = id;
    editTitle = p.title;
    editContent = p.content;
  }

  async function save() {
    if (!editTitle.trim() || !editContent.trim()) return;
    if (editingId === "new") {
      app.prompts.push({ id: nextId(), title: editTitle.trim(), content: editContent.trim() });
    } else if (editingId) {
      const p = app.prompts.find((x) => x.id === editingId);
      if (p) {
        p.title = editTitle.trim();
        p.content = editContent.trim();
      }
    }
    editingId = null;
    await savePrompts();
  }

  async function del(id: string) {
    const i = app.prompts.findIndex((x) => x.id === id);
    if (i >= 0) app.prompts.splice(i, 1);
    await savePrompts();
  }

  function insert(content: string) {
    app.insertPrompt = { text: content, at: Date.now() };
    app.overlay = null;
    if (app.tab !== "chat") app.tab = "chat";
  }

  function copy(content: string) {
    navigator.clipboard.writeText(content).then(() => toast("ok", t("已复制"))).catch((e) => toast("error", String(e)));
  }
</script>

{#if app.overlay === "prompts"}
  <div class="panel" role="dialog" aria-label={t("常用提示词")}>
    <div class="head">
      <Icon name="prompts" size={15} /> {t("常用提示词")}
      <span class="spacer"></span>
      <button class="btn sm" onclick={add}><Icon name="plus" size={12} /> {t("新增")}</button>
      <button class="btn ghost sm" onclick={() => (app.overlay = null)}>✕</button>
    </div>
    <div class="list">
      {#if editingId}
        <div class="edit">
          <input placeholder={t("标题")} bind:value={editTitle} />
          <textarea rows="4" placeholder={t("提示词内容")} bind:value={editContent}></textarea>
          <div class="rowbtns">
            <button class="btn sm primary" onclick={save}>{t("保存")}</button>
            <button class="btn sm" onclick={() => (editingId = null)}>{t("取消")}</button>
          </div>
        </div>
      {/if}
      {#each app.prompts as p (p.id)}
        <div class="row">
          <div class="info">
            <b>{p.title}</b>
            <div class="content dim">{p.content}</div>
          </div>
          <div class="acts">
            <button class="btn sm primary" onclick={() => insert(p.content)}>{t("插入")}</button>
            <button class="btn ghost sm" title={t("复制")} onclick={() => copy(p.content)}><Icon name="copy" size={13} /></button>
            <button class="btn ghost sm" onclick={() => startEdit(p.id)}>{t("编辑")}</button>
            <button class="btn ghost sm danger" onclick={() => del(p.id)}>✕</button>
          </div>
        </div>
      {:else}
        {#if !editingId}
          <p class="none">{t("还没有常用提示词，点「新增」创建")}</p>
        {/if}
      {/each}
    </div>
    <p class="tip">{t("「插入」会填到当前上下文聊天的输入框。")}</p>
  </div>
{/if}

<style>
  .panel {
    position: fixed;
    top: 50px;
    right: 12px;
    width: 520px;
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
  .spacer {
    flex: 1;
  }
  .list {
    overflow: auto;
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--border-soft);
    border-radius: 8px;
  }
  .info {
    flex: 1;
    min-width: 0;
    font-size: 0.9em;
  }
  .content {
    font-size: 0.85em;
    margin-top: 2px;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .dim {
    color: var(--text-faint);
  }
  .acts {
    display: flex;
    gap: 4px;
    flex: none;
    align-items: center;
  }
  .edit {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .rowbtns {
    display: flex;
    gap: 6px;
  }
  .none {
    color: var(--text-faint);
    text-align: center;
    padding: 18px;
    font-size: 0.9em;
  }
  .tip {
    padding: 8px 14px 12px;
    color: var(--text-faint);
    font-size: 0.8em;
    border-top: 1px solid var(--border-soft);
  }
</style>
