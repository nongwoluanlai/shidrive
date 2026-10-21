<script lang="ts">
  import { t } from "../i18n";
  // Agent 管理设置页：折叠行/开关/探测状态/?指引/优先级/手动配置/安装适配器
  // 注意：草稿初始化只能在 $effect 或事件回调里写状态——渲染期写状态会触发
  // Svelte 的 state_unsafe_mutation，导致展开/上移下移整块渲染中断失效。
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import type { AgentEnvStatusItem, ShidriveAgentConfig, ShidriveAgentConfigView } from "../types";

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
    if (on && r.id === "shidrive" && !r.adapter_ready) {
      toast("info", t("已启用 ShiDrive：请填写接口地址、模型与 API Key 后保存"));
      if (expanded !== r.id) toggleExpand(r);
    } else if (on && r.npm) {
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

  /** DeepSeek 按当前方式安装对应包；先落盘方式，保证后端安装的就是界面所选。 */
  async function pkgFor(r: AgentEnvStatusItem): Promise<string | null> {
    if (r.id !== "deepseek") return r.npm;
    const mode = r.deepseek?.mode ?? "acp";
    await api.deepseekSetSource(mode);
    return DS_PKG[mode] ?? null;
  }

  async function install(r: AgentEnvStatusItem) {
    if (!r.npm) return;
    busy = r.id;
    try {
      const pkg = await pkgFor(r);
      if (!pkg) return;
      toast("info", t("正在安装 {p0}…（首次可能需要几分钟）", { p0: pkg }));
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

  async function piLogin() {
    try {
      await api.acpPiLogin();
      toast("info", t("已打开 pi 终端：执行 /login 或配置 API Key，完成后退出并重新连接 Pi"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function uninstall(r: AgentEnvStatusItem) {
    if (!r.npm) return;
    busy = r.id;
    try {
      const pkg = await pkgFor(r);
      if (!pkg) return;
      toast("info", t("正在卸载 {p0}…", { p0: pkg }));
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

  // ---- 内置 ShiDrive Agent：OpenAI 兼容接口配置 ----
  let sdView = $state<ShidriveAgentConfigView | null>(null);
  let sdDraft = $state<{ base_url: string; model: string; models: string; max_steps: string }>({ base_url: "", model: "", models: "", max_steps: "" });
  let sdKey = $state("");
  let sdShowKey = $state(false);
  let sdModels = $state<string[]>([]);
  let sdBusy = $state<"" | "models" | "save">("");

  $effect(() => {
    if (expanded !== "shidrive" || sdView) return;
    void api
      .shidriveAgentConfigGet()
      .then((v) => {
        sdView = v;
        sdDraft = { base_url: v.base_url, model: v.model, models: v.models, max_steps: v.max_steps ? String(v.max_steps) : "" };
      })
      .catch((e) => toast("error", String(e)));
  });

  async function sdFetchModels() {
    if (!sdDraft.base_url.trim()) return toast("info", t("请先填写接口地址"));
    sdBusy = "models";
    try {
      const list = await api.shidriveAgentModels(sdDraft.base_url.trim(), sdKey.trim() || undefined);
      sdModels = list;
      toast("ok", list.length ? t("连接成功：{n} 个模型", { n: list.length }) : t("连接成功，但接口未返回模型列表，请手动填写模型名"));
      if (!sdDraft.model && list.length) sdDraft.model = list[0];
    } catch (e) {
      toast("error", String(e));
    } finally {
      sdBusy = "";
    }
  }

  async function sdSave(clearKey = false) {
    const steps = sdDraft.max_steps.trim() ? Number(sdDraft.max_steps.trim()) : null;
    if (steps !== null && (!Number.isInteger(steps) || steps < 1 || steps > 500)) return toast("error", t("最大步数需为 1 ~ 500 的整数"));
    sdBusy = "save";
    try {
      const config: ShidriveAgentConfig = {
        base_url: sdDraft.base_url.trim(),
        model: sdDraft.model.trim(),
        models: sdDraft.models,
        max_steps: steps,
        temperature: sdView?.temperature ?? null,
      };
      await api.shidriveAgentConfigSet(config, clearKey ? "" : sdKey.trim() || null);
      sdKey = "";
      sdView = await api.shidriveAgentConfigGet();
      sdDraft = { base_url: sdView.base_url, model: sdView.model, models: sdView.models, max_steps: sdView.max_steps ? String(sdView.max_steps) : "" };
      toast("ok", sdView.configured ? t("已保存，ShiDrive Agent 可以使用了") : t("已保存（还需填写接口地址与模型）"));
      await loadRegistry();
    } catch (e) {
      toast("error", String(e));
    } finally {
      sdBusy = "";
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
        d.command.trim() || hasEnv || d.args.trim()
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
  };

  // ── DeepSeek dsh 来源：检测 / 手动指定 / 独立包互相独立 ──
  let dsBusy = $state(false);

  const DS_MODES = [
    { id: "acp", name: "三方 ACP", sub: "推荐 · 支持历史与标题" },
    { id: "dsh", name: "本地 dsh.cmd", sub: "Desktop 自带 · 官方 ACP" },
    { id: "harness", name: "官方完整包", sub: "npm 约 520MB · 官方 ACP" },
  ] as const;
  const DS_PKG: Record<string, string> = { acp: "@openma/deepseek-harness-acp", harness: "@deepseek-ai/dsh" };

  function dsKindLabel(kind?: string): string {
    switch (kind) {
      case "acp": case "npm": return t("已安装");
      case "manual": return t("手动指定");
      case "desktop": return t("自动检测");
      case "invalid": return t("路径无效");
      default: return t("未就绪");
    }
  }

  async function dsSetMode(r: AgentEnvStatusItem, mode: string) {
    if (r.deepseek?.mode === mode) return;
    dsBusy = true;
    try {
      await api.deepseekSetSource(mode);
      await loadRegistry();
    } catch (e) {
      toast("error", String(e));
    } finally {
      dsBusy = false;
    }
  }

  function setEnvLine(r: AgentEnvStatusItem, key: string, value: string | null) {
    const d = draftOf(r);
    const lines = d.env.split("\n").filter((l) => l.trim() && !l.startsWith(key + "="));
    if (value !== null) lines.push(`${key}=${value}`);
    drafts[r.id] = { ...d, env: lines.join("\n") };
  }

  /** 草稿可能尚未从后端载入（刚展开）；写入前先拉一次完整配置，避免覆盖已有 API Key 等。 */
  async function ensureDraft(r: AgentEnvStatusItem) {
    if (drafts[r.id]) return;
    const l = await api.agentConfigGet(r.id).catch(() => null);
    drafts[r.id] = {
      command: l?.command ?? r.manual_command ?? "",
      args: (l?.args ?? []).join("\n"),
      env: Object.entries(l?.env ?? {}).map(([k, v]) => `${k}=${v}`).join("\n"),
    };
  }

  async function dsApplyPath(r: AgentEnvStatusItem, input: string) {
    dsBusy = true;
    try {
      const dsh = await api.deepseekResolveDsh(input);
      await ensureDraft(r);
      setEnvLine(r, "DSH_CMD", dsh);
      await saveConfig(r);
      toast("ok", t("已指定 dsh：{path}", { path: dsh }));
    } catch (e) {
      toast("error", String(e));
    } finally {
      dsBusy = false;
    }
  }

  async function dsPickDir(r: AgentEnvStatusItem) {
    const ds = r.deepseek;
    const initial = ds?.manual ?? ds?.desktop ?? "";
    let dir: string | null;
    try {
      dir = await api.pickFolder(t("选择 DeepSeek Harness 安装目录"), initial);
    } catch {
      // 原生选择器不可用时退回手动输入
      return dsTypePath(r);
    }
    if (dir) await dsApplyPath(r, dir);
  }

  async function dsTypePath(r: AgentEnvStatusItem) {
    const { promptDialog } = await import("../dialog.svelte");
    const v = await promptDialog({
      title: t("DeepSeek Harness 安装目录"),
      label: t("安装目录或 dsh.cmd 的完整路径"),
      initial: r.deepseek?.manual ?? r.deepseek?.desktop ?? "",
    });
    if (v && v.trim()) await dsApplyPath(r, v.trim());
  }

  async function dsClear(r: AgentEnvStatusItem) {
    await ensureDraft(r);
    setEnvLine(r, "DSH_CMD", null);
    await saveConfig(r);
  }

  async function dsRedetect(r: AgentEnvStatusItem) {
    dsBusy = true;
    let found: string | null = null;
    try {
      found = await api.deepseekRedetect();
      await loadRegistry();
    } catch (e) {
      toast("error", String(e));
      return;
    } finally {
      dsBusy = false;
    }
    if (found) {
      toast("ok", t("已检测到 DeepSeek Harness Desktop：{path}", { path: found }));
      return;
    }
    // 自动检测失败 → 引导用户手动选择目录
    const { confirmDialog } = await import("../dialog.svelte");
    const pick = await confirmDialog({
      title: t("未检测到 DeepSeek Harness Desktop"),
      message: t("未在常见安装目录与注册表中找到。可手动选择 DeepSeek Harness 安装目录或 dsh.cmd。"),
      confirmText: t("选择路径…"),
    });
    if (pick) await dsPickDir(r);
  }

  async function fillDeepseekApiKey(r: AgentEnvStatusItem) {
    const NL = "\n";
    const initial = (() => {
      const line = draftOf(r).env.split(NL).find((l) => l.startsWith("DEEPSEEK_API_KEY="));
      return line ? line.slice("DEEPSEEK_API_KEY=".length) : "";
    })();
    const key = await import("../dialog.svelte").then((m) =>
      m.promptDialog({ title: t("API Key"), label: t("DeepSeek API Key（platform.deepseek.com 获取）"), initial }),
    );
    if (key === null || !key.trim()) return;
    const d = draftOf(r);
    const lines = d.env.split(NL).filter((l) => l.trim() && !l.startsWith("DEEPSEEK_API_KEY="));
    lines.push(`DEEPSEEK_API_KEY=${key.trim()}`);
    drafts[r.id] = { ...d, env: lines.join(NL) };
    toast("ok", t("已填充 {key}，确认后点「保存配置」", { key: "DEEPSEEK_API_KEY" }));
  }

  async function fillFromInstallDir(r: AgentEnvStatusItem) {
    const target = DIR_ENV_TARGETS[r.id];
    if (!target) { toast("info", t("该 Agent 无需安装目录")); return; }
    const NL = "\n";
    const initial = (() => {
      const line = draftOf(r).env.split(NL).find((l) => l.startsWith(target.env + "="));
      if (line) return line.slice(target.env.length + 1).replace(new RegExp("\\\\+(resources\\\\glm)?\\\\zcode\\.cjs$", "i"), "").replace(/\\codex\.exe$/i, "");
      return (r.auto_env?.[target.env] ?? "").replace(/\\(resources\\glm\\)?zcode\.cjs$/i, "").replace(/\\codex\.exe$/i, "");
    })();
    let dir: string | null = null;
    let native = false;
    if (!target.api) {
      try {
        dir = await api.pickFolder(t("{name} 的安装目录", { name: r.name }), initial);
        native = true;
      } catch {
        native = false;
      }
    }
    if (!native) dir = await import("../dialog.svelte").then((m) =>
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
  <p class="hint">{t("内置 ShiDrive 无需安装，展开填写接口即可使用。启用后会话页顶部出现对应 Agent 标签；顺序即标签顺序（用 ↑↓ 调整）。npm 类工具：展开该行点「安装适配器」自动下载到用户数据目录（DeepSeek 会安装运行必需的原生依赖，可配代理）；二进制类（Cursor/OpenCode）手动填命令路径。")}</p>
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
            {#if r.id === "shidrive"}
              <span class="badge {r.adapter_ready ? 'ok' : 'warn'}">{r.adapter_ready ? t("已配置") : t("待配置接口")}</span>
            {:else if !r.npm}
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
            {#if r.id === "shidrive"}
              <p class="help-text">{t(r.help)}</p>
              {#if !sdView}
                <p class="pend">{t("加载中…")}</p>
              {:else}
                <div class="field"><label for="sd-base">{t("接口地址（OpenAI 兼容，如 https://api.openai.com/v1）")}</label>
                  <input id="sd-base" class="mono" bind:value={sdDraft.base_url} placeholder="https://…/v1" />
                </div>
                <div class="field"><label for="sd-key">{t("API Key")}</label>
                  <div class="cfg-line">
                    <input id="sd-key" class="mono grow" type={sdShowKey ? "text" : "password"} autocomplete="new-password" placeholder={sdView.api_key_set ? t("已保存（留空不修改）") : "sk-…"} bind:value={sdKey} />
                    <button class="btn sm" onclick={() => (sdShowKey = !sdShowKey)}>{sdShowKey ? t("隐藏") : t("显示")}</button>
                    {#if sdView.api_key_set}<button class="btn sm" disabled={!!sdBusy} onclick={() => sdSave(true)}>{t("清除")}</button>{/if}
                  </div>
                </div>
                <div class="field"><label for="sd-model">{t("模型")}</label>
                  <div class="cfg-line">
                    <input id="sd-model" class="mono grow" list="sd-models" bind:value={sdDraft.model} placeholder="gpt-4.1 / deepseek-chat / qwen-…" />
                    <datalist id="sd-models">{#each sdModels as m (m)}<option value={m}></option>{/each}</datalist>
                    <button class="btn sm" disabled={!!sdBusy} onclick={sdFetchModels}>{sdBusy === "models" ? t("连接中…") : t("获取模型 / 测试连接")}</button>
                  </div>
                </div>
                <div class="field"><label for="sd-models-extra">{t("可切换的其他模型（逗号分隔，可选；会话中可在模型下拉里切换）")}</label>
                  <input id="sd-models-extra" class="mono" bind:value={sdDraft.models} />
                </div>
                <div class="field"><label for="sd-steps">{t("单轮最大工具步数（可选，默认 60）")}</label>
                  <input id="sd-steps" class="mono" inputmode="numeric" bind:value={sdDraft.max_steps} placeholder="60" />
                </div>
                <div class="rowbtns">
                  <button class="btn sm primary" disabled={!!sdBusy} onclick={() => sdSave()}>{sdBusy === "save" ? t("保存中…") : t("保存")}</button>
                  <span class="pend">{t("保存后立即生效（会重置当前连接）。模式：询问（改文件 / 执行命令前确认）、自动、只读，可在会话页切换。")}</span>
                </div>
              {/if}
            {:else}
{#if r.id !== "deepseek"}<p class="help-text">{t(r.help)}</p>{/if}
            {#if r.id === "deepseek"}
              {@const ds = r.deepseek}
              {@const mode = ds?.mode ?? "acp"}
              <div class="ds-box">
                <div class="ds-modes" role="radiogroup">
                  {#each DS_MODES as m (m.id)}
                    <button class="ds-mode" class:active={mode === m.id} role="radio" aria-checked={mode === m.id}
                      disabled={dsBusy || busy === r.id} onclick={() => dsSetMode(r, m.id)}>
                      <span class="ds-mode-name">{t(m.name)}</span>
                      <span class="ds-mode-sub">{t(m.sub)}</span>
                    </button>
                  {/each}
                </div>
                <div class="ds-detail">
                  <span class="badge {ds && ['acp', 'manual', 'desktop', 'npm'].includes(ds.kind) ? 'ok' : 'warn'}">{dsKindLabel(ds?.kind)}</span>
                  {#if ds?.path}<code class="ds-path" title={ds.path}>{ds.path}</code>{/if}
                  <span class="spacer"></span>
                  {#if mode === "dsh"}
                    <button class="btn sm" disabled={dsBusy} onclick={() => dsRedetect(r)}>{dsBusy ? t("检测中…") : t("检测")}</button>
                    <button class="btn sm" disabled={dsBusy} onclick={() => dsPickDir(r)}>{t("选择路径…")}</button>
                    {#if ds?.manual}<button class="btn sm" disabled={dsBusy} onclick={() => dsClear(r)}>{t("清除")}</button>{/if}
                  {:else if ds?.kind === "acp" || ds?.kind === "npm"}
                    <button class="btn sm" disabled={busy === r.id} onclick={() => install(r)}>{busy === r.id ? t("处理中…") : t("更新")}</button>
                    <button class="btn sm" disabled={busy === r.id} onclick={() => uninstall(r)}>{t("卸载")}</button>
                  {:else}
                    <button class="btn sm primary" disabled={busy === r.id} onclick={() => install(r)}>{busy === r.id ? t("安装中…") : t("安装")}</button>
                  {/if}
                </div>
                <div class="cfg-line">
                  <button class="btn sm" onclick={() => void fillDeepseekApiKey(r)}>{t("填充 API Key")}</button>
                  {#if r.auto_env?.DEEPSEEK_API_KEY === "set"}<span class="badge ok">{t("已检测到 {key}（系统环境）", { key: "DEEPSEEK_API_KEY" })}</span>{/if}
                </div>
              </div>
            {:else if r.npm && !r.adapter_ready}
              <div class="cfg-line">
                <button class="btn sm primary" disabled={busy === r.id} onclick={() => install(r)}>
                  {busy === r.id ? t("安装中…") : t("安装适配器")}
                </button>
                <span class="pend">{t("安装后即可连接（npm：{package}）", { package: r.npm })}</span>
              </div>
            {:else if r.npm && r.adapter_ready}
              <div class="cfg-line">
                <button class="btn sm" disabled={busy === r.id} onclick={() => uninstall(r)}>
                  {busy === r.id ? t("卸载中…") : t("卸载适配器")}
                </button>
                <span class="pend">{t("卸载后需重新安装才能连接")}</span>
              </div>
            {/if}
            {#if r.id === "pi" && r.adapter_ready}
              <div class="cfg-line">
                <button class="btn sm" onclick={piLogin}>{t("登录 / 配置 pi")}</button>
                <span class="pend">{t("在新终端运行 pi（/login 或配置模型提供商），配置与终端 pi 共用 ~/.pi/agent")}</span>
              </div>
            {/if}
            {#if DIR_ENV_TARGETS[r.id]}
              <div class="cfg-line">
                <button class="btn sm" onclick={() => fillFromInstallDir(r)}>{t("从安装目录填充")}</button>
                <span class="pend">{t("输入软件安装目录，自动推导环境变量")}</span>
              </div>
            {/if}
            <div class="field"><label>{t("命令（{hint}）", { hint: r.npm ? t("覆盖自动检测，留空=自动") : t("必填：可执行文件完整路径") })}</label>
              <input value={draftOf(r).command} oninput={(e) => (drafts[r.id] = { ...draftOf(r), command: (e.target as HTMLInputElement).value })} placeholder={r.npm ? t("留空自动") : r.id === "cursor" ? "…\\dist-package\\cursor-agent.cmd" : "…\\opencode.exe"} />
            </div>
            {#if !r.npm}
              <p class="help-text">{t("参数留空时自动使用 acp；填写参数时按完整参数执行。")}</p>
            {/if}
            <div class="field"><label>{t("参数（每行一个，一般留空）")}</label><textarea rows="2" class="mono" value={draftOf(r).args} oninput={(e) => (drafts[r.id] = { ...draftOf(r), args: (e.target as HTMLTextAreaElement).value })}></textarea></div>
            <div class="field"><label>{t("环境变量（每行 KEY=VALUE）")}</label><textarea rows="2" class="mono" value={draftOf(r).env} oninput={(e) => (drafts[r.id] = { ...draftOf(r), env: (e.target as HTMLTextAreaElement).value })}></textarea></div>
            <div class="rowbtns">
              <button class="btn sm primary" onclick={() => saveConfig(r)}>{t("保存配置")}</button>
              <button class="btn sm" onclick={() => clearConfig(r)}>{t("清除自定义")}</button>
            </div>
            {/if}
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
  .ds-box {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px;
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    background: color-mix(in srgb, var(--bg-elev) 70%, transparent);
  }
  .ds-modes {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 6px;
  }
  .ds-mode {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 6px 8px;
    border: 1px solid var(--border-soft);
    border-radius: 6px;
    background: transparent;
    color: inherit;
    cursor: pointer;
    text-align: left;
  }
  .ds-mode.active {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 9%, transparent);
  }
  .ds-mode-name {
    font-weight: 600;
    font-size: 0.86em;
  }
  .ds-mode-sub {
    font-size: 0.74em;
    color: var(--text-faint);
  }
  .ds-detail {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    font-size: 0.86em;
  }
  .ds-path {
    font-family: var(--mono);
    font-size: 0.86em;
    color: var(--text-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    user-select: text;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .rowbtns {
    display: flex;
    gap: 8px;
  }
</style>
