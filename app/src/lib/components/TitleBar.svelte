<script lang="ts">
  import { t } from "../i18n";
  import { app, toast } from "../state.svelte";
  import Icon from "./Icon.svelte";
  import logo from "../../assets/logo.png";

  let maximized = $state(false);

  async function w(fn: string) {
    try {
      const win = (window as any).__TAURI__.window.getCurrentWindow();
      if (fn === "minimize") await win.minimize();
      else if (fn === "toggleMaximize") {
        await win.toggleMaximize();
        maximized = !maximized;
      } else if (fn === "close") await win.close();
    } catch (e) {
      toast("error", String(e));
    }
  }
</script>

<header class="titlebar">
  <div class="left" data-tauri-drag-region>
    <img src={logo} alt={t("使驾")} class="logo" />
    <span class="name" data-tauri-drag-region>{#if app.locale !== "en"}使驾 {/if}<b>ShiDrive</b></span>
  </div>

  <div class="quick">
    <button class="qbtn" class:active={app.overlay === "sessions"} title={t("会话管理")} onclick={() => (app.overlay = app.overlay === "sessions" ? null : "sessions")}>
      <Icon name="sessions" size={15} /> {t("会话管理")}
    </button>
    <button class="qbtn" class:active={app.overlay === "tasks"} title={t("运行中任务")} onclick={() => (app.overlay = app.overlay === "tasks" ? null : "tasks")}>
      <Icon name="tasks" size={15} /> {t("运行中任务")}
    </button>
    <button class="qbtn" class:active={app.overlay === "prompts"} title={t("常用提示词")} onclick={() => (app.overlay = app.overlay === "prompts" ? null : "prompts")}>
      <Icon name="prompts" size={15} /> {t("常用提示词")}
    </button>
    <span class="divider"></span>
    <button class="qbtn icon-only" class:active={app.settingsOpen} title={t("设置")} onclick={() => (app.settingsOpen = true)}>
      <Icon name="settings" size={16} />
    </button>
  </div>

  <div class="winctrls">
    <button class="wbtn" title={t("最小化")} onclick={() => w("minimize")}>
      <svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true"><path d="M1 6h10" stroke="currentColor" stroke-width="1" /></svg>
    </button>
    <button class="wbtn" title={t("最大化/还原")} onclick={() => w("toggleMaximize")}>
      {#if maximized}
        <svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true">
          <rect x="1" y="3.5" width="7.5" height="7.5" fill="none" stroke="currentColor" stroke-width="1" />
          <path d="M3.5 3.5V1H11v7.5H8.5" fill="none" stroke="currentColor" stroke-width="1" />
        </svg>
      {:else}
        <svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true">
          <rect x="1" y="1" width="10" height="10" fill="none" stroke="currentColor" stroke-width="1" />
        </svg>
      {/if}
    </button>
    <button class="wbtn close" title={t("关闭")} onclick={() => w("close")}>
      <svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true"><path d="M1 1l10 10M11 1L1 11" stroke="currentColor" stroke-width="1" /></svg>
    </button>
  </div>
</header>

<style>
  .titlebar {
    height: 44px;
    display: flex;
    align-items: center;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border-soft);
    -webkit-user-select: none;
    user-select: none;
  }
  .left {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 12px;
    height: 100%;
    flex: 1;
    min-width: 0;
  }
  .logo {
    width: 24px;
    height: 24px;
    border-radius: 6px;
  }
  .name {
    font-size: 0.95em;
    color: var(--text-dim);
    white-space: nowrap;
  }
  .name b {
    color: var(--text);
  }
  .quick {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 6px;
  }
  .qbtn {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 11px;
    border-radius: 8px;
    color: var(--text-dim);
    font-size: 0.9em;
  }
  .qbtn:hover {
    background: var(--bg-elev2);
    color: var(--text);
  }
  .qbtn.active {
    background: var(--bg-elev2);
    color: var(--accent);
  }
  .qbtn.icon-only {
    padding: 7px;
  }
  .divider {
    width: 1px;
    height: 20px;
    background: var(--border);
    margin: 0 4px;
  }
  .winctrls {
    display: flex;
    height: 100%;
  }
  .wbtn {
    width: 44px;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-dim);
  }
  .wbtn:hover {
    background: var(--bg-elev2);
    color: var(--text);
  }
  .wbtn.close:hover {
    background: var(--danger);
    color: #fff;
  }
  .wbtn svg {
    display: block;
  }
</style>
