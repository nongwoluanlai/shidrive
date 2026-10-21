<script lang="ts">
  // 设置 → 全局助手：大脑（ACP 客户端 + 模型 / 自定义 OpenAI 兼容接口）、人设、记忆与气泡
  import { onMount } from "svelte";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { t } from "../i18n";
  import { confirmDialog } from "../dialog.svelte";
  import { ga, persona, afterConfigSaved, resetSession, clearMessages } from "../assistant/store.svelte";
  import type { AssistantConfig, AssistantConfigView } from "../types";
  import VoiceSettings from "./VoiceSettings.svelte";
  import { tryCare } from "../assistant/care.svelte";
  import { inputEnabled } from "../assistant/voice.svelte";

  let view = $state<AssistantConfigView | null>(null);
  let draft = $state<AssistantConfig | null>(null);
  let apiKey = $state("");
  let showKey = $state(false);
  let saving = $state(false);
  let fetching = $state(false);
  /** 最近一次「获取模型列表」的结果（ACP：能力；API：模型 id） */
  let fetchedCaps = $state<Record<string, { models?: unknown; configOptions?: unknown }>>({});
  let apiModels = $state<string[]>([]);

  const dirty = $derived(!!view && !!draft && (JSON.stringify(draft) !== JSON.stringify(view.config) || apiKey.trim() !== ""));
  const current = $derived(persona());

  onMount(() => { void load(); });

  async function load() {
    try {
      view = await api.assistantConfigGet();
      draft = structuredClone($state.snapshot(view.config)) as AssistantConfig;
      if (!draft.agent_type && app.agents.length) draft.agent_type = app.agents[0].id;
    } catch (e) {
      toast("error", String(e));
    }
  }

  type Opt = { value: unknown; label: string };
  /** 从能力描述里取出模型候选：configOptions 的 model 选项（含分组）或 models.availableModels */
  function modelOptions(caps: { models?: any; configOptions?: any } | undefined): Opt[] {
    if (!caps) return [];
    const out: Opt[] = [];
    const cfg = Array.isArray(caps.configOptions) ? caps.configOptions.find((o: any) => o?.id === "model" || o?.category === "model") : null;
    const walk = (list: any[]) => {
      for (const o of list ?? []) {
        if (Array.isArray(o?.options)) walk(o.options);
        else if (o && o.value !== undefined) out.push({ value: o.value, label: String(o.name ?? o.value) });
      }
    };
    if (cfg) walk(cfg.options);
    if (!out.length && Array.isArray(caps.models?.availableModels)) {
      for (const m of caps.models.availableModels) if (m?.modelId) out.push({ value: m.modelId, label: String(m.name ?? m.modelId) });
    }
    return out;
  }
  const acpModels = $derived(draft ? modelOptions((fetchedCaps[draft.agent_type] ?? app.agentCaps[draft.agent_type]) as any) : []);
  const modelKey = (v: unknown) => (v === null || v === undefined ? "" : JSON.stringify(v));
  const modelMissing = $derived(!!draft && draft.model !== null && draft.model !== undefined && !acpModels.some((o) => modelKey(o.value) === modelKey(draft!.model)));

  async function fetchModels() {
    if (!draft) return;
    fetching = true;
    try {
      if (draft.provider === "acp") {
        const caps = await api.assistantModels("acp", { agentType: draft.agent_type });
        fetchedCaps = { ...fetchedCaps, [draft.agent_type]: caps };
        const n = modelOptions(caps as any).length;
        toast(n ? "ok" : "warn", n ? t("获取到 {n} 个模型", { n }) : t("该客户端没有提供可选模型，将使用其默认模型"));
      } else {
        const r = await api.assistantModels("api", { baseUrl: draft.api.base_url, apiKey: apiKey.trim() || undefined });
        apiModels = Array.isArray(r.models) ? (r.models as string[]) : [];
        toast(apiModels.length ? "ok" : "warn", apiModels.length ? t("连接成功，获取到 {n} 个模型", { n: apiModels.length }) : t("连接成功，但接口没有返回模型列表"));
      }
    } catch (e) {
      toast("error", String(e));
    } finally {
      fetching = false;
    }
  }

  async function save() {
    if (!draft) return;
    if (draft.enabled && draft.provider === "api" && (!draft.api.base_url.trim() || !draft.api.model.trim())) {
      toast("warn", t("请填写接口地址和模型名"));
      return;
    }
    if (draft.enabled && draft.provider === "acp" && !draft.agent_type) {
      toast("warn", t("请选择 ACP 客户端"));
      return;
    }
    saving = true;
    try {
      const v = await api.assistantConfigSet($state.snapshot(draft) as AssistantConfig, apiKey.trim() || null);
      view = v;
      draft = structuredClone($state.snapshot(v.config)) as AssistantConfig;
      apiKey = "";
      afterConfigSaved(v);
      toast("ok", t("全局助手设置已保存"));
    } catch (e) {
      toast("error", String(e));
    } finally {
      saving = false;
    }
  }

  async function clearKey() {
    if (!draft || !view?.api_key_set) return;
    const ok = await confirmDialog({ title: t("清除 API 密钥"), message: t("清除已保存的 API 密钥？"), danger: true, confirmText: t("清除") });
    if (!ok) return;
    try {
      view = await api.assistantConfigSet($state.snapshot(view.config) as AssistantConfig, "");
      afterConfigSaved(view);
      toast("ok", t("API 密钥已清除"));
    } catch (e) { toast("error", String(e)); }
  }

  async function newSession() {
    if (dirty) { toast("warn", t("请先保存设置")); return; }
    const ok = await confirmDialog({
      title: t("新建长期会话"),
      message: t("将为助手新开一个长期会话（对话记录和长期记忆会保留，新会话会带上记忆与最近对话回顾）。继续吗？"),
      confirmText: t("新建"),
    });
    if (!ok) return;
    await resetSession();
    view = ga.view;
  }

  async function clearHistory() {
    const ok = await confirmDialog({ title: t("清空对话记录"), message: t("清空助手的全部对话记录？长期记忆不受影响。"), danger: true, confirmText: t("清空") });
    if (!ok) return;
    try { await clearMessages(); toast("ok", t("对话记录已清空")); } catch (e) { toast("error", String(e)); }
  }

  let caring = $state(false);
  async function testCare() {
    if (dirty) { toast("warn", t("请先保存设置")); return; }
    if (!view?.config.enabled) { toast("warn", t("请先启用全局助手")); return; }
    caring = true;
    try {
      const r = await tryCare();
      if (r === "busy") toast("warn", t("助手正在忙，请稍后"));
      else if (r === "skipped") toast("info", t("她觉得现在不适合打扰你，这次没说话"));
      else if (r === "failed") toast("warn", t("这次没有生成关怀内容"));
      else toast("ok", t("她开口了：关掉设置就能看到气泡"));
    } finally {
      caring = false;
    }
  }

  const styleLabel = (s: string) => (s === "amadeus" ? t("终端风格气泡") : s === "generic" ? t("人物旁通用气泡") : t("左下角简易助手"));
