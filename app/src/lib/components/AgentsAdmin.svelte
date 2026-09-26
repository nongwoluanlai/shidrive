<script lang="ts">
  import { t } from "../i18n";
  // Agent 管理设置页：折叠行/开关/探测状态/?指引/优先级/手动配置/安装适配器
  // 注意：草稿初始化只能在 $effect 或事件回调里写状态——渲染期写状态会触发
  // Svelte 的 state_unsafe_mutation，导致展开/上移下移整块渲染中断失效。
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import type { AgentEnvStatusItem } from "../types";

  let {
    registry = $bindable([] as AgentEnvStatusItem[]),
    loadRegistry,
  }: {
    registry: AgentEnvStatusItem[];
    loadRegistry: () => Promise<void>;
  } = $props();

  interface Draft {
    command: string;
    args: string;
    env: string;
  }
  let expanded = $state<string | null>(null);
  let busy = $state<string | null>(null);
  // 启用顺序（决定行的显示顺序：启用的在前按优先级排，停用的按注册表顺序跟在后面）
  let enabledOrder = $state<string[]>([]);
  // 手动配置草稿（id → {command,args,env}）
  let drafts = $state<Record<string, Draft>>({});

  const sortedRegistry = $derived.by(() => {
    const rank = new Map(enabledOrder.map((id, i) => [id, i]));
    return [...registry].sort((a, b) => {
      const ia = rank.get(a.id);
      const ib = rank.get(b.id);
      if (ia !== undefined && ib !== undefined) return ia - ib;
      if (ia !== undefined) return -1;
      if (ib !== undefined) return 1;
      return 0;
    });
  });

  // 渲染期只读；缺省草稿不写状态，交给下方 $effect 补齐
  function draftOf(r: AgentEnvStatusItem): Draft {
    return drafts[r.id] ?? { command: r.manual_command ?? "", args: "", env: "" };
  }

  // 挂载时拉启用顺序与注册表（默认 tab 即 Agent 管理时也必须加载）
  $effect(() => {
    void api
      .agentsEnabledGet()
      .then((e: string[]) => (enabledOrder = e))
      .catch(() => {});
    void loadRegistry();
  });

  // 展开行渲染完成后异步补全草稿（先取 override + 自动探测环境变量预填，
  // 再同步远端完整配置覆盖）
  $effect(() => {
    const id = expanded;
    if (!id || drafts[id]) return;
    const r = registry.find((x) => x.id === id);
    // 环境变量草稿预填自动探测值（ZCODE_BIN/ZCODE_NODE/CODEX_PATH…），可编辑后保存；
    // api 类（DEEPSEEK_API_KEY 等）只作检测展示，绝不预填
    const autoEnv = Object.entries(r?.auto_env ?? {})
      .filter(([k]) => !DIR_ENV_TARGETS[k]?.api)
      .map(([k, v]) => `${k}=${v}`)
      .join("\n");
    drafts[id] = { command: r?.manual_command ?? "", args: "", env: autoEnv };
    void api
      .agentConfigGet(id)
      .then((l) => {
        drafts[id] = {
          command: l.command ?? "",
          args: (l.args ?? []).join("\n"),
          env: l.env && Object.keys(l.env).length
            ? Object.entries(l.env ?? {}).map(([k, v]) => `${k}=${v}`).join("\n")
            : autoEnv,
        };
      })
      .catch(() => {});
  });

  function toggleExpand(r: AgentEnvStatusItem) {
    expanded = expanded === r.id ? null : r.id;
  }

  async function toggle(r: AgentEnvStatusItem, on: boolean) {
    const enabled: string[] = await api.agentsEnabledGet().catch((): string[] => []);
    let next: string[];
    if (on) {
      if (enabled.includes(r.id)) return;
      next = [...enabled, r.id];
    } else {
      next = enabled.filter((x) => x !== r.id);
    }
    await api.agentsEnabledSet(next);
    await refreshEnabled();
    if (on && r.npm) {
      const row = registry.find((x) => x.id === r.id);
      if (row && !row.adapter_ready) {
        toast("info", t("已启用 {p0}：适配器未安装，请展开该行点「安装适配器」（需联网，仅此一次）", { p0: r.name }));
        toggleExpand(r);
      } else {
        toast("ok", t("已启用 {p0}", { p0: r.name }));
      }
    } else {
      toast("ok", on ? t("已启用 {p0}（排在第 {p1} 位）", { p0: r.name, p1: next.length }) : t("已停用 {p0}", { p0: r.name }));
    }
  }

  async function refreshEnabled() {
    const enabled: string[] = await api.agentsEnabledGet().catch((): string[] => []);
    enabledOrder = enabled;
    const reg = await api.agentsRegistry().catch(() => []);
    registry = reg;
    app.agents = enabled
      .map((id) => reg.find((x) => x.id === id))
      .filter((x): x is AgentEnvStatusItem => !!x)
      .map((x) => ({ id: x.id, name: x.name }));
    if (app.agent && !app.agents.find((a) => a.id === app.agent)) {
      app.agent = app.agents[0]?.id ?? "";
    } else if (!app.agent && app.agents.length) {
      app.agent = app.agents[0].id;
    }
  }

  async function move(r: AgentEnvStatusItem, dir: -1 | 1) {
    const enabled: string[] = await api.agentsEnabledGet().catch((): string[] => []);
    const i = enabled.indexOf(r.id);
    const j = i + dir;
    if (i < 0) return;
    if (j < 0) return toast("info", t("{p0} 已在最前", { p0: r.name }));
    if (j >= enabled.length) return toast("info", t("{p0} 已在最后", { p0: r.name }));
    [enabled[i], enabled[j]] = [enabled[j], enabled[i]];
    await api.agentsEnabledSet(enabled);
    await refreshEnabled();
  }

  async function install(r: AgentEnvStatusItem) {
    if (!r.npm) return;
    busy = r.id;
    toast("info", t("正在安装 {p0}…（首次可能需要几分钟）", { p0: r.npm }));
    try {
      await api.agentsBootstrap(r.id);
      toast("ok", t("适配器已安装"));
      await loadRegistry();
      await refreshEnabled();
    } catch (e) {
      toast("error", String(e));
      // 修复失败会撤销 DeepSeek 就绪标记；立即刷新，勿继续显示旧的“就绪”。
      if (r.id === "deepseek") await loadRegistry();
    } finally {
      busy = null;
    }
  }

  async function uninstall(r: AgentEnvStatusItem) {
    if (!r.npm) return;
    busy = r.id;
    toast("info", t("正在卸载 {p0}…", { p0: r.npm }));
    try {
      await api.agentsUninstall(r.id);
      toast("ok", t("适配器已卸载"));
      await loadRegistry();
      await refreshEnabled();
    } catch (e) {
      toast("error", String(e));
    } finally {
      busy = null;
    }
  }

  async function saveConfig(r: AgentEnvStatusItem) {
    const d = draftOf(r);
    const env: Record<string, string> = {};
    for (const line of d.env.split("\n")) {
      const i = line.indexOf("=");
      if (i > 0) env[line.slice(0, i).trim()] = line.slice(i + 1).trim();
    }
    try {
      const hasEnv = Object.keys(env).length > 0;
      await api.agentConfigSet(
        r.id,
        d.command.trim() || hasEnv
          ? { command: d.command.trim(), args: d.args.split("\n").map((x) => x.trim()).filter(Boolean), env }
          : null,
      );
      toast("ok", t("已保存配置，连接已重置"));
      await loadRegistry();
    } catch (e) {
      toast("error", String(e));
    }
  }

  function clearConfig(r: AgentEnvStatusItem) {
    drafts[r.id] = { command: "", args: "", env: "" };
    void saveConfig(r);
  }

  // 安装目录 / 密钥 → 推导该 Agent 已知的环境变量，填充到草稿（可再手改）
  const DIR_ENV_TARGETS: Record<string, { env: string; rel: string; api?: boolean }> = {
    zcode: { env: "ZCODE_BIN", rel: "resources\\glm\\zcode.cjs" },
    codex: { env: "CODEX_PATH", rel: "codex.exe" },
    deepseek: { env: "DEEPSEEK_API_KEY", rel: "", api: true },
  };

  async function fillFromInstallDir(r: AgentEnvStatusItem) {
    const target = DIR_ENV_TARGETS[r.id];
    if (!target) { toast("info", t("该 Agent 无需安装目录")); return; }
    const NL = "\n";
    const initial = (() => {
      const line = draftOf(r).env.split(NL).find((l) => l.startsWith(target.env + "="));
      if (line) return line.slice(target.env.length + 1).replace(new RegExp("\\\\+(resources\\\\glm)?\\\\zcode\\.cjs$", "i"), "").replace(/\\codex\.exe$/i, "");
      return (r.auto_env?.[target.env] ?? "").replace(/\\(resources\\glm\\)?zcode\.cjs$/i, "").replace(/\\codex\.exe$/i, "");
    })();
    const dir = await import("../dialog.svelte").then((m) =>
      m.promptDialog({
        title: t(target.api ? "API Key" : "安装目录"),
        label: target.api ? t("填入 {name} 的 API Key", { name: r.name }) : t("{name} 的安装目录", { name: r.name }),
        initial,
      }),
    );
    if (dir === null || !dir.trim()) return;
    const clean = dir.trim().replace(/[\\/]+$/, "");
    const d = draftOf(r);
    const lines = d.env.split(NL).filter((l) => l.trim() && !l.startsWith(target.env + "="));
    const value = target.rel ? `${target.env}=${clean}\\${target.rel}` : `${target.env}=${clean}`;
    lines.push(value);
    drafts[r.id] = { ...d, env: lines.join(NL) };
    toast("ok", t("已填充 {key}，确认后点「保存配置」", { key: target.env }));
  }
