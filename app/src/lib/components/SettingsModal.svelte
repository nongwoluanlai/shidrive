<script lang="ts">
  import { app, toast, saveTheme } from "../state.svelte";
  import { api } from "../ipc";
  import { ACCENTS } from "../theme";
  import { isEnabled as autoStartEnabled, enable as autoStartEnable, disable as autoStartDisable } from "@tauri-apps/plugin-autostart";
  import AgentsAdmin from "./AgentsAdmin.svelte";
  import type { AgentEnvStatusItem, NodeStat } from "../types";

  let tab = $state<"agents" | "theme" | "paths">("agents");
  let registry = $state<AgentEnvStatusItem[]>([]);

  // --- paths / ports editing state ---
  let mcpPort = $state("");
  let pythonPath = $state("");
  let nodePath = $state("");
  let proxy = $state("");
  let vscodePath = $state("");
  let pathsLoaded = $state(false);
  let nodeStat = $state<NodeStat | null>(null);
  let nodeBusy = $state(false);
  let autoStart = $state(false);

  async function loadNodeStatus() {
    nodeStat = await api.nodeStatus().catch(() => null);
  }

  async function loadAutoStart() {
    autoStart = await autoStartEnabled().catch(() => false);
  }

  async function exportData() {
    try {
      const path = await api.dataExport();
      toast("ok", `已导出到 ${path}`);
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function importData() {
    const path = await import("../dialog.svelte").then((m) => m.promptDialog({ title: "导入数据", label: "备份 JSON 路径", initial: "" }));
    if (path === null || !path.trim()) return;
    try {
      const msg = await api.dataImport(path.trim());
      toast("ok", msg + "（部分界面刷新或重启后完全生效）");
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function toggleEnterSend(e: Event) {
    const on = (e.target as HTMLInputElement).checked;
    app.enterSend = on;
    await api.settingsSet("chat.enter_send", on ? "1" : "0").catch(() => {});
    toast("ok", on ? "已切换：Enter 发送，Shift+Enter 换行" : "已切换：Enter 换行，Ctrl+Enter 发送");
  }

  async function toggleAutoStart(v: boolean) {
    try {
      if (v) await autoStartEnable();
      else await autoStartDisable();
      autoStart = v;
      toast("ok", v ? "已开启开机自动启动" : "已关闭开机自动启动");
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function downloadNode() {
    nodeBusy = true;
    toast("info", "正在下载 node22 便携版（Windows x64）…");
    try {
      const msg = await api.nodeDownload();
      toast("ok", msg);
      await loadNodeStatus();
    } catch (e) {
      toast("error", String(e));
    } finally {
      nodeBusy = false;
    }
  }

  async function loadRegistry() {
    registry = await api.agentsRegistry().catch(() => []);
  }

  async function loadPathSettings() {
    mcpPort = String((await api.settingsGet("mcp.port").catch(() => null)) ?? "");
    pythonPath = (await api.settingsGet("tools.python").catch(() => null)) ?? "";
    nodePath = (await api.settingsGet("tools.node").catch(() => null)) ?? "";
    proxy = (await api.settingsGet("network.proxy").catch(() => null)) ?? "";
    vscodePath = (await api.settingsGet("tools.vscode").catch(() => null)) ?? "";
    void loadNodeStatus();
    void loadAutoStart();
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
            ["tools.vscode", ""],
          ]
        : [
            ["mcp.port", mcpPort.trim()],
            ["tools.python", pythonPath.trim()],
            ["tools.node", nodePath.trim()],
            ["network.proxy", proxy.trim()],
            ["tools.vscode", vscodePath.trim()],
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
        <button class:active={tab === "agents"} onclick={() => { tab = "agents"; void loadRegistry(); }}>Agent 管理</button>
        <button class:active={tab === "paths"} onclick={() => (tab = "paths")}>环境与路径</button>
        <button class:active={tab === "theme"} onclick={() => (tab = "theme")}>外观主题</button>
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
            <h3>通用</h3>
            <label class="check"><input type="checkbox" checked={autoStart} onchange={(e) => toggleAutoStart((e.target as HTMLInputElement).checked)} /> 开机自动启动使驾（最小化到托盘运行）</label>
            <label class="check"><input type="checkbox" checked={app.enterSend} onchange={toggleEnterSend} /> Enter 发送消息（默认关闭：Enter 换行，Ctrl+Enter 发送）</label>
            <div class="rowbtns">
              <button class="btn sm" onclick={exportData}>导出数据（项目/上下文/工作流 → 桌面）</button>
              <button class="btn sm" onclick={importData}>导入数据（备份 JSON）</button>
            </div>
          </div>
          <div class="sec">
            <h3>服务与解释器（保存后部分需重启使驾生效）</h3>
            <div class="field"><label>MCP 服务端口（共享上下文 /skills 与 /mcp）</label><input bind:value={mcpPort} placeholder="8345" /></div>
            <div class="field"><label>Python 解释器路径（工作流 python 节点使用，留空自动检测）</label><input bind:value={pythonPath} placeholder="自动检测" /></div>
            <div class="field"><label>VS Code 路径（code.cmd / code，目录树「在 VS Code 中打开」使用，留空自动检测）</label><input bind:value={vscodePath} placeholder="自动检测" /></div>
          </div>
          <div class="sec">
            <h3>Node 运行时（适配器安装与启动使用，需 ≥ 22）</h3>
            <div class="field"><label>Node 路径（node.exe，留空自动检测）</label>
              <input bind:value={nodePath} placeholder={nodeStat?.ok ? `已自动使用 ${nodeStat.version}（${nodeStat.source}）` : "请指定 Node 路径或点击下载"} />
            </div>
            {#if nodeStat && !nodeStat.ok}
              <p class="note warn">{nodeStat.source === "未找到" ? "未检测到可用的 Node（≥ 22）" : `检测到 ${nodeStat.version || "不可用的 Node"}（${nodeStat.source}），版本过低或不可用`}</p>
            {/if}
            <div class="rowbtns">
              <button class="btn sm" disabled={nodeBusy} onclick={downloadNode}>{nodeBusy ? "下载中…" : "下载 node22（Windows x64）"}</button>
            </div>
            <p class="note">下载解压到用户数据目录 tools\node22，不影响系统 Node 环境；GitHub 较慢可先在下方填写代理。也可自行指定系统安装的 Node ≥ 22 路径。</p>
          </div>
          <div class="sec">
            <h3>网络代理（仅使驾自身依赖安装使用）</h3>
            <div class="field"><label>HTTP 代理（如 http://127.0.0.1:10809）</label><input bind:value={proxy} placeholder="留空=直连" /></div>
            <p class="note">只影响「安装适配器」与 node22 下载；工作流、ACP 会话、MCP 均不走此代理。</p>
          </div>
          <div class="rowbtns">
            <button class="btn sm primary" onclick={() => savePaths()}>保存路径与端口</button>
            <button class="btn sm" onclick={() => savePaths(true)}>清除自定义（恢复自动检测）</button>
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
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.9em;
    color: var(--text-dim);
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
  }
  .note.warn {
    color: var(--danger);
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
