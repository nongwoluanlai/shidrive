<script lang="ts">
  import { app, toast, saveTheme } from "../state.svelte";
  import { api } from "../ipc";
  import { ACCENTS } from "../theme";
  import AgentsAdmin from "./AgentsAdmin.svelte";
  import type { AgentEnvStatusItem } from "../types";

  let tab = $state<"agents" | "theme" | "paths">("theme");
  let registry = $state<AgentEnvStatusItem[]>([]);

  // --- paths / ports editing state ---
  let mcpPort = $state("");
  let pythonPath = $state("");
  let nodePath = $state("");
  let proxy = $state("");
  let pathsLoaded = $state(false);

  async function loadRegistry() {
    registry = await api.agentsRegistry().catch(() => []);
  }

  async function loadPathSettings() {
    mcpPort = String((await api.settingsGet("mcp.port").catch(() => null)) ?? "");
    pythonPath = (await api.settingsGet("tools.python").catch(() => null)) ?? "";
    nodePath = (await api.settingsGet("tools.node").catch(() => null)) ?? "";
    proxy = (await api.settingsGet("network.proxy").catch(() => null)) ?? "";
    pathsLoaded = true;
  }

  $effect(() => {
    if (app.settingsOpen && !pathsLoaded) void loadPathSettings();
  });

  async function savePaths(clear = false) {
    try {
      const pairs: [string, string][] = clear
        ? [
            ["mcp.port", ""],
            ["tools.python", ""],
            ["tools.node", ""],
            ["network.proxy", ""],
          ]
        : [
            ["mcp.port", mcpPort.trim()],
            ["tools.python", pythonPath.trim()],
            ["tools.node", nodePath.trim()],
            ["network.proxy", proxy.trim()],
          ];
      for (const [k, v] of pairs) await api.settingsSet(k, v);
      if (!clear && mcpPort.trim()) app.mcpPort = Number(mcpPort.trim()) || app.mcpPort;
      await loadPathSettings();
      toast("ok", clear ? "已清除自定义路径（重启生效）" : "已保存（MCP 端口与解释器重启后完全生效；代理仅用于适配器安装）");
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function pickThemePreset(preset: string) {
    app.theme.preset = preset as "dark" | "light";
    await saveTheme();
  }

  async function pickAccent(color: string) {
    app.theme.accent = color;
    await saveTheme();
  }

  async function onRadius(e: Event) {
    app.theme.radius = Number((e.target as HTMLInputElement).value);
    await saveTheme();
  }

  async function onFontSize(e: Event) {
    app.theme.font_size = Number((e.target as HTMLInputElement).value);
    await saveTheme();
  }

  function close() {
    app.settingsOpen = false;
  }
</script>

{#if app.settingsOpen}
  <div class="modal-backdrop">
    <div class="modal settings">
      <header>设置 <button class="btn ghost sm" onclick={close}>✕</button></header>
      <div class="tabs">
        <button class:active={tab === "theme"} onclick={() => (tab = "theme")}>外观主题</button>
        <button class:active={tab === "agents"} onclick={() => { tab = "agents"; void loadRegistry(); }}>Agent 管理</button>
        <button class:active={tab === "paths"} onclick={() => (tab = "paths")}>环境与路径</button>
      </div>

      <div class="body">
        {#if tab === "theme"}
          <div class="sec">
            <h3>主题模式</h3>
            <div class="presets">
              <button class="preset" class:on={app.theme.preset === "dark"} onclick={() => pickThemePreset("dark")}>
                <span class="chip dark-chip"></span>深色
              </button>
              <button class="preset" class:on={app.theme.preset === "light"} onclick={() => pickThemePreset("light")}>
                <span class="chip light-chip"></span>浅色
              </button>
            </div>
          </div>
          <div class="sec">
            <h3>强调色</h3>
            <div class="accents">
              {#each ACCENTS as a (a.value)}
                <button
                  class="accent"
                  class:on={app.theme.accent === a.value}
                  style="background:{a.value}"
                  title={a.name}
                  onclick={() => pickAccent(a.value)}
                >
                  {#if app.theme.accent === a.value}✓{/if}
                </button>
              {/each}
              <input type="color" bind:value={app.theme.accent} onchange={() => saveTheme()} title="自定义颜色" />
            </div>
          </div>
          <div class="sec">
            <h3>圆角 {app.theme.radius}px</h3>
            <input type="range" min="0" max="18" value={app.theme.radius} oninput={onRadius} />
            <h3>字体大小 {app.theme.font_size}px</h3>
            <input type="range" min="12" max="18" value={app.theme.font_size} oninput={onFontSize} />
          </div>
        {:else if tab === "agents"}
          <AgentsAdmin bind:registry loadRegistry={loadRegistry} />
        {:else}
          <div class="sec">
            <h3>服务与解释器（保存后部分需重启使驾生效）</h3>
            <div class="field"><label>MCP 服务端口（共享上下文 /skills 与 /mcp）</label><input bind:value={mcpPort} placeholder="8345" /></div>
            <div class="field"><label>Python 解释器路径（工作流 python 节点使用，留空自动检测）</label><input bind:value={pythonPath} placeholder="自动检测" /></div>
          </div>
          <div class="sec">
            <h3>Node 运行时（适配器安装与启动使用）</h3>
            <div class="field"><label>Node 路径（发布包内置 node22，留空即用内置）</label><input bind:value={nodePath} placeholder="内置" /></div>
            <p class="note">发布结构：shidrive.exe 旁的 .tools\node22（含 npm）。也可指向系统安装的 Node ≥ 22。</p>
          </div>
          <div class="sec">
            <h3>网络代理（仅使驾自身依赖安装使用）</h3>
            <div class="field"><label>HTTP 代理（如 http://127.0.0.1:10809）</label><input bind:value={proxy} placeholder="留空=直连" /></div>
            <p class="note">只影响「安装适配器」等使驾自身的下载；工作流、ACP 会话、MCP 均不走此代理。</p>
          </div>
          <div class="rowbtns">
            <button class="btn sm primary" onclick={() => savePaths()}>保存路径与端口</button>
            <button class="btn sm" onclick={() => savePaths(true)}>清除自定义（恢复内置 / 自动检测）</button>
          </div>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  /* 固定尺寸：切 tab 时对话框不跳变 */
  .modal.settings {
    width: 700px;
    max-width: 92vw;
    height: min(620px, 86vh);
  }
  .tabs {
    display: flex;
    gap: 4px;
    padding: 8px 18px 0;
    border-bottom: 1px solid var(--border-soft);
  }
  .tabs button {
    padding: 8px 14px;
    color: var(--text-dim);
    border-bottom: 2px solid transparent;
  }
  .tabs button.active {
    color: var(--text);
    border-bottom-color: var(--accent);
    font-weight: 600;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 18px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .sec h3 {
    font-size: 0.9em;
    color: var(--text-dim);
    margin-bottom: 8px;
    margin-top: 10px;
  }
  .sec h3:first-child {
    margin-top: 0;
  }
  .sec .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 0.88em;
    margin-bottom: 8px;
  }
  .sec .field label {
    color: var(--text-dim);
  }
  .note {
    color: var(--text-faint);
    font-size: 0.8em;
    line-height: 1.5;
    user-select: text;
  }
  .presets {
    display: flex;
    gap: 10px;
  }
  .preset {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 18px;
    border: 2px solid var(--border);
    border-radius: var(--radius);
  }
  .preset.on {
    border-color: var(--accent);
  }
  .chip {
    width: 22px;
    height: 22px;
    border-radius: 6px;
    border: 1px solid var(--border);
  }
  .dark-chip {
    background: #141722;
  }
  .light-chip {
    background: #f2f4f9;
  }
  .accents {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .accent {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    color: #fff;
    border: 2px solid transparent;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 13px;
  }
  .accent.on {
    border-color: var(--text);
  }
  input[type="color"] {
    width: 30px;
    height: 30px;
    padding: 2px;
    border-radius: 50%;
  }
  input[type="range"] {
    width: 100%;
    accent-color: var(--accent);
  }
  .rowbtns {
    display: flex;
    gap: 8px;
  }
</style>
