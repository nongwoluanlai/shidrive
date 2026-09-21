<script lang="ts">
  import { t } from "../i18n";
  // Collapsible file preview/editor pane. Save is debounced.
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
</script>

<div class="editor">
  <div class="pbar">
    <Icon name="file" size={13} />
    <span class="ppath" title={ed.path}>{relPath(ed.path)}</span>
    {#if ed.dirty}<span class="badge warn">{t("未保存")}</span>{/if}
    <span class="spacer"></span>
    <button class="btn ghost sm" title={t("在资源管理器中显示")} onclick={() => api.fsOpenExplorer(ed.path).catch((e) => toast("error", String(e)))}><Icon name="folder" size={13} /></button>
    <button class="btn ghost sm" title={t("收起（文件树中可再次打开）")} onclick={collapse}><Icon name="close" size={13} /></button>
  </div>
  {#if ed.binary}
    <div class="empty">{t("无法预览二进制文件")}</div>
  {:else}
    <textarea class="editor-area" bind:value={ed.content} oninput={queueSave} spellcheck="false"></textarea>
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
  .editor-area {
    flex: 1;
    border: none;
    border-radius: 0;
    background: var(--bg);
    font-family: var(--mono);
    font-size: 0.86em;
    line-height: 1.6;
    padding: 12px 16px;
    resize: none;
    user-select: text;
  }
</style>