</script>

{#if draft && view}
  <section class="ga-set">
    <div class="status-row">
      <span class="dot {draft.enabled ? 'on' : 'off'}"></span>
      <span>{t("全局助手")}：{view.config.enabled ? t("已启用") : t("未启用")}</span>
      {#if ga.busy}<span class="sep">·</span><span>{t("回复中")}</span>{/if}
      <span class="grow"></span>
      <label class="switch" title={t("启用后在左下角（或皮肤人物旁）出现助手，可随时聊天")}>
        <input type="checkbox" bind:checked={draft.enabled} />
        <span class="slider"></span>
      </label>
    </div>
    <p class="note">{t("一个独立于项目会话的长期助手：可闲聊、提问，会记住你的偏好。人设与说话风格跟随左下角的皮肤角色。点击人物打开输入框，双击 Ctrl 语音输入。")}</p>

    <div class="sec">
      <h3>{t("大脑")}</h3>
      <div class="row">
        <span class="lbl w">{t("来源")}</span>
        <label class="check"><input type="radio" value="acp" bind:group={draft.provider} /> {t("ACP 客户端（复用已安装的 Agent）")}</label>
        <label class="check"><input type="radio" value="api" bind:group={draft.provider} /> {t("自定义 API（OpenAI 兼容）")}</label>
      </div>
      {#if draft.provider === "acp"}
        <div class="row">
          <span class="lbl w">{t("客户端")}</span>
          <select bind:value={draft.agent_type} onchange={() => { if (draft) draft.model = null; }}>
            {#each app.agents as a (a.id)}<option value={a.id}>{a.name}</option>{/each}
            {#if draft.agent_type && !app.agents.some((a) => a.id === draft!.agent_type)}<option value={draft.agent_type}>{draft.agent_type}{t("（未启用）")}</option>{/if}
          </select>
        </div>
        <div class="row">
          <span class="lbl w">{t("模型")}</span>
          <select value={modelKey(draft.model)} onchange={(e) => { const k = (e.target as HTMLSelectElement).value; if (draft) draft.model = k ? JSON.parse(k) : null; }}>
            <option value="">{t("客户端默认")}</option>
            {#each acpModels as o (modelKey(o.value))}<option value={modelKey(o.value)}>{o.label}</option>{/each}
            {#if modelMissing}<option value={modelKey(draft.model)}>{String(draft.model)}</option>{/if}
          </select>
          <button class="btn ghost sm" disabled={fetching || !draft.agent_type || !!ga.busy} title={t("会启动该客户端并建立助手会话，读取它提供的模型")} onclick={fetchModels}>{fetching ? t("获取中…") : t("获取模型列表")}</button>
        </div>
        <div class="row">
          <span class="lbl w">{t("工作目录")}</span>
          <input class="grow" placeholder={view.default_workspace} bind:value={draft.workspace} />
        </div>
        <p class="note">{t("助手使用该客户端的一个独立长期会话（不绑定任何项目上下文）。工作目录留空则使用使驾数据目录下的 assistant 文件夹。")}</p>
      {:else}
        <div class="row">
          <span class="lbl w">{t("接口地址")}</span>
          <input class="grow" placeholder="https://api.openai.com/v1" bind:value={draft.api.base_url} />
        </div>
        <div class="row">
          <span class="lbl w">API Key</span>
          <input class="grow mono" type={showKey ? "text" : "password"} autocomplete="new-password" placeholder={view.api_key_set ? t("已保存（留空不修改）") : "sk-…"} bind:value={apiKey} />
          <button class="btn ghost sm" onclick={() => (showKey = !showKey)}>{showKey ? t("隐藏") : t("显示")}</button>
          {#if view.api_key_set}<button class="btn ghost sm" onclick={clearKey}>{t("清除")}</button>{/if}
        </div>
        <div class="row">
          <span class="lbl w">{t("模型")}</span>
          <input class="grow mono" list="ga-api-models" placeholder="gpt-4o-mini" bind:value={draft.api.model} />
          <datalist id="ga-api-models">{#each apiModels as m (m)}<option value={m}></option>{/each}</datalist>
          <button class="btn ghost sm" disabled={fetching || !draft.api.base_url.trim()} onclick={fetchModels}>{fetching ? t("连接中…") : t("测试连接 / 获取模型")}</button>
        </div>
        <div class="row">
          <span class="lbl w">{t("温度")}</span>
          <input class="num" type="number" min="0" max="2" step="0.1" placeholder={t("默认")} value={draft.api.temperature ?? ""} oninput={(e) => { const v = (e.target as HTMLInputElement).value; if (draft) draft.api.temperature = v === "" ? null : Number(v); }} />
          <span class="pend">{t("留空使用接口默认值")}</span>
        </div>
        <p class="note">{t("支持 OpenAI、DeepSeek、通义、Moonshot、本地 Ollama / LM Studio 等兼容 /chat/completions 的服务。密钥只保存在本机。")}</p>
      {/if}
    </div>

    <div class="sec">
      <h3>{t("人设")}</h3>
      <div class="row">
        <span class="lbl w">{t("角色")}</span>
        <label class="check"><input type="radio" value="skin" bind:group={draft.persona_mode} /> {t("跟随皮肤角色")}</label>
        <label class="check"><input type="radio" value="custom" bind:group={draft.persona_mode} /> {t("自定义")}</label>
      </div>
      {#if draft.persona_mode === "custom"}
        <div class="row">
          <span class="lbl w">{t("名字")}</span>
          <input class="grow" maxlength="40" placeholder={current.name} bind:value={draft.custom_name} />
        </div>
        <div class="row top">
          <span class="lbl w">{t("设定")}</span>
          <textarea class="grow" rows="4" maxlength="4000" placeholder={t("性格、说话风格、口头禅、对用户的称呼等")} bind:value={draft.custom_persona}></textarea>
        </div>
      {/if}
      <div class="preview">
        <div><b>{t("当前角色")}：</b>{draft.persona_mode === "custom" && draft.custom_name.trim() ? draft.custom_name.trim() : current.name}<span class="sep"> · </span>{styleLabel(current.style)}</div>
        {#if draft.persona_mode !== "custom"}<div class="dim clamp">{current.persona}</div>{/if}
      </div>
      <p class="note">{t("无皮肤时是左下角的简易助手；带人物的皮肤会在人物头部旁显示说话气泡（朗读时显示同步字幕），脚下是输入控制台，点击人物打开（牧濑红莉栖·动态 为终端风格）。皮肤作者可在 manifest 里用 assistant 字段自定义角色设定。")}</p>
    </div>

    <div class="sec">
      <h3>{t("记忆与气泡")}</h3>
      <label class="check"><input type="checkbox" bind:checked={draft.auto_memory} /> {t("自动记忆（助手在对话中发现值得记住的信息时自动记下）")}</label>
      <label class="check"><input type="checkbox" bind:checked={draft.mcp_tools} /> {t("给助手开放使驾 MCP 工具（读写项目上下文、管理 / 运行工作流、给自己设定时——到点由工作流唤醒她，定时在工作流「无项目」里可查看 / 删除）")}</label>
      <label class="check"><input type="checkbox" bind:checked={draft.share_app_state} /> {t("附带使驾当前状态（项目名与路径、共享上下文、页面、编程 Agent）")}</label>
      <div class="row">
        <span class="lbl w2">{t("每轮带上记忆")}</span>
        <input class="num" type="number" min="0" max="200" bind:value={draft.memory_limit} /><span class="pend">{t("条（置顶优先）")}</span>
      </div>
      <div class="row">
        <span class="lbl w2">{t("历史回顾")}</span>
        <input class="num" type="number" min="0" max="60" bind:value={draft.history_turns} /><span class="pend">{t("条（API 模式每轮附带；ACP 新会话首轮回顾）")}</span>
      </div>
      <div class="row">
        <span class="lbl w2">{t("气泡自动收起")}</span>
        <input class="num" type="number" min="0" max="600" bind:value={draft.bubble_seconds} /><span class="pend">{t("秒（0 = 不自动收起）")}</span>
      </div>
    </div>

    <div class="sec">
      <div class="sec-head">
        <h3 class="grow">{t("主动关怀")}</h3>
        <label class="switch" title={t("你不忙的时候，她偶尔会主动找你说句话")}>
          <input type="checkbox" bind:checked={draft.care_enabled} />
          <span class="slider"></span>
        </label>
      </div>
      <p class="note">{t("你不忙的时候，她偶尔会主动搭句话：深夜提醒早点休息、连续忙太久劝你歇会儿、饭点、还没做完的待办、晚上问问今天过得怎么样…… 只在窗口在前台、你在电脑前但没在打字时出现，每天最多 6 次；她觉得会打扰时会选择不说。")}</p>
      {#if draft.care_enabled}
        <div class="row">
          <span class="lbl w2">{t("最短间隔")}</span>
          <input class="num" type="number" min="30" max="600" step="10" bind:value={draft.care_interval_minutes} /><span class="pend">{t("分钟（30~600）")}</span>
        </div>
        <div class="row">
          <span class="lbl w2">{t("深夜从")}</span>
          <input class="num" type="number" min="0" max="23" bind:value={draft.care_late_hour} /><span class="pend">{t("点开始算深夜（到凌晨 5 点）")}</span>
        </div>
        <label class="check"><input type="checkbox" bind:checked={draft.care_voice} /> {t("说完后进入连续语音对话（可以直接开口回她，不说话就自动结束）")}</label>
        {#if draft.care_voice && !inputEnabled()}<p class="note">{t("「语音输入」未开启：只会显示气泡，不会开麦。")}</p>{/if}
      {/if}
      <div class="rowbtns">
        <button class="btn sm" disabled={caring || !!ga.busy || !view.config.enabled} onclick={testCare}>{caring ? t("她在想…") : t("现在试一下")}</button>
        <span class="pend">{t("「待办」栏（标题栏）里没完成的事项也会被她惦记着。")}</span>
      </div>
    </div>

    <VoiceSettings />

    <div class="sec">
      <h3>{t("长期会话")}</h3>
      {#if view.session_id}
        <div class="row">
          <span class="lbl w">{t("当前会话")}</span>
          <code class="grow">{view.session_provider === "api" ? t("自定义 API") : view.session_agent} · {view.session_id.slice(0, 18)}{view.session_id.length > 18 ? "…" : ""}</code>
        </div>
        {#if view.session_created_at}<div class="row"><span class="lbl w">{t("创建于")}</span><span class="pend">{view.session_created_at.replace("T", " ")}</span></div>{/if}
      {:else}
        <div class="dim">{t("尚未建立会话（启用后第一次对话时自动创建并打招呼）")}</div>
      {/if}
      <div class="rowbtns">
        <button class="btn sm" disabled={!view.config.enabled || !!ga.busy} onclick={newSession}>{t("新建长期会话")}</button>
        <button class="btn sm" onclick={clearHistory}>{t("清空对话记录")}</button>
      </div>
      <p class="note">{t("对话记录与长期记忆保存在本机数据库，与客户端会话分开：切换客户端或新建会话后，助手仍记得你。")}</p>
    </div>

    <div class="save-bar" class:dirty>
      {#if dirty}<span class="pend">{t("有未保存的修改")}</span>{/if}
      <button class="btn sm" disabled={!dirty || saving} onclick={load}>{t("撤销")}</button>
      <button class="btn sm primary" disabled={!dirty || saving} onclick={save}>{saving ? t("保存中…") : t("保存")}</button>
    </div>
  </section>
{:else}
  <div class="dim">{t("加载中…")}</div>
{/if}

<style>
  .ga-set { display: flex; flex-direction: column; gap: 14px; }
  .status-row { display: flex; align-items: center; gap: 8px; font-size: .92em; }
  .grow { flex: 1; min-width: 0; }
  .dot { width: 10px; height: 10px; border-radius: 50%; display: inline-block; }
  .dot.on { background: #34c77b; box-shadow: 0 0 6px #34c77b; }
  .dot.off { background: var(--text-faint); }
  .sep { color: var(--text-faint); }
  .sec h3 { font-size: .9em; color: var(--text-dim); margin-bottom: 8px; }
  .sec > .check { display: flex; margin-bottom: 6px; }
  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 6px; }
  .row.top { align-items: flex-start; }
  .check { display: inline-flex; align-items: center; gap: 6px; font-size: .88em; color: var(--text-dim); }
  .lbl { font-size: .86em; color: var(--text-dim); }
  .lbl.w { width: 5.5em; flex: none; }
  .lbl.w2 { width: 7em; flex: none; }
  .num { width: 5.5em; }
  select { min-width: 12em; max-width: 100%; }
  textarea { resize: vertical; font: inherit; }
  code { font-family: var(--mono); font-size: .86em; user-select: text; word-break: break-all; }
  .mono { font-family: var(--mono); }
  .note { font-size: .82em; color: var(--text-faint); line-height: 1.5; margin: 0; }
  .pend { font-size: .82em; color: var(--text-faint); }
  .dim { color: var(--text-faint); font-size: .86em; }
  .clamp { display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; white-space: pre-line; margin-top: 4px; }
  .preview { border: 1px solid var(--border-soft); border-radius: 8px; padding: 8px 10px; font-size: .86em; margin-bottom: 6px; background: var(--bg-elev2); }
  .rowbtns { display: flex; gap: 8px; flex-wrap: wrap; margin: 6px 0; }
  .switch { position: relative; display: inline-block; width: 38px; height: 22px; flex: none; }
  .switch input { opacity: 0; width: 0; height: 0; }
  .slider { position: absolute; inset: 0; cursor: pointer; border-radius: 22px; background: var(--border); transition: background .2s; }
  .slider::before { content: ""; position: absolute; width: 16px; height: 16px; left: 3px; top: 3px; border-radius: 50%; background: #fff; transition: transform .2s; }
  .switch input:checked + .slider { background: #34c77b; }
  .switch input:checked + .slider::before { transform: translateX(16px); }
  .switch input:disabled + .slider { opacity: .5; cursor: default; }
  .sec-head { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
  .sec-head h3 { margin: 0; }
  /* 保存条：右下角的紧凑浮条，只包住按钮本身 */
  .save-bar {
    position: sticky; bottom: 6px; align-self: flex-end; width: fit-content; max-width: 100%;
    display: flex; align-items: center; gap: 8px; padding: 5px 6px 5px 10px; margin-top: -4px;
    background: var(--bg-elev); border: 1px solid var(--border-soft); border-radius: 10px; box-shadow: var(--shadow);
  }
  .save-bar:not(.dirty) { padding-left: 6px; box-shadow: none; }
  .save-bar.dirty { border-color: var(--accent); }
</style>
