<script lang="ts">
  import { onMount } from "svelte";
  import { subscribe, setUnauthorizedHandler } from "./api";
  import { ui, bootstrap, handleEvent, setTheme, loadDesktopTheme, nextTheme, THEME_LABEL, setView, selectProject, selectContext, currentProject, currentContext, logout, type View } from "./store.svelte";
  import Icon from "./Icon.svelte";
  import Login from "./Login.svelte";
  import ChatPane from "./ChatPane.svelte";
  import FilesPane from "./FilesPane.svelte";
  import WorkflowsPane from "./WorkflowsPane.svelte";
  import ContextPane from "./ContextPane.svelte";
  import RemotePane from "./RemotePane.svelte";
  import logo from "../assets/logo.png";
  import PermissionSheet from "./PermissionSheet.svelte";

  const NAV: { id: View; label: string; icon: string }[] = [
    { id: "chat", label: "会话", icon: "chat" },
    { id: "files", label: "文件", icon: "folder" },
    { id: "workflows", label: "工作流", icon: "flow" },
    { id: "context", label: "上下文", icon: "book" },
    { id: "remote", label: "编程接入", icon: "plug" },
  ];

  let unsub: (() => void) | null = null;

  setTheme(ui.theme);
  setUnauthorizedHandler(() => { ui.authed = false; });

  onMount(() => {
    void bootstrap();
    void loadDesktopTheme();
    // 回到页面时同步电脑端可能已切换的配色
    const onVis = () => { if (document.visibilityState === "visible") void loadDesktopTheme(); };
    document.addEventListener("visibilitychange", onVis);
    return () => { unsub?.(); document.removeEventListener("visibilitychange", onVis); };
  });
  const themeIcon = $derived(ui.theme === "desktop" ? "monitor" : ui.theme === "dark" ? "moon" : "sun");

  // 登录状态变化时开 / 关实时通道
  $effect(() => {
    if (ui.authed) {
      unsub ??= subscribe(handleEvent, (c) => (ui.connected = c));
    } else if (unsub) {
      unsub();
      unsub = null;
      ui.connected = false;
    }
  });

  const runningCount = $derived(Object.keys(ui.wfRunning).length);
  const projectName = $derived(currentProject()?.name ?? "未选择项目");
  const contextName = $derived(currentContext()?.name ?? "未选择上下文");
</script>

