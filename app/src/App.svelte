<script lang="ts">
  import { onMount } from "svelte";
  import { app, loadProjects, currentProject, currentContext, loadPrompts, isNoProject, loadCfgPrefs } from "./lib/state.svelte";
  import { api } from "./lib/ipc";
  import { applyTheme } from "./lib/theme";
  import { activateSkin, loadSkinOpacity } from "./lib/skins.svelte";
  import { toast as skinToast } from "./lib/state.svelte";
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
  import OAuthConsent from "./lib/components/OAuthConsent.svelte";
  import ElicitationDialog from "./lib/components/ElicitationDialog.svelte";
  import SessionManagerModal from "./lib/components/SessionManagerModal.svelte";
  import TasksPanel from "./lib/components/TasksPanel.svelte";
  import PromptsPanel from "./lib/components/PromptsPanel.svelte";
  import HistoryBindModal from "./lib/components/HistoryBindModal.svelte";
  import DialogHost from "./lib/components/DialogHost.svelte";
  import Icon from "./lib/components/Icon.svelte";
import SkinCharacter from "./lib/components/SkinCharacter.svelte";
  import logo from "./assets/logo.png";
  import { t } from "./lib/i18n";

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
      console.warn(t("DevTools 仅在调试构建中可用"));
    }
  }

  onMount(() => {
    document.addEventListener("contextmenu", blockMenu);
    void boot();
    return () => {
      document.removeEventListener("contextmenu", blockMenu);
    };
  });

  // One lifecycle owns preset tokens, custom assets and complete cleanup.
  $effect(() => activateSkin(app.skin, (message) => {
    skinToast("error", `${t("皮肤加载失败，已恢复基础主题")}: ${message}`);
    app.skin = "";
  }));

  function blockMenu(e: MouseEvent) {
    // 组件内已自行处理的自定义菜单（画布/文件树/正文等）会先 preventDefault，
    // 冒泡到这里时直接放行；其余场景仍拦截浏览器默认右键菜单
    if (e.defaultPrevented) return;
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
    // 界面语言与皮肤
    const locale = await api.settingsGet("ui.locale").catch(() => null);
    if (locale === "en" || locale === "zh") app.locale = locale;
    const skin = await api.settingsGet("ui.skin").catch(() => null);
    app.skin = skin ?? "";
    await loadSkinOpacity();
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
    loadCfgPrefs();
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
                <Icon name="chat" size={14} /> {t("聊天")}
              </button>
              <button class="tab" class:active={app.tab === "workflows"} disabled={!app.projectId} onclick={() => (app.tab = "workflows")}>
                <Icon name="workflow" size={14} /> {t("工作流")}
              </button>
            </nav>
            <div class="crumb">
              <span class="proj">{currentProject()?.name}</span>
              {#if currentContext()}<span class="sep">/</span><span class="ctx">{currentContext()?.name}</span>{/if}
            </div>
            <div class="spacer"></div>
            {#if (app.tab === "chat" || app.tab === "context") && currentContext()}
              <button
                class="btn ghost sm"
                class:active={app.tab === "context"}
                title={t("共享上下文")}
                onclick={() => (app.tab = app.tab === "context" ? "chat" : "context")}
              >
                <Icon name="context" size={15} />
              </button>
            {/if}
            {#if app.projectId}
              <button
                class="btn ghost sm"
                title={app.fileTreeOpen ? t("隐藏文件树") : t("显示文件树")}
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
              <img src={logo} alt={t("使驾")} class="wlogo" />
              <h2>{t("欢迎来到使驾 ShiDrive")}</h2>
              <p>{t("One Context. Any Harness. —— 让 Harness 并驾齐驱")}</p>
              <p class="hint">{t("统一管理项目、工作上下文、AI Agent 会话与工作流 · 左侧选择或创建一个项目开始")}</p>
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
<ElicitationDialog />
<Toasts />
<DialogHost />
<OAuthConsent />
<!-- 皮肤人物：顶层悬浮于左下角，不占布局、不挡操作 -->
<SkinCharacter />

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
