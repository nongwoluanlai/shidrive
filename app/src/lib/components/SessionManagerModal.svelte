<script lang="ts">
  import { t } from "../i18n";
  // 会话管理: 表格形式展示所有绑定，双击跳转；支持解绑、复制提示词、改标题。
  import { app, toast, sharedContextPrompt } from "../state.svelte";
  import { api } from "../ipc";
  import Icon from "./Icon.svelte";
  import type { AgentType, BindingInfo } from "../types";
  import { confirmDialog, promptDialog } from "../dialog.svelte";

  let bindings = $state<BindingInfo[]>([]);
  let loading = $state(true);
  let editingTitle = $state<string | null>(null);

  $effect(() => {
    if (app.overlay !== "sessions") return;
    loading = true;
    api
      .bindingsAll()
      .then((b) => (bindings = b))
      .catch((e) => toast("error", String(e)))
      .finally(() => (loading = false));
  });

  async function unbind(b: BindingInfo) {
    if (!(await confirmDialog({ title: t("解绑会话"), message: t("解绑 {p0} × {p1} 的会话？AI 端会话不会删除。", { p0: b.context_name, p1: b.agent_type }), danger: true, confirmText: t("解绑") }))) return;
    try {
      await api.bindingUnbind(b.context_id, b.agent_type);
      bindings = await api.bindingsAll();
      toast("ok", t("已解绑"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function jump(b: BindingInfo) {
    const p = app.projects.find((x) => x.name === b.project_name);
    if (p && app.projectId !== p.id) {
      const { selectProject } = await import("../state.svelte");
      await selectProject(p.id);
    }
    const c = app.contexts.find((x) => x.id === b.context_id);
    if (c) {
      app.agent = b.agent_type as AgentType;
      const { selectContext } = await import("../state.svelte");
      await selectContext(c.id);
      app.tab = "chat";
      app.overlay = null;
    }
  }

  async function editTitle(b: BindingInfo) {
    const newTitle = await promptDialog({ title: t("会话标题"), label: t("标题（本地备注）"), initial: b.title ?? "" });
    if (newTitle === null || !newTitle.trim()) return;
    try {
      await api.bindingSetTitle(b.context_id, b.agent_type, newTitle.trim());
      b.title = newTitle.trim();
      toast("ok", t("标题已更新"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  const agentName = (id: string) => app.agents.find((x) => x.id === id)?.name ?? id;

  const displayTitle = (b: BindingInfo) => b.title || (b.session_id ? b.session_id.slice(0, 8) + "…" : "—");
</script>

{#if app.overlay === "sessions"}
  <div class="panel" role="dialog" aria-label={t("会话管理")}>
    <div class="head">
      <Icon name="sessions" size={15} /> {t("会话管理")}
      <span class="spacer"></span>
      <button class="btn ghost sm" onclick={() => (app.overlay = null)}>✕</button>
    </div>
    <div class="table-wrap">
      {#if loading}
        <p class="none">{t("加载中…")}</p>
      {:else}
        <table>
          <thead>
            <tr>
              <th>{t("项目")}</th>
              <th>{t("状态")}</th>
              <th>{t("会话描述")}</th>
              <th>Agent</th>
              <th>{t("工作目录")}</th>
              <th>session</th>
              <th>{t("更新时间")}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each bindings as b (b.id)}
              <tr ondblclick={() => jump(b)} title={t("双击前往该会话")}>
                <td>{b.project_name}</td>
                <td>
                  <span class="badge {b.status === 'running' ? 'accent' : b.status === 'completed' ? 'ok' : b.status === 'interrupted' ? 'warn' : ''}">
                    {{ running: t("对话中"), completed: t("已完成"), interrupted: t("已中断") }[b.status ?? ""] ?? t("未创建")}
                  </span>
                </td>
                <td class="tdesc">
                  <span class="ttitle" title={t("点击修改标题（本地备注）")} onclick={(e) => { e.stopPropagation(); editTitle(b); }}>{displayTitle(b)}</span>
                </td>
                <td>{agentName(b.agent_type)}</td>
                <td class="twd" title={b.workspace ?? ""}>{b.workspace || "—"}</td>
                <td class="tsid" title={b.session_id ?? ""}>{b.session_id ? b.session_id.slice(0, 8) + "…" : "—"}</td>
                <td>{b.updated_at.slice(5, 16)}</td>
                <td class="acts">
                  <button class="btn sm" onclick={(e) => { e.stopPropagation(); void jump(b); }}>{t("前往")}</button>
                  <button class="btn ghost sm danger" onclick={(e) => { e.stopPropagation(); void unbind(b); }}>{t("解绑")}</button>
                </td>
              </tr>
            {:else}
              <tr><td colspan="8" class="none">{t("还没有任何会话绑定")}</td></tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
    <p class="tip">{t("双击行前往会话 · 点击会话描述可修改标题（本地备注）· 绑定历史会话请进入对应上下文聊天页的「绑定」。")}</p>
  </div>
{/if}

<style>
  .panel {
    position: fixed;
    top: 50px;
    right: 12px;
    width: min(1120px, calc(100vw - 292px));
    max-height: 78vh;
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
  .table-wrap {
    overflow: auto;
    padding: 8px 12px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85em;
  }
  th,
  td {
    text-align: left;
    padding: 7px 9px;
    border-bottom: 1px solid var(--border-soft);
    white-space: nowrap;
  }
  th {
    color: var(--text-faint);
    font-weight: 600;
    font-size: 0.92em;
  }
  tbody tr {
    cursor: pointer;
  }
  tbody tr:hover {
    background: var(--bg-elev);
  }
  .tdesc {
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .ttitle {
    cursor: text;
    user-select: text;
  }
  .twd {
    max-width: 170px;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-faint);
  }
  .tsid {
    font-family: var(--mono);
    font-size: 0.9em;
    color: var(--text-dim);
  }
  .acts {
    display: flex;
    gap: 4px;
    flex: none;
  }
  .none {
    color: var(--text-faint);
    text-align: center;
    padding: 18px;
  }
  .tip {
    padding: 8px 14px 12px;
    color: var(--text-faint);
    font-size: 0.8em;
    border-top: 1px solid var(--border-soft);
  }
</style>
