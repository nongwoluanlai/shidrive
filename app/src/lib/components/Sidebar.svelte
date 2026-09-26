<script lang="ts">
  import { app, selectProject, selectContext, refreshContexts, refreshWorkflows, toast, isNoProject } from "../state.svelte";
import { confirmDialog, promptDialog } from "../dialog.svelte";
  import { api } from "../ipc";
  import ContextMenu from "./ContextMenu.svelte";
  import Icon from "./Icon.svelte";
  import type { MenuItem } from "./menu-item";
  import type { Edge, Workflow, WorkflowStep } from "../types";
  import { scheduleText } from "./wf-shared";
  import { t } from "../i18n";

  let showNewContext = $state(false);
  let projectName = $state("");
  let projectPath = $state("");
  let projectDesc = $state("");
  let contextName = $state("");
  let showEditProject = $state(false);
  let editProjectId = $state("");
  let editName = $state("");
  let editPath = $state("");
  let editDesc = $state("");

  function openEditProject() {
    const p = app.projects.find((x) => x.id === app.projectId);
    if (!p) return;
    editProjectId = p.id;
    editName = p.name;
    editPath = p.root_path;
    editDesc = p.description;
    showEditProject = true;
  }

  async function saveProject() {
    if (!editName.trim()) {
      toast("warn", t("项目名称不能为空"));
      return;
    }
    try {
      await api.projectsUpdate(editProjectId, editName, editPath, editDesc);
      app.projects = await api.projectsList();
      showEditProject = false;
      toast("ok", t("项目已更新"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function deleteProject() {
    if (!(await confirmDialog({ title: t("删除项目"), message: t("删除该项目？其下上下文、会话绑定、工作流（含运行记录）与远程授权将一并删除，正在运行的工作流会被停止（项目目录文件不受影响）。"), danger: true, confirmText: t("删除") }))) return;
    try {
      const id = editProjectId;
      const cleanup = await api.projectsDelete(id);
      showEditProject = false;
      if (app.projectId === id) await selectProject(null);
      app.projects = await api.projectsList();
      toast("ok", t("项目已删除（清理 {p0} 个上下文、{p1} 个工作流、{p2} 项远程授权）", { p0: cleanup.contexts, p1: cleanup.workflows, p2: cleanup.grants }));
    } catch (e) {
      toast("error", String(e));
    }
  }

  let showNewProject = $state(false);

  function openNewProject() {
    projectName = "";
    projectDesc = "";
    projectPath = "";
    showNewProject = true;
  }

  async function createProject() {
    if (!projectName.trim()) {
      toast("warn", t("项目名称不能为空"));
      return;
    }
    try {
      const p = await api.projectsCreate(projectName, projectPath, projectDesc);
      showNewProject = false;
      projectName = projectPath = projectDesc = "";
      app.projects = await api.projectsList();
      await selectProject(p.id);
      toast("ok", t("项目已创建"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);


  async function createContext() {
    if (!contextName.trim() || !app.projectId) return;
    try {
      const c = await api.contextsCreate(app.projectId, contextName);
      showNewContext = false;
      contextName = "";
      await refreshContexts();
      if (app.projectId === c.project_id) await selectContext(c.id);
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function deleteContext(id: string) {
    if (!(await confirmDialog({ title: t("删除上下文"), message: t("删除该上下文？会话绑定与本地聊天记录将一并删除（AI 端会话不受影响）。"), danger: true, confirmText: t("删除") }))) return;
    try {
      await api.contextsDelete(id);
      if (app.contextId === id) app.contextId = null;
      await refreshContexts();
      toast("ok", t("上下文已删除"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function renameContext(id: string) {
    const c = app.contexts.find((x) => x.id === id);
    if (!c) return;
    const name = await promptDialog({ title: t("编辑上下文"), label: t("名称"), initial: c.name });
    if (name === null || !name.trim() || name.trim() === c.name) return;
    try {
      await api.contextsUpdate(id, name.trim(), c.overview, c.constraints);
      await refreshContexts();
      toast("ok", t("上下文已更新"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  function contextMenu(e: MouseEvent, id: string) {
    e.preventDefault();
    // 阻止冒泡到 window：ContextMenu 挂载时会注册一次性的 contextmenu
    // 关闭监听，不拦截的话菜单会被当前这个右键事件立刻关掉
    e.stopPropagation();
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: t("✏️ 编辑（重命名）"), run: () => renameContext(id) },
        "sep",
        { label: t("🗑 删除上下文"), danger: true, run: () => deleteContext(id) },
      ],
    };
  }

  // Clipboard for "copy / paste as duplicate". Only the workflow body is copied:
  // the pasted copy is always a manual, non-scheduled workflow so a paste can
  // never silently arm a second schedule.
  let wfClipboard: { name: string; steps: WorkflowStep[]; edges: Edge[]; env: Record<string, string>; description: string } | null = null;

  function copyWorkflow(w: Workflow) {
    wfClipboard = {
      name: w.name,
      description: w.description,
      steps: structuredClone($state.snapshot(w.steps)) as WorkflowStep[],
      edges: structuredClone($state.snapshot(w.edges ?? [])) as Edge[],
      env: structuredClone($state.snapshot(w.env ?? {})) as Record<string, string>,
    };
    toast("ok", t("工作流已复制"));
  }

  /** Paste the clipboard as a new workflow of the *current* project (not of the right-clicked row). */
  async function pasteWorkflow() {
    const projectId = app.projectId;
    if (!projectId) return;
    if (!wfClipboard) {
      toast("info", t("剪贴板中没有工作流，请先复制"));
      return;
    }
    const src = wfClipboard;
    try {
      const copy = await api.workflowCreate({
        project_id: projectId,
        name: src.name + t(" 副本"),
        description: src.description,
        enabled: true,
        trigger_type: "manual",
        schedule: null,
        steps: structuredClone(src.steps),
        env: structuredClone(src.env),
        edges: structuredClone(src.edges),
      });
      await refreshWorkflows();
      if (app.projectId !== projectId) return;
      app.tab = "workflows";
      app.workflowSelected = copy.id;
      toast("ok", t("已创建副本"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function newWorkflow() {
    if (!app.projectId) return;
    const name = await promptDialog({ title: t("新建工作流"), label: t("工作流名称 *") });
    if (name === null || !name.trim()) return;
    try {
      const w = await api.workflowCreate({
        project_id: app.projectId,
        name: name.trim(),
        description: "",
        enabled: true,
        trigger_type: "manual",
        schedule: null,
        steps: [{ type: "start", name: "", x: 60, y: 40 }],
      });
      await refreshWorkflows();
      app.tab = "workflows";
      app.workflowSelected = w.id;
      toast("ok", t("工作流已创建"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  // 空白区右键：上下文区（新建/刷新）
  function ctxBlankMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (!app.projectId) return;
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: t("＋ 新建上下文"), run: () => (showNewContext = true) },
        { label: t("⟳ 刷新列表"), run: () => void refreshContexts() },
      ],
    };
  }

  // 空白区右键：工作流区（新建/粘贴/刷新）
  function wfBlankMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (!app.projectId) return;
    const items = [
      { label: t("＋ 新建工作流"), run: () => void newWorkflow() },
      { label: t("⟳ 刷新列表"), run: () => void refreshWorkflows() },
    ];
    if (wfClipboard) items.push({ label: t("📋 粘贴为副本"), run: () => void pasteWorkflow() });
    menu = { x: e.clientX, y: e.clientY, items };
  }

  function wfMenu(e: MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    const w = app.workflows.find((x) => x.id === id);
    if (!w) return;
    const items: MenuItem[] = [
      { label: t("▶ 运行"), run: () => {
          // 运行不切页：当前在会话页就留在会话页，仅提示结果
          if (app.tab === "workflows") app.workflowSelected = id;
          api.workflowRun(id).then(() => toast("ok", t("已启动"))).catch((err) => toast("error", String(err)));
      } },
      { label: t("↻ 重新执行"), run: () => {
          void (async () => {
            const act = await api.activeRuns();
            const r = act.find((x) => x[1] === id);
            if (r) {
              await api.workflowStop(r[0]);
              await new Promise((res) => setTimeout(res, 800));
            }
            if (app.tab === "workflows") app.workflowSelected = id;
            try {
              await api.workflowRun(id);
              toast("ok", t("已重新执行"));
            } catch (err) {
              toast("error", String(err));
            }
          })();
      } },
      { label: t("■ 停止运行"), run: () => {
          void (async () => {
            const act = await api.activeRuns();
            const r = act.find((x) => x[1] === id);
            if (r) {
              await api.workflowStop(r[0]);
              toast("ok", t("已发送停止请求"));
            } else {
              toast("info", t("该工作流未在运行"));
            }
          })();
      } },
      "sep",
      { label: t("↑ 上移"), run: () => api.workflowMove(id, -1).then(refreshWorkflows).catch((err) => toast("error", String(err))) },
      { label: t("↓ 下移"), run: () => api.workflowMove(id, 1).then(refreshWorkflows).catch((err) => toast("error", String(err))) },
      { label: t("⧉ 复制"), run: () => copyWorkflow(w) },
      { label: t("📋 粘贴为副本"), run: () => void pasteWorkflow() },
      "sep",
      { label: t("🗑 删除工作流"), danger: true, run: () => {
          void (async () => {
            if (!(await confirmDialog({ title: t("删除工作流"), message: t("删除工作流「{p0}」？运行历史将一并删除。", { p0: w.name }), danger: true, confirmText: t("删除") }))) return;
            try {
              await api.workflowDelete(id);
              await refreshWorkflows();
              if (app.workflowSelected === id) app.workflowSelected = null;
              toast("ok", t("工作流已删除"));
            } catch (err) {
              toast("error", String(err));
            }
          })();
      } },
    ];
    menu = { x: e.clientX, y: e.clientY, items };
  }

</script>

<aside class="sidebar">
  <!-- project switcher -->
  <div class="proj">
    <button class="btn sm projbtn add" onclick={openNewProject}><Icon name="plus" size={14} /> {t("添加项目")}</button>
    <div class="selrow">
      <select
        class="psel"
        value={app.projectId ?? ""}
        onchange={async (e) => {
          const v = (e.target as HTMLSelectElement).value;
          await selectProject(v || null);
        }}
      >
        {#if !app.projects.length}
          <option value="">{t("（无项目）")}</option>
        {/if}
        {#each app.projects as p (p.id)}
          <option value={p.id}>{isNoProject(p.id) ? t("无项目") : p.name}</option>
        {/each}
      </select>
      {#if app.projectId}
        <button class="btn ghost sm sq" title={t("编辑项目")} onclick={openEditProject}><Icon name="pencil" size={14} /></button>
      {/if}
    </div>
  </div>

  <!-- contexts (top half) —— 无项目只存工作流，不展示上下文 -->
  {#if !isNoProject(app.projectId)}
  <div class="sec">
    <div class="sec-head">
      <Icon name="context" size={13} /> {t("上下文")} <span class="spacer"></span>
      <button class="btn ghost sm" title={t("新建上下文")} disabled={!app.projectId} onclick={() => (showNewContext = true)}><Icon name="plus" size={13} /></button>
    </div>
    <div class="list ctx-list" oncontextmenu={ctxBlankMenu}>
      {#each app.contexts as c (c.id)}
        <div
          class="item"
          class:active={app.contextId === c.id && app.tab === "chat"}
          role="button"
          tabindex="0"
          onclick={() => {
            app.tab = "chat";
            void selectContext(c.id);
          }}
          oncontextmenu={(e) => contextMenu(e, c.id)}
        >
          <span class="iname">{c.name}</span>
        </div>
      {:else}
        <div class="none">{app.projectId ? t("还没有上下文") : t("先选择项目")}</div>
      {/each}
    </div>
  </div>

  {/if}

  <!-- workflows (bottom half) -->
  <div class="sec grow">
    <div class="sec-head">
      <Icon name="workflow" size={13} /> {t("工作流")} <span class="spacer"></span>
      <button
        class="btn ghost sm"
        title={t("新建工作流")}
        disabled={!app.projectId}
        onclick={() => void newWorkflow()}><Icon name="plus" size={13} /></button
      >
    </div>
    <div class="list wf-list" oncontextmenu={wfBlankMenu}>
      {#each app.workflows as w (w.id)}
        <div
          class="item"
          class:active={app.tab === "workflows" && app.workflowSelected === w.id}
          role="button"
          tabindex="0"
          onclick={() => {
            app.tab = "workflows";
            app.workflowSelected = w.id;
          }}
          oncontextmenu={(e) => wfMenu(e, w.id)}
        >
          <span class="iname">{w.name}</span>
          <span class="imeta">
            {#if w.trigger_type === "schedule" && w.enabled}<span class="badge accent">⏰ {scheduleText(w.schedule)}</span>{:else}<span class="badge">{t("手动")}</span>{/if}
            <span class="dim">{w.steps.length} {t("节点")}</span>
          </span>
        </div>
      {:else}
        <div class="none">{app.projectId ? t("还没有工作流") : t("先选择项目")}</div>
      {/each}
    </div>
  </div>

</aside>


{#if showNewProject}
  <div class="modal-backdrop">
    <div class="modal">
      <header>{t("新建项目")} <button class="btn ghost sm" onclick={() => (showNewProject = false)}>✕</button></header>
      <div class="body col">
        <div class="field"><label>{t("名称 *")}</label><input bind:value={projectName} placeholder={t("例如：我的项目")} onkeydown={(e) => e.key === "Enter" && createProject()} /></div>
        <div class="field"><label>{t("根目录")}</label><input bind:value={projectPath} placeholder="D:\path\to\project" /></div>
        <div class="field"><label>{t("描述")}</label><textarea rows="2" bind:value={projectDesc} placeholder={t("项目简介")}></textarea></div>
      </div>
      <footer>
        <button class="btn" onclick={() => (showNewProject = false)}>{t("取消")}</button>
        <button class="btn primary" onclick={createProject}>{t("创建")}</button>
      </footer>
    </div>
  </div>
{/if}

{#if showEditProject}
  <div class="modal-backdrop">
    <div class="modal">
      <header>{t("编辑项目")} <button class="btn ghost sm" onclick={() => (showEditProject = false)}>✕</button></header>
      <div class="body col">
        <div class="field"><label>{t("名称 *")}</label><input bind:value={editName} /></div>
        <div class="field"><label>{t("根目录")}</label><input bind:value={editPath} placeholder="D:\path\to\project" /></div>
        <div class="field"><label>{t("描述")}</label><textarea rows="2" bind:value={editDesc}></textarea></div>
      </div>
      <footer>
        <button class="btn danger left" onclick={deleteProject}>{t("删除项目")}</button>
        <span class="spacer"></span>
        <button class="btn" onclick={() => (showEditProject = false)}>{t("取消")}</button>
        <button class="btn primary" onclick={saveProject}>{t("保存")}</button>
      </footer>
    </div>
  </div>
{/if}

{#if showNewContext}
  <div class="modal-backdrop">
    <div class="modal">
      <header>{t("新建上下文")} <button class="btn ghost sm" onclick={() => (showNewContext = false)}>✕</button></header>
      <div class="body col">
        <div class="field">
          <label>{t("名称 *")}</label>
          <input bind:value={contextName} placeholder={t("例如：主开发 / Bug修复 / UI重构")} onkeydown={(e) => e.key === "Enter" && createContext()} />
        </div>
        <p class="note">{t("Context 不绑定某一种 AI，可分别与 Codex / ZCode 新建会话或绑定历史会话。")}</p>
      </div>
      <footer>
        <button class="btn" onclick={() => (showNewContext = false)}>{t("取消")}</button>
        <button class="btn primary" onclick={createContext}>{t("创建")}</button>
      </footer>
    </div>
  </div>
{/if}

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

<style>
  .sidebar {
    background: var(--bg-panel);
    border-right: 1px solid var(--border-soft);
    display: flex;
    flex-direction: column;
    min-height: 0;
    /* 固定宽度：不随项目/上下文名称长短抖动，超长内容由内部省略号消化 */
    flex: none;
    width: 240px;
    min-width: 240px;
  }
  .proj {
    padding: 10px 12px 8px;
    border-bottom: 1px solid var(--border-soft);
  }
  .plabel {
    font-size: 0.74em;
    color: var(--text-faint);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    display: block;
    margin-bottom: 4px;
  }
  .psel {
    width: 100%;
  }
  .add {
    width: calc(100% - 0px);
    justify-content: center;
    margin-bottom: 8px;
  }
  .selrow {
    display: flex;
    gap: 6px;
    align-items: stretch;
  }
  .selrow .psel {
    flex: 1;
    min-width: 0;
  }
  .sq {
    flex: none;
    display: flex;
    align-items: center;
  }
  .projbtn {
    flex: 1;
    justify-content: center;
    padding: 8px 10px;
  }
  .psel {
    height: 34px;
    line-height: 32px;
    padding: 0 30px 0 10px;
    font-weight: 600;
    appearance: none;
    background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 24 24' fill='none' stroke='%239ba3b8' stroke-width='2.2' stroke-linecap='round'><path d='M6 9l6 6 6-6'/></svg>");
    background-repeat: no-repeat;
    background-position: right 9px center;
    cursor: pointer;
  }
  .psel option {
    background: var(--bg-panel);
    color: var(--text);
    padding: 8px 10px;
  }
  .sec {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: none;
    max-height: 52%;
  }
  .sec.grow {
    flex: 1;
    max-height: none;
    border-top: 1px solid var(--border-soft);
  }
  .sec-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 9px 12px 5px;
    font-size: 0.78em;
    color: var(--text-faint);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .spacer {
    flex: 1;
  }
  .list {
    overflow: auto;
    padding: 2px 8px 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-height: 56px;
  }
  .ctx-list {
    flex: none;
    max-height: 360px;
  }
  .wf-list {
    flex: 1;
  }
  .item {
    padding: 7px 10px;
    border-radius: calc(var(--radius) - 4px);
    cursor: pointer;
    color: var(--text-dim);
    border: 1px solid transparent;
  }
  .item:hover {
    background: var(--bg-elev);
    color: var(--text);
  }
  .item.active {
    background: var(--bg-elev2);
    color: var(--text);
    border-color: var(--border);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .iname {
    display: block;
    font-size: 0.92em;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .imeta {
    display: flex;
    gap: 6px;
    align-items: center;
    margin-top: 3px;
    font-size: 0.78em;
  }
  .dim {
    color: var(--text-faint);
  }
  .none {
    color: var(--text-faint);
    text-align: center;
    padding: 14px 6px;
    font-size: 0.85em;
  }
  .ctxname {
    margin-left: auto;
    font-size: 0.85em;
    color: var(--text-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 45%;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .note {
    color: var(--text-faint);
    font-size: 0.85em;
  }
  .spacer {
    flex: 1;
  }
</style>