</script>

<div class="agents-admin">
  <p class="hint">{t("默认全部停用。启用后会话页顶部出现对应 Agent 标签；顺序即标签顺序（用 ↑↓ 调整）。npm 类工具：展开该行点「安装适配器」自动下载到用户数据目录（DeepSeek 会安装运行必需的原生依赖，可配代理）；二进制类（Cursor/OpenCode）手动填命令路径。")}</p>
  <div class="rows">
    {#each sortedRegistry as r (r.id)}
      <div class="agent-row" class:open={expanded === r.id} class:on={r.enabled}>
        <div class="row-head">
          <input
            type="checkbox"
            class="toggle"
            checked={r.enabled}
            onchange={(e) => toggle(r, (e.target as HTMLInputElement).checked)}
            title={r.enabled ? t("停用") : t("启用")}
          />
          <span class="name">{r.name}</span>
          {#if r.enabled}
            {#if !r.npm}
              <span class="badge {r.manual_command ? 'ok' : 'warn'}">{r.manual_command ? t("已配置命令") : t("待配置命令")}</span>
            {:else if r.adapter_ready}
              <span class="badge ok">{t("适配器就绪")}</span>
            {:else}
              <span class="badge warn">{t("未安装")}</span>
            {/if}
            <span class="qid" title={t(r.help)}>?</span>
            <button class="mini" title={t("上移（提高优先级）")} onclick={() => move(r, -1)}>↑</button>
            <button class="mini" title={t("下移")} onclick={() => move(r, 1)}>↓</button>
          {:else}
            <span class="qid" title={t(r.help)}>?</span>
          {/if}
          <span class="spacer"></span>
          <button class="mini chev" title={expanded === r.id ? t("收起") : t("展开配置")} onclick={() => toggleExpand(r)}>{expanded === r.id ? "▾" : "▸"}</button>
        </div>
        {#if expanded === r.id}
          <div class="row-body">
<p class="help-text">{t(r.help)}</p>
            {#if r.npm && !r.adapter_ready}
              <div class="cfg-line">
                <button class="btn sm primary" disabled={busy === r.id} onclick={() => install(r)}>
                  {busy === r.id ? t("安装中…") : t(r.id === "deepseek" ? "安装/修复 DeepSeek 适配器" : "安装适配器")}
                </button>
                <span class="pend">{t("安装后即可连接（npm：{package}）", { package: r.npm })}</span>
              </div>
            {:else if r.npm && r.adapter_ready}
              <div class="cfg-line">
                <button class="btn sm" disabled={busy === r.id} onclick={() => uninstall(r)}>
                  {busy === r.id ? t("卸载中…") : t("卸载适配器")}
                </button>
                {#if r.id === "deepseek"}
                  <button class="btn sm" disabled={busy === r.id} onclick={() => install(r)}>
                    {busy === r.id ? t("修复中…") : t("检查并修复适配器")}
                  </button>
                {/if}
                <span class="pend">{t("卸载后需重新安装才能连接")}</span>
              </div>
            {/if}
            {#if DIR_ENV_TARGETS[r.id]}
              <div class="cfg-line">
                <button class="btn sm" onclick={() => fillFromInstallDir(r)}>
                  {DIR_ENV_TARGETS[r.id].api ? t("填充 API Key") : t("从安装目录填充")}
                </button>
                <span class="pend">{DIR_ENV_TARGETS[r.id].api ? t("填入后保存配置，连接时注入环境变量") : t("输入软件安装目录，自动推导环境变量")}</span>
                {#if DIR_ENV_TARGETS[r.id].api && r.auto_env?.[DIR_ENV_TARGETS[r.id].env] === "set"}
                  <span class="badge ok">{t("已检测到 {key}（系统环境）", { key: DIR_ENV_TARGETS[r.id].env })}</span>
                {/if}
              </div>
            {/if}
            <div class="field"><label>{t("命令（{hint}）", { hint: r.npm ? t("覆盖自动检测，留空=自动") : t("必填：可执行文件完整路径") })}</label>
              <input value={draftOf(r).command} oninput={(e) => (drafts[r.id] = { ...draftOf(r), command: (e.target as HTMLInputElement).value })} placeholder={r.npm ? t("留空自动") : r.id === "cursor" ? "…\\dist-package\\cursor-agent.cmd" : "…\\opencode.exe"} />
            </div>
            {#if !r.npm}
              <p class="help-text">{t("参数 acp 已自动附加，无需填写。")}</p>
            {/if}
            <div class="field"><label>{t("参数（每行一个，一般留空）")}</label><textarea rows="2" class="mono" value={draftOf(r).args} oninput={(e) => (drafts[r.id] = { ...draftOf(r), args: (e.target as HTMLTextAreaElement).value })}></textarea></div>
            <div class="field"><label>{t("环境变量（每行 KEY=VALUE）")}</label><textarea rows="2" class="mono" value={draftOf(r).env} oninput={(e) => (drafts[r.id] = { ...draftOf(r), env: (e.target as HTMLTextAreaElement).value })}></textarea></div>
            <div class="rowbtns">
              <button class="btn sm primary" onclick={() => saveConfig(r)}>{t("保存配置")}</button>
              <button class="btn sm" onclick={() => clearConfig(r)}>{t("清除自定义")}</button>
            </div>
          </div>
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .agents-admin {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .hint {
    color: var(--text-faint);
    font-size: 0.84em;
    line-height: 1.5;
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .agent-row {
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    background: var(--bg-elev);
  }
  .agent-row.on {
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border));
  }
  .row-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 10px;
  }
  .toggle {
    width: 15px;
    height: 15px;
    accent-color: var(--accent);
  }
  .name {
    font-weight: 600;
    min-width: 72px;
  }
  .badge {
    font-size: 0.74em;
  }
  .qid {
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: 1px solid var(--text-faint);
    color: var(--text-faint);
    font-size: 0.72em;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: help;
    flex: none;
  }
  .mini {
    color: var(--text-faint);
    font-size: 0.85em;
    padding: 0 4px;
  }
  .mini:hover {
    color: var(--accent);
  }
  .spacer {
    flex: 1;
  }
  .row-body {
    border-top: 1px solid var(--border-soft);
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .help-text {
    color: var(--text-dim);
    font-size: 0.84em;
    line-height: 1.55;
    user-select: text;
  }
  .help-text code {
    background: var(--code-bg);
    padding: 0 4px;
    border-radius: 4px;
  }
  .cfg-line {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .pend {
    color: var(--text-faint);
    font-size: 0.82em;
    user-select: text;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 0.86em;
  }
  .field label {
    color: var(--text-dim);
  }
  .mono {
    font-family: var(--mono);
    font-size: 0.9em;
  }
  .rowbtns {
    display: flex;
    gap: 8px;
  }
</style>
