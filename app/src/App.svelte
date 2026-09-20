<script lang="ts">
  import { onMount } from "svelte";
  import { app, loadProjects, currentProject, currentContext, loadPrompts, isNoProject } from "./lib/state.svelte";
  import { api } from "./lib/ipc";
  import { applyTheme } from "./lib/theme";
  import { wireEvents } from "./lib/events";
  import TitleBar from "./lib/components/TitleBar.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import ChatView from "./lib/components/ChatView.svelte";
  import ContextView from "./lib/components/ContextView.svelte";
  import WorkflowsView from "./lib/components/WorkflowsView.svelte";
  import FileTree from "./lib/components/FileTree.svelte";
  import FileEditor from "./lib/components/FileEditor.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";
  import Toasts from "./lib/components/Toasts.svelte";
  import PermissionDialog from "./lib/components/PermissionDialog.svelte";
  import SessionManagerModal from "./lib/components/SessionManagerModal.svelte";
  import TasksPanel from "./lib/components/TasksPanel.svelte";
  import PromptsPanel from "./lib/components/PromptsPanel.svelte";
  import HistoryBindModal from "./lib/components/HistoryBindModal.svelte";
  import DialogHost from "./lib/components/DialogHost.svelte";
  import Icon from "./lib/components/Icon.svelte";
  import logo from "./assets/logo.png";

  $effect(() => {
    applyTheme(app.theme);
  });

  // 禁用浏览器默认右键菜单（自定义菜单在各自组件内处理 contextmenu 并已 preventDefault）
  // Konami Code（上上下下左右左右BA）唤出 DevTools
  let konamiBuf: string[] = [];
  const KONAMI = ["arrowup", "arrowup", "arrowdown", "arrowdown", "arrowleft", "arrowright", "arrowleft", "arrowright", "b", "a"];
  function onKeydown(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
    konamiBuf.push(e.key.toLowerCase());
    konamiBuf = konamiBuf.slice(-KONAMI.length);
    if (konamiBuf.length === KONAMI.length && konamiBuf.every((k, i) => k === KONAMI[i])) {
      konamiBuf = [];
      void toggleDevtools();
    }
  }
  async function toggleDevtools() {
    try {
      const tauri = (window as any).__TAURI__;
      const win = tauri.webviewWindow.getCurrentWebviewWindow();
      await win.toggleDevtools();
    } catch {
      console.warn("DevTools 仅在调试构建中可用");
    }
  }

  onMount(() => {
    document.addEventListener("contextmenu", blockMenu);
    void boot();
    return () => {
      document.removeEventListener("contextmenu", blockMenu);
    };
  });

  function blockMenu(e: MouseEvent) {
    e.preventDefault();
  }

  async function boot() {
    await wireEvents();
    const t = await api.settingsGet("ui.theme").catch(() => null);
    if (t) {
      try {
        Object.assign(app.theme, JSON.parse(t));
      } catch {
        /* keep defaults */
      }
    }
    const port = await api.settingsGet("mcp.port").catch(() => null);
    if (port) app.mcpPort = Number(port) || 8345;
    // 加载启用的 agent（顺序即会话页 tab 顺序）与缓存能力
    const enabled = await api.agentsEnabledGet().catch(() => [] as string[]);
    const reg = await api.agentsRegistry().catch(() => []);
    app.agents = enabled
      .map((id) => reg.find((r) => r.id === id))
      .filter((r): r is NonNullable<typeof r> => !!r)
      .map((r) => ({ id: r.id, name: r.name }));
    if (!app.agent && app.agents.length) app.agent = app.agents[0].id;
    for (const r of reg) {
      const raw = await api.settingsGet("caps." + r.id).catch(() => null);
      if (raw) {
        try {
          app.agentCaps[r.id] = JSON.parse(raw);
        } catch {
          /* ignore */
        }
      }
    }
    await loadProjects(true);
    await loadPrompts();
    app.ready = true;
    for (const a of app.agents) {
      api
        .acpStatus(a.id)
        .then((s) => (app.agentStatus[a.id] = s))
        .catch(() => {});
    }
  }

  // 编辑器仅与聊天共存；工作流等其他页面时自动让位（状态保留，回到聊天恢复显示）
  const editorVisible = $derived(!!app.editor && !app.editor.collapsed && app.tab === "chat");