{#if ui.authed === null}
  <div class="boot"><span class="spin"></span></div>
{:else if !ui.authed}
  <Login />
{:else}
  <div class="shell" class:drawer-open={ui.drawer}>
    <!-- 侧栏：宽屏常驻，窄屏为抽屉 -->
    <aside class="side">
      <div class="brand">
        <img class="logo" src={logo} alt="使驾" draggable="false" />
        <div class="brand-txt"><b>使驾 ShiDrive</b><small>网站接入 · v{ui.version}</small></div>
        <button class="icon-btn only-narrow" onclick={() => (ui.drawer = false)} aria-label="关闭"><Icon name="x" /></button>
      </div>

      <label class="lbl" for="sd-project">项目</label>
      <select id="sd-project" class="select" value={ui.projectId ?? ""} onchange={(e) => void selectProject((e.currentTarget as HTMLSelectElement).value || null)}>
        {#each ui.projects as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
        {#if !ui.projects.length}<option value="">（暂无项目）</option>{/if}
      </select>

      <div class="lbl">上下文</div>
      <div class="ctx-list scroll">
        {#each ui.contexts as c (c.id)}
          <button class="ctx" class:on={c.id === ui.contextId} onclick={() => selectContext(c.id)}>
            <Icon name="book" size={15} /><span>{c.name}</span>
          </button>
        {:else}
          <div class="faint small">该项目还没有上下文，请在电脑端创建</div>
        {/each}
      </div>

      <nav class="side-nav only-wide">
        {#each NAV as n (n.id)}
          <button class:on={ui.view === n.id} onclick={() => setView(n.id)}>
            <Icon name={n.icon} /><span>{n.label}</span>
            {#if n.id === "workflows" && runningCount}<span class="badge run">{runningCount}</span>{/if}
          </button>
        {/each}
      </nav>

      <div class="side-foot">
        <span class="conn" class:on={ui.connected} title={ui.connected ? "实时连接正常" : "实时连接断开，正在重连"}></span>
        <span class="faint small">{ui.connected ? "已连接" : "重连中…"}</span>
        <span class="grow"></span>
        <button class="icon-btn" onclick={() => setTheme(nextTheme(ui.theme))} title="主题：{THEME_LABEL[ui.theme]}（点击切换）" aria-label="切换主题"><Icon name={themeIcon} /></button>
        <button class="icon-btn" onclick={() => { if (confirm("退出登录？")) void logout(); }} aria-label="退出登录"><Icon name="logout" /></button>
      </div>
    </aside>
    <button class="scrim" aria-label="关闭菜单" onclick={() => (ui.drawer = false)}></button>

    <div class="main">
      <!-- 窄屏顶栏 -->
      <header class="topbar only-narrow">
        <button class="icon-btn" onclick={() => (ui.drawer = true)} aria-label="菜单"><Icon name="menu" /></button>
        <button class="crumb" onclick={() => (ui.drawer = true)}>
          <b>{contextName}</b><small>{projectName}</small>
        </button>
        <span class="conn" class:on={ui.connected}></span>
      </header>

      <section class="view">
        {#if ui.view === "chat"}<ChatPane />
        {:else if ui.view === "files"}<FilesPane />
        {:else if ui.view === "workflows"}<WorkflowsPane />
        {:else if ui.view === "remote"}<RemotePane />
        {:else}<ContextPane />{/if}
      </section>

      <nav class="tabbar only-narrow">
        {#each NAV as n (n.id)}
          <button class:on={ui.view === n.id} onclick={() => setView(n.id)}>
            <span class="ti"><Icon name={n.icon} size={20} />{#if n.id === "workflows" && runningCount}<i class="dotn">{runningCount}</i>{/if}</span>
            <span>{n.label}</span>
          </button>
        {/each}
      </nav>
    </div>
  </div>
  <PermissionSheet />
{/if}

<div class="toasts">
  {#each ui.toasts as t (t.id)}<div class="toast {t.kind}">{t.text}</div>{/each}
</div>

<style>
  .boot { height: 100%; display: grid; place-items: center; }
  .shell { height: 100%; display: grid; grid-template-columns: 272px 1fr; }
  .side {
    background: var(--bg-panel); border-right: 1px solid var(--border-soft);
    display: flex; flex-direction: column; gap: 8px; padding: 14px 12px calc(10px + var(--safe-b)); min-height: 0;
  }
  .brand { display: flex; align-items: center; gap: 10px; padding: 2px 4px 10px; }
  .logo { width: 34px; height: 34px; border-radius: 10px; flex: none; object-fit: contain; }
  .brand-txt { display: flex; flex-direction: column; line-height: 1.25; flex: 1; min-width: 0; }
  .brand-txt small { color: var(--text-faint); font-size: 12px; }
  .lbl { font-size: 12px; color: var(--text-faint); padding: 6px 4px 0; letter-spacing: .04em; }
  .ctx-list { flex: 1; min-height: 60px; display: flex; flex-direction: column; gap: 2px; }
  .ctx {
    display: flex; align-items: center; gap: 8px; padding: 9px 10px; border-radius: 10px; text-align: left; color: var(--text-dim);
  }
  .ctx span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ctx:hover { background: var(--bg-elev); color: var(--text); }
  .ctx.on { background: var(--accent-soft); color: var(--accent); font-weight: 600; }
  .small { font-size: 12.5px; padding: 6px 4px; }
  .side-nav { display: flex; flex-direction: column; gap: 2px; border-top: 1px solid var(--border-soft); padding-top: 8px; }
  .side-nav button { display: flex; align-items: center; gap: 10px; padding: 9px 10px; border-radius: 10px; color: var(--text-dim); }
  .side-nav button:hover { background: var(--bg-elev); color: var(--text); }
  .side-nav button.on { background: var(--bg-elev2); color: var(--text); font-weight: 600; }
  .side-nav .badge { margin-left: auto; }
  .side-foot { display: flex; align-items: center; gap: 4px; border-top: 1px solid var(--border-soft); padding-top: 8px; }
  .grow { flex: 1; }
  .conn { width: 8px; height: 8px; border-radius: 50%; background: var(--warn); flex: none; margin: 0 4px; }
  .conn.on { background: var(--ok); box-shadow: 0 0 6px var(--ok); }
  .scrim { display: none; }
  .main { display: flex; flex-direction: column; min-width: 0; min-height: 0; }
  .view { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .only-narrow { display: none; }

  @media (max-width: 899px) {
    .shell { grid-template-columns: 1fr; }
    .only-wide { display: none !important; }
    .only-narrow { display: flex; }
    .side {
      position: fixed; inset: 0 auto 0 0; width: min(86vw, 320px); z-index: 40;
      transform: translateX(-102%); transition: transform .22s ease; box-shadow: var(--shadow);
      padding-top: calc(14px + var(--safe-t));
    }
    .drawer-open .side { transform: none; }
    .scrim { display: block; position: fixed; inset: 0; background: rgba(0, 0, 0, .45); z-index: 39; opacity: 0; pointer-events: none; transition: opacity .2s; }
    .drawer-open .scrim { opacity: 1; pointer-events: auto; }
    .topbar {
      align-items: center; gap: 6px; padding: calc(6px + var(--safe-t)) 8px 6px; background: var(--bg-panel);
      border-bottom: 1px solid var(--border-soft); flex: none;
    }
    .crumb { flex: 1; min-width: 0; display: flex; flex-direction: column; align-items: flex-start; line-height: 1.2; text-align: left; }
    .crumb b, .crumb small { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .crumb small { color: var(--text-faint); font-size: 12px; }
    .tabbar {
      flex: none; background: var(--bg-panel); border-top: 1px solid var(--border-soft);
      padding: 4px 4px calc(4px + var(--safe-b));
    }
    .tabbar button { flex: 1; display: flex; flex-direction: column; align-items: center; gap: 1px; padding: 5px 0; font-size: 11.5px; color: var(--text-faint); border-radius: 10px; }
    .tabbar button.on { color: var(--accent); }
    .ti { position: relative; }
    .dotn { position: absolute; top: -4px; right: -10px; font-style: normal; font-size: 10px; background: var(--accent); color: var(--accent-contrast); border-radius: 8px; padding: 0 4px; line-height: 14px; }
  }

  .toasts { position: fixed; left: 50%; transform: translateX(-50%); bottom: calc(76px + var(--safe-b)); z-index: 100; display: flex; flex-direction: column; gap: 8px; align-items: center; pointer-events: none; width: min(92vw, 420px); }
  .toast { background: var(--bg-elev2); border: 1px solid var(--border); color: var(--text); padding: 9px 14px; border-radius: 12px; box-shadow: var(--shadow); font-size: 14px; animation: rise .2s ease; max-width: 100%; }
  .toast.ok { border-color: color-mix(in srgb, var(--ok) 50%, transparent); }
  .toast.warn { border-color: color-mix(in srgb, var(--warn) 50%, transparent); }
  .toast.error { border-color: color-mix(in srgb, var(--danger) 60%, transparent); color: var(--danger); }
  @keyframes rise { from { transform: translateY(8px); opacity: 0; } }
</style>