</script>

<div class="root">
  <TitleBar />
  <div class="body">
    <Sidebar />
    <div class="center">
      <main class="main">
        {#if currentContext() || (app.tab === "workflows" && currentProject())}
          <header class="tabsbar">
            <nav class="tabs">
              <button class="tab" class:active={app.tab === "chat"} disabled={!app.contextId} onclick={() => (app.tab = "chat")}>
                <Icon name="chat" size={14} /> 聊天
              </button>
              <button class="tab" class:active={app.tab === "workflows"} disabled={!app.projectId} onclick={() => (app.tab = "workflows")}>
                <Icon name="workflow" size={14} /> 工作流
              </button>
            </nav>
            <div class="crumb">
              <span class="proj">{currentProject()?.name}</span>
              {#if currentContext()}<span class="sep">/</span><span class="ctx">{currentContext()?.name}</span>{/if}
            </div>
            <div class="spacer"></div>
            {#if app.projectId}
              <button
                class="btn ghost sm"
                title={app.fileTreeOpen ? "隐藏文件树" : "显示文件树"}
                onclick={() => (app.fileTreeOpen = !app.fileTreeOpen)}
              >
                <Icon name="tree" size={15} />
              </button>
            {/if}
          </header>
        {/if}
        <div class="view">
          {#if app.tab === "chat" && currentContext()}
            <ChatView />
          {:else if app.tab === "context" && currentContext()}
            <ContextView />
          {:else if app.tab === "workflows" && currentProject()}
            <WorkflowsView />
          {:else}
            <div class="empty welcome">
              <img src={logo} alt="使驾" class="wlogo" />
              <h2>欢迎来到使驾 ShiDrive</h2>
              <p>统一管理项目、工作上下文、AI Agent 会话与工作流</p>
              <p class="hint">左侧选择或创建一个项目开始 · AI 是发动机，Agent 是车辆，使驾是驾驶席</p>
            </div>
          {/if}
        </div>
      </main>
      {#if editorVisible}
        <div class="editor-pane half">
          <FileEditor />
        </div>
      {/if}
    </div>
    {#if app.projectId && !isNoProject(app.projectId) && app.fileTreeOpen}
      <aside class="filedock">
        <FileTree />
      </aside>
    {/if}
  </div>
</div>

<SettingsModal />
<SessionManagerModal />
<TasksPanel />
<PromptsPanel />
<HistoryBindModal />
<PermissionDialog />
<Toasts />
<DialogHost />

<style>
  .root {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .center {
    flex: 1;
    min-width: 0;
    display: flex;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  .editor-pane {
    flex: 1;
    min-width: 0;
    border-left: 1px solid var(--border-soft);
    display: flex;
  }
  .editor-pane.half {
    flex: 0 0 50%;
  }
  .tabsbar {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 8px 14px;
    border-bottom: 1px solid var(--border-soft);
  }
  .tabs {
    display: flex;
    gap: 4px;
    background: var(--bg-panel);
    border: 1px solid var(--border-soft);
    padding: 3px;
    border-radius: calc(var(--radius) + 2px);
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 13px;
    border-radius: calc(var(--radius) - 2px);
    color: var(--text-dim);
    transition: all 0.15s;
  }
  .tab:hover:not(:disabled) {
    color: var(--text);
    background: var(--bg-elev);
  }
  .tab.active {
    background: var(--accent);
    color: var(--accent-contrast);
    font-weight: 600;
  }
  .tab:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .crumb {
    display: flex;
    gap: 8px;
    align-items: baseline;
    min-width: 0;
    white-space: nowrap;
  }
  .crumb .proj {
    color: var(--text-dim);
  }
  .crumb .ctx {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sep {
    color: var(--text-faint);
  }
  .spacer {
    flex: 1;
  }
  .view {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .filedock {
    width: 246px;
    flex: none;
    border-left: 1px solid var(--border-soft);
    background: var(--bg-panel);
    display: flex;
    min-height: 0;
  }
  .welcome {
    flex: 1;
    align-self: center;
  }
  .wlogo {
    width: 84px;
    height: 84px;
    border-radius: 22px;
    margin-bottom: 8px;
  }
  .welcome h2 {
    color: var(--text);
  }
  .welcome .hint {
    font-size: 0.88em;
    opacity: 0.7;
  }
</style>
