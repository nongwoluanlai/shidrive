<script lang="ts">
  // 设置 → 全局助手 → 语音：语音输入（本地 SenseVoice / 百度极速版）、语音输出（系统 / 本地 TTS / 角色语音包）与组件管理；
  // 以及独立的「唤醒词」一节（离线关键词检测，默认 Christina）。修改即时保存（与上方助手设置的「保存」分开）。
  import { onMount } from "svelte";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { t } from "../i18n";
  import { confirmDialog } from "../dialog.svelte";
  import { vc, initVoice, refreshVoice, saveVoice, packForSkin, packReady, packBaseName, outputVoice, previewVoice, systemVoices, listenOnce, stopCapture, stopSpeaking, displayLang, localModelReady, type OutputVoice } from "../assistant/voice.svelte";
  import { wk } from "../assistant/wake.svelte";
  import type { VoiceConfig, VoicePack, VoiceTask } from "../types";

  let draft = $state<VoiceConfig | null>(null);
  let secret = $state("");
  let showSecret = $state(false);
  let sysVoices = $state<SpeechSynthesisVoice[]>([]);
  let micText = $state("");
  let micBusy = $state(false);
  let showDl = $state(false);

  onMount(() => {
    void initVoice().then(sync);
    // 设置页开着时，听到唤醒词只提示，不去唤起助手
    wk.testing = true;
    const upd = () => (sysVoices = systemVoices());
    upd();
    const ss = "speechSynthesis" in window ? speechSynthesis : null;
    ss?.addEventListener("voiceschanged", upd);
    return () => { ss?.removeEventListener("voiceschanged", upd); clearTimeout(timer); wk.testing = false; };
  });

  function sync() {
    if (vc.view) draft = structuredClone($state.snapshot(vc.view.config)) as VoiceConfig;
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  function commit(delay = 400) {
    clearTimeout(timer);
    timer = setTimeout(() => void save(), delay);
  }
  async function save() {
    if (!draft) return;
    try { await saveVoice($state.snapshot(draft) as VoiceConfig); } catch (e) { toast("error", String(e)); }
  }
  async function saveSecret() {
    if (!draft || !secret.trim()) return;
    clearTimeout(timer);
    try {
      await saveVoice($state.snapshot(draft) as VoiceConfig, secret.trim());
      secret = "";
      toast("ok", t("百度密钥已保存"));
    } catch (e) { toast("error", String(e)); }
  }
  async function clearSecret() {
    if (!draft) return;
    const ok = await confirmDialog({ title: t("清除百度密钥"), message: t("清除已保存的百度 Secret Key？"), danger: true, confirmText: t("清除") });
    if (!ok) return;
    try { await saveVoice($state.snapshot(draft) as VoiceConfig, ""); } catch (e) { toast("error", String(e)); }
  }

  const st = $derived(vc.view?.status ?? null);
  const model = (id: string) => st?.models.find((m) => m.id === id);
  const busyTask = $derived(Object.values(vc.tasks).find((x) => x.running) ?? null);
  const packs = $derived(st?.packs ?? []);
  const effective = $derived(vc.view ? outputVoice() : null);
  const skinName = $derived(app.skin ? app.customSkins[app.skin] ?? app.skin : "");
  const autoPack = $derived.by(() => {
    if (!app.skin || !vc.view) return null;
    const saved = vc.view.config.bindings[app.skin];
    if (saved) return null;
    return packForSkin(app.skin);
  });

  function pct(task: VoiceTask) {
    return task.total ? Math.min(100, Math.round((task.received * 100) / task.total)) : null;
  }
  const mb = (n: number) => (n / 1048576).toFixed(1);

  async function install(id: string) {
    clearTimeout(timer);
    await save();
    try {
      vc.tasks = { ...vc.tasks, [id]: { id, stage: "download", received: 0, total: 0, message: t("准备中…"), running: true } };
      await api.voiceInstall(id);
    } catch (e) {
      vc.tasks = { ...vc.tasks, [id]: { id, stage: "error", received: 0, total: 0, message: String(e), running: false } };
    }
  }
  async function cancelInstall(id: string) {
    await api.voiceCancelInstall(id).catch((e) => toast("error", String(e)));
  }
  async function remove(id: string, label: string) {
    const ok = await confirmDialog({ title: t("删除语音组件"), message: t("删除「{name}」？之后需要时可以重新下载 / 导入。", { name: label }), danger: true, confirmText: t("删除") });
    if (!ok) return;
    stopSpeaking();
    try {
      await api.voiceRemove(id);
      const tasks = { ...vc.tasks };
      delete tasks[id];
      vc.tasks = tasks;
      await refreshVoice();
      toast("ok", t("已删除"));
    } catch (e) { toast("error", String(e)); }
  }

  async function importPack() {
    let path: string | null = null;
    try { path = await api.voicePickPack(); } catch (e) { toast("error", String(e)); return; }
    if (!path) return;
    let pack: VoicePack | null = null;
    try {
      pack = await api.voiceImportPack(path);
    } catch (e) {
      const msg = String(e);
      if (!msg.startsWith("EXISTS:")) { toast("error", msg); return; }
      const ok = await confirmDialog({ title: t("覆盖语音包"), message: t("已安装同名语音包「{id}」，用新文件覆盖吗？", { id: msg.slice(7) }), confirmText: t("覆盖") });
      if (!ok) return;
      try { pack = await api.voiceImportPack(path, true); } catch (e2) { toast("error", String(e2)); return; }
    }
    await refreshVoice();
    if (pack) toast("ok", t("已导入语音包：{name}", { name: pack.name }));
  }

  const SAMPLE: Record<string, string> = {
    zh: "你好，我是你的全局助手。今天也一起加油吧。",
    en: "Hello, I'm your global assistant. Let's get things done today.",
    ja: "はじめまして、牧瀬クリスよ。よろしくね。",
  };
  function preview(v: OutputVoice) {
    previewVoice(v, SAMPLE[v.lang] ?? SAMPLE.zh);
  }
  const lang = $derived(displayLang());
  const sysPreview = (): OutputVoice => ({ kind: "system", id: "system", name: t("系统语音"), lang, layer: false });
  const localPreview = (k: "melo" | "kokoro"): OutputVoice => ({ kind: k, id: k, name: k, lang, layer: false });
  const packPreview = (p: VoicePack): OutputVoice => ({ kind: "pack", id: `pack:${p.id}`, name: p.name, lang: p.lang || "ja", layer: false });

  async function testMic() {
    if (vc.recording) { stopCapture(true); return; }
    micBusy = true;
    micText = "";
    try {
      const text = await listenOnce();
      micText = text || t("（没有听清）");
    } catch (e) {
      toast("error", e instanceof Error ? e.message : String(e));
    } finally {
      micBusy = false;
    }
  }

  const wakeModel = $derived(model("kws-gigaspeech"));
  const wakeStatus = $derived.by(() => {
    if (!draft?.wake_enabled) return "";
    if (!st?.runtime_installed) return t("需要先安装上面的语音运行环境");
    if (!wakeModel?.installed) return t("需要先下载唤醒词模型");
    if (wk.error) return `${t("出错")}：${wk.error}`;
    if (wk.listening && wk.suspended) return t("点一下窗口任意位置后开始监听");
    if (wk.listening) return t("正在监听");
    if (!wk.visible && !draft.wake_background) return t("窗口在后台，已暂停");
    return t("准备中…");
  });
  /** 刚刚听到唤醒词（设置页里 3 秒内高亮） */
  let heardFlash = $state(false);
  $effect(() => {
    if (!wk.hits) return;
    heardFlash = true;
    const id = setTimeout(() => (heardFlash = false), 3000);
    return () => clearTimeout(id);
  });
  const sensitivity = $derived(draft ? Math.round(((0.6 - draft.wake_threshold) / 0.55) * 100) : 0);

  const langName = (l: string) => ({ ja: t("日语"), zh: t("中文"), en: t("英语"), ko: t("韩语"), yue: t("粤语") })[l] ?? l;
  const sysList = $derived(sysVoices.filter((v) => /^(zh|en|ja)/i.test(v.lang)));
</script>

{#snippet taskRow(id: string)}
  {@const task = vc.tasks[id]}
  {#if task && (task.running || task.stage === "error" || task.stage === "cancelled")}
    <div class="task" class:err={task.stage === "error"}>
      {#if task.running}
        {@const p = pct(task)}
        <div class="bar"><i style="width:{p ?? 100}%" class:indet={p === null}></i></div>
        <div class="tmsg">
          <span class="grow ell">{task.message}</span>
          {#if task.stage === "download" && task.received}<span>{mb(task.received)}{task.total ? ` / ${mb(task.total)}` : ""} MB</span>{/if}
          <button class="btn ghost sm" onclick={() => cancelInstall(id)}>{t("取消")}</button>
        </div>
      {:else}
        <div class="tmsg"><span class="grow">{task.stage === "cancelled" ? t("已取消") : task.message}</span></div>
      {/if}
    </div>
  {/if}
{/snippet}

{#snippet modelRow(id: string, label: string)}
  {@const m = model(id)}
  {#if m}
    <div class="comp">
      <span class="dot {m.installed ? 'on' : 'off'}"></span>
      <span class="grow">{label} <span class="dim">· {m.installed ? t("已安装") : t("约 {n} MB", { n: m.size_mb })}</span></span>
      {#if !m.installed}
        <button class="btn sm" disabled={!!busyTask} onclick={() => install(id)}>{t("下载安装")}</button>
      {:else}
        <button class="btn ghost sm" disabled={!!busyTask} onclick={() => remove(id, m.name)}>{t("删除")}</button>
      {/if}
    </div>
    {@render taskRow(id)}
  {/if}
{/snippet}

{#if draft && st}
  <div class="sec voice">
    <h3>{t("语音")}</h3>

    <div class="sub">{t("语音运行环境")}</div>
    <div class="comp">
      <span class="dot {st.runtime_installed ? 'on' : 'off'}"></span>
      <span class="grow">
        {#if st.runtime_installed}
          {t("已安装")} · Python {st.runtime_info?.python ?? ""} · sherpa-onnx {st.runtime_info?.sherpa_onnx ?? ""}
        {:else}
          {t("未安装（本地识别 / 本地语音 / 角色语音需要，约 90 MB）")}
        {/if}
      </span>
      {#if st.runtime_installed}
        <button class="btn ghost sm" disabled={!!busyTask} onclick={() => install("runtime")}>{t("重新安装")}</button>
        <button class="btn ghost sm" disabled={!!busyTask} onclick={() => remove("runtime", t("语音运行环境"))}>{t("删除")}</button>
      {:else}
        <button class="btn sm primary" disabled={!!busyTask} onclick={() => install("runtime")}>{t("下载安装")}</button>
      {/if}
    </div>
    {@render taskRow("runtime")}
    <p class="note">{t("独立的 Python 3.12 嵌入版 + sherpa-onnx / onnxruntime / pyopenjtalk，装在使驾数据目录，不影响系统环境；全部离线运行。")}
      <button class="link" onclick={() => api.voiceOpenDir().catch((e) => toast("error", String(e)))}>{t("打开目录")}</button>
      <button class="link" onclick={() => (showDl = !showDl)}>{showDl ? t("收起下载设置") : t("下载设置")}</button>
    </p>
    {#if showDl}
      <div class="dl">
        <div class="row">
          <span class="lbl w">{t("下载源")}</span>
          <label class="check"><input type="radio" value="official" bind:group={draft.source} onchange={() => commit(0)} /> {t("官方")}</label>
          <label class="check"><input type="radio" value="cn" bind:group={draft.source} onchange={() => commit(0)} /> {t("国内镜像（Python: npmmirror，PyPI: 清华）")}</label>
        </div>
        <div class="row">
          <span class="lbl w">{t("GitHub 加速")}</span>
          <input class="grow mono" placeholder={t("可选，如 https://ghfast.top/（模型从 GitHub 下载）")} bind:value={draft.github_prefix} oninput={() => commit()} />
        </div>
        <div class="row">
          <span class="lbl w">PyPI</span>
          <input class="grow mono" placeholder={t("可选，自定义索引，如 https://mirrors.aliyun.com/pypi/simple")} bind:value={draft.pypi_index} oninput={() => commit()} />
        </div>
        <div class="row">
          <span class="lbl w">{t("代理")}</span>
          <input class="grow mono" placeholder="http://127.0.0.1:7890" bind:value={draft.proxy} oninput={() => commit()} />
        </div>
      </div>
    {/if}

    <!-- ---------------- 语音输入 ---------------- -->
    <div class="sub row">
      <span class="grow">{t("语音输入")}</span>
      <label class="switch" title={t("在助手输入框旁显示麦克风按钮")}>
        <input type="checkbox" bind:checked={draft.input_enabled} onchange={() => commit(0)} />
        <span class="slider"></span>
      </label>
    </div>
    {#if draft.input_enabled}
      <div class="row">
        <span class="lbl w">{t("识别引擎")}</span>
        <label class="check"><input type="radio" value="local" bind:group={draft.input_engine} onchange={() => commit(0)} /> {t("本地离线（SenseVoice）")}</label>
        <label class="check"><input type="radio" value="baidu" bind:group={draft.input_engine} onchange={() => commit(0)} /> {t("百度短语音识别极速版")}</label>
      </div>
      {#if draft.input_engine === "local"}
        {@render modelRow("asr-sensevoice", t("SenseVoice 识别模型（中 / 英 / 日 / 韩 / 粤）"))}
        {#if !st.runtime_installed}<p class="warn">{t("需要先安装上面的语音运行环境。")}</p>{/if}
      {:else}
        <div class="row">
          <span class="lbl w">API Key</span>
          <input class="grow mono" autocomplete="off" bind:value={draft.baidu_api_key} oninput={() => commit()} />
        </div>
        <div class="row">
          <span class="lbl w">Secret Key</span>
          <input class="grow mono" type={showSecret ? "text" : "password"} autocomplete="new-password" placeholder={vc.view?.baidu_secret_set ? t("已保存（留空不修改）") : ""} bind:value={secret} />
          <button class="btn ghost sm" onclick={() => (showSecret = !showSecret)}>{showSecret ? t("隐藏") : t("显示")}</button>
          <button class="btn sm" disabled={!secret.trim()} onclick={saveSecret}>{t("保存")}</button>
          {#if vc.view?.baidu_secret_set}<button class="btn ghost sm" onclick={clearSecret}>{t("清除")}</button>{/if}
        </div>
        <p class="note">{t("在百度智能云控制台创建语音技术应用并开通「短语音识别极速版」，填入 API Key / Secret Key。录音会发送到百度服务器识别，单次最长 60 秒。")}</p>
      {/if}
      <label class="check"><input type="checkbox" bind:checked={draft.input_auto_send} onchange={() => commit(0)} /> {t("识别后直接发送（关闭则填入输入框）")}</label>
      <div class="row">
        <span class="lbl w2">{t("单次最长")}</span>
        <input class="num" type="number" min="5" max="60" bind:value={draft.input_max_seconds} oninput={() => commit()} /><span class="pend">{t("秒（说完停顿约 1.5 秒自动结束）")}</span>
      </div>
      <div class="row">
        <button class="btn sm" disabled={micBusy && !vc.recording} onclick={testMic}>{vc.recording ? t("结束录音") : vc.transcribing ? t("识别中…") : t("测试麦克风")}</button>
        {#if vc.recording}<span class="lv"><i style="width:{Math.round(vc.level * 100)}%"></i></span>{/if}
        {#if micText}<span class="pend">{t("识别结果")}：{micText}</span>{/if}
      </div>
    {/if}

    <!-- ---------------- 语音输出 ---------------- -->
    <div class="sub row">
      <span class="grow">{t("语音输出")}</span>
      <label class="switch" title={t("助手的回复逐句朗读")}>
        <input type="checkbox" bind:checked={draft.output_enabled} onchange={() => commit(0)} />
        <span class="slider"></span>
      </label>
    </div>
    {#if draft.output_enabled}
      <div class="row">
        <span class="lbl w">{t("通用语音")}</span>
        <select bind:value={draft.default_voice} onchange={() => commit(0)}>
          <option value="system">{t("系统语音（Windows 自带，免下载）")}</option>
          <option value="melo">{t("MeloTTS 中英（离线，快速）")}</option>
          <option value="kokoro">{t("Kokoro 中英多音色（离线，较慢）")}</option>
        </select>
        <button class="btn ghost sm" onclick={() => (draft!.default_voice === "system" ? preview(sysPreview()) : preview(localPreview(draft!.default_voice as "melo" | "kokoro")))} disabled={draft.default_voice !== "system" && !localModelReady(draft.default_voice)}>{t("试听")}</button>
      </div>
      {#if draft.default_voice === "system"}
        <div class="row">
          <span class="lbl w">{t("系统音色")}</span>
          <select bind:value={draft.system_voice} onchange={() => commit(0)}>
            <option value="">{t("按语言自动")}</option>
            {#each sysList as v (v.name)}<option value={v.name}>{v.name}（{v.lang}）</option>{/each}
          </select>
        </div>
      {:else}
        {@render modelRow(draft.default_voice === "melo" ? "tts-melo" : "tts-kokoro", draft.default_voice === "melo" ? t("MeloTTS 中英模型") : t("Kokoro v1.1 中英模型"))}
        {#if draft.default_voice === "kokoro"}
          <div class="row">
            <span class="lbl w">{t("音色编号")}</span>
            <input class="num" type="number" min="0" max="102" bind:value={draft.kokoro_speaker} oninput={() => commit()} /><span class="pend">{t("0~102（3 起为中文音色）")}</span>
          </div>
        {/if}
        {#if !st.runtime_installed}<p class="warn">{t("需要先安装上面的语音运行环境；未就绪时自动改用系统语音。")}</p>{/if}
      {/if}
      <div class="row">
        <span class="lbl w">{t("语速")}</span>
        <input type="range" min="0.5" max="2" step="0.05" bind:value={draft.rate} oninput={() => commit()} /><span class="pend">{draft.rate.toFixed(2)}×</span>
        <span class="lbl">{t("音量")}</span>
        <input type="range" min="0" max="1" step="0.05" bind:value={draft.volume} oninput={() => commit()} /><span class="pend">{Math.round(draft.volume * 100)}%</span>
      </div>

      <label class="check" title={t("关闭后只读气泡里的简短字幕；角色配音（如日语）时会要求助手把要说的话全部写成字幕")}><input type="checkbox" bind:checked={draft.read_full} onchange={() => commit(0)} /> {t("完整朗读回复（正文也逐句读出，代码、表格、链接不读）")}</label>
      <label class="check"><input type="checkbox" bind:checked={draft.character_voice} onchange={() => commit(0)} /> {t("皮肤角色有专属语音包时使用角色语音")}</label>
      {#if draft.character_voice}
        {#if app.skin}
          <div class="row">
            <span class="lbl w">{t("当前皮肤")}</span>
            <span class="pend">{skinName}</span>
            <select value={draft.bindings[app.skin] ?? ""} onchange={(e) => { const v = (e.target as HTMLSelectElement).value; if (!draft) return; const b = { ...draft.bindings }; if (v) b[app.skin] = v; else delete b[app.skin]; draft.bindings = b; commit(0); }}>
              <option value="">{t("自动")}{autoPack ? `（${autoPack.name}）` : t("（无匹配）")}</option>
              <option value="none">{t("不使用角色语音")}</option>
              {#each packs as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
            </select>
          </div>
        {/if}
        {#each packs as p (p.id)}
          <div class="comp pack">
            <span class="dot on"></span>
            <span class="grow">
              {p.name} <span class="dim">· {langName(p.lang)}{p.skins.length ? ` · ${t("皮肤")} ${p.skins.join(", ")}` : ""}{p.license ? ` · ${p.license}` : ""}</span>
              {#if p.engine === "preset"}<span class="dim block">{t("基于 {base} 的预设音色", { base: packBaseName(p) })}{packReady(p) ? "" : ` · ${p.base?.startsWith("pack:") ? t("需先导入该语音包") : t("需先安装该模型")}`}</span>{/if}
              {#if p.credits || p.source}<span class="dim block">{p.credits}{p.source ? ` ${p.source}` : ""}</span>{/if}
            </span>
            <button class="btn ghost sm" disabled={!packReady(p)} onclick={() => preview(packPreview(p))}>{t("试听")}</button>
            <button class="btn ghost sm" onclick={() => remove(`pack:${p.id}`, p.name)}>{t("删除")}</button>
          </div>
        {:else}
          <div class="dim">{t("还没有角色语音包。")}</div>
        {/each}
        <div class="rowbtns">
          <button class="btn sm" onclick={importPack}>{t("导入语音包（.zip）…")}</button>
          <button class="linklike" onclick={() => void api.fsOpenDefault("https://shidrive.nwll.top/resources#voice").catch((e) => toast("error", String(e)))}>{t("获取语音包 ↗")}</button>
        </div>
        <p class="note">{t("角色语音包含模型、音素表与 voice.json（声明语言与适用皮肤）。语言与界面不同时（如日语配音），助手会在每句后附上该语言的台词用于朗读，气泡里仍显示中文。")}</p>
      {/if}
      {#if effective}
        <div class="preview">
          <b>{t("当前朗读")}：</b>{effective.name}{effective.layer ? ` · ${t("{lang}配音，字幕为界面语言", { lang: langName(effective.lang) })}` : ""}
        </div>
      {/if}
      <p class="note">{t("开启后会要求模型用短句、尽快给出第一句，边生成边朗读；代码块和链接不会被读出。人物气泡只在朗读时显示字幕，读完即收起；点控制台里的喇叭可停止朗读。")}</p>
    {/if}
  </div>

  <!-- ---------------- 唤醒词（独立一节） ---------------- -->
  <div class="sec voice wake">
    <div class="row">
      <h3 class="grow">{t("唤醒词")}</h3>
      <label class="switch" title={t("常开麦克风，听到唤醒词后唤起助手")}>
        <input type="checkbox" bind:checked={draft.wake_enabled} onchange={() => commit(0)} />
        <span class="slider"></span>
      </label>
    </div>
    <p class="note">{t("喊一声唤醒词（默认「Christina」）即可唤起助手，免去点击或按键。完全离线：麦克风音频只在本机检测，不会上传。")}</p>
    {#if draft.wake_enabled}
      {@render modelRow("kws-gigaspeech", t("唤醒词模型（英文关键词检测）"))}
      {#if !st.runtime_installed}<p class="warn">{t("需要先安装上面的语音运行环境。")}</p>{/if}
      <div class="row">
        <span class="lbl w">{t("唤醒词")}</span>
        <input class="grow" placeholder="Christina" bind:value={draft.wake_keywords} onchange={() => commit(0)} />
      </div>
      <p class="note">{t("英文单词或短语，多个用逗号分隔（最多 4 个），如「Christina, Hey Kurisu」。模型只认英文发音。")}</p>
      <div class="row">
        <span class="lbl w">{t("灵敏度")}</span>
        <span class="pend">{t("严格")}</span>
        <input type="range" min="0" max="100" step="1" value={sensitivity} oninput={(e) => { if (!draft) return; const v = Number((e.target as HTMLInputElement).value); draft.wake_threshold = Math.round((0.6 - (v / 100) * 0.55) * 100) / 100; commit(); }} />
        <span class="pend">{t("灵敏")}</span>
        <span class="pend">（{t("阈值")} {draft.wake_threshold.toFixed(2)}）</span>
      </div>
      <div class="row">
        <span class="lbl w">{t("唤醒后")}</span>
        <label class="check"><input type="radio" value="listen" bind:group={draft.wake_action} onchange={() => commit(0)} /> {t("直接开始语音输入")}</label>
        <label class="check"><input type="radio" value="open" bind:group={draft.wake_action} onchange={() => commit(0)} /> {t("只打开输入框")}</label>
      </div>
      {#if draft.wake_action === "listen" && !draft.input_enabled}<p class="warn">{t("「语音输入」未开启：唤醒后只会打开输入框。")}</p>{/if}
      <label class="check" title={t("唤醒后进入连续语音对话：她说完会自动接着听，觉得聊完了会自己停下；你不说话、开始打字、按 Esc 或点「结束」也会退出")}>
        <input type="checkbox" bind:checked={draft.wake_multiturn} disabled={draft.wake_action !== "listen"} onchange={() => commit(0)} /> {t("唤醒后多轮对话（她说完自动继续听，由她判断何时结束）")}
      </label>
      <label class="check"><input type="checkbox" bind:checked={draft.wake_chime} onchange={() => commit(0)} /> {t("唤醒时播放提示音")}</label>
      <label class="check"><input type="checkbox" bind:checked={draft.wake_background} onchange={() => commit(0)} /> {t("窗口最小化 / 在后台时也监听（唤醒时切到前台）")}</label>
      {#if wakeStatus}
        <div class="preview" class:heard={heardFlash}>
          <span class="dot {wk.listening ? 'on' : 'off'}"></span>
          <b>{t("状态")}：</b>{wakeStatus}
          {#if wk.listening}<span class="lv wlv"><i style="width:{Math.round(wk.level * 100)}%"></i></span>{/if}
          {#if wk.lastHit}<span class="pend">· {heardFlash ? t("听到了：{word}", { word: wk.lastHit }) : t("上次听到：{word}", { word: wk.lastHit })}</span>{/if}
        </div>
      {/if}
      <p class="note">{t("设置页开着时只做测试：听到唤醒词会提示并响一声，不会唤起助手。录音、识别和朗读期间暂停检测。")}</p>
    {/if}
  </div>
{/if}

<style>
  .voice { display: flex; flex-direction: column; gap: 6px; }
  .voice h3 { font-size: .9em; color: var(--text-dim); margin-bottom: 2px; }
  .sub { font-size: .86em; color: var(--text); font-weight: 600; margin-top: 8px; }
  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .check { display: inline-flex; align-items: center; gap: 6px; font-size: .88em; color: var(--text-dim); }
  .lbl { font-size: .86em; color: var(--text-dim); }
  .lbl.w { width: 5.5em; flex: none; }
  .lbl.w2 { width: 7em; flex: none; }
  .num { width: 5.5em; }
  select { min-width: 12em; max-width: 100%; }
  .mono { font-family: var(--mono); }
  .note { font-size: .82em; color: var(--text-faint); line-height: 1.5; margin: 0; }
  .warn { font-size: .82em; color: #e5a53a; margin: 0; }
  .pend { font-size: .82em; color: var(--text-faint); }
  .dim { color: var(--text-faint); font-size: .92em; }
  .block { display: block; font-size: .85em; }
  .comp { display: flex; align-items: center; gap: 8px; font-size: .86em; padding: 6px 8px; border: 1px solid var(--border-soft); border-radius: 8px; background: var(--bg-elev2); }
  .dot { width: 8px; height: 8px; border-radius: 50%; display: inline-block; flex: none; }
  .dot.on { background: #34c77b; }
  .dot.off { background: var(--text-faint); }
  .task { font-size: .82em; color: var(--text-dim); padding: 0 2px; }
  .task.err { color: #e5484d; }
  .bar { height: 4px; border-radius: 2px; background: var(--border-soft); overflow: hidden; margin: 2px 0 4px; }
  .bar i { display: block; height: 100%; background: var(--accent); transition: width .2s; }
  .bar i.indet { width: 30% !important; animation: vs-indet 1.2s linear infinite; }
  @keyframes vs-indet { from { transform: translateX(-100%); } to { transform: translateX(340%); } }
  .tmsg { display: flex; align-items: center; gap: 8px; }
  .lv { width: 120px; height: 6px; border-radius: 3px; background: var(--border-soft); overflow: hidden; }
  .lv i { display: block; height: 100%; background: #e5484d; }
  .dl { border-left: 2px solid var(--border-soft); padding-left: 10px; display: flex; flex-direction: column; gap: 6px; }
  .link { background: none; border: 0; padding: 0 0 0 8px; color: var(--accent); cursor: pointer; font-size: inherit; }
  .preview { border: 1px solid var(--border-soft); border-radius: 8px; padding: 6px 10px; font-size: .86em; background: var(--bg-elev2); }
  .rowbtns { display: flex; gap: 8px; flex-wrap: wrap; }
  .linklike { color: var(--accent); font-size: .85em; background: none; border: none; padding: 0; cursor: pointer; align-self: center; }
  .linklike:hover { text-decoration: underline; }
  .wake h3 { margin: 0; }
  .wake .preview { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; transition: border-color .2s, background .2s; }
  .wake .preview.heard { border-color: #34c77b; background: color-mix(in srgb, #34c77b 14%, var(--bg-elev2)); }
  .wlv { width: 80px; }
  .wlv i { background: #34c77b; }
  .switch { position: relative; display: inline-block; width: 38px; height: 22px; flex: none; }
  .switch input { opacity: 0; width: 0; height: 0; }
  .slider { position: absolute; inset: 0; cursor: pointer; border-radius: 22px; background: var(--border); transition: background .2s; }
  .slider::before { content: ""; position: absolute; width: 16px; height: 16px; left: 3px; top: 3px; border-radius: 50%; background: #fff; transition: transform .2s; }
  .switch input:checked + .slider { background: #34c77b; }
  .switch input:checked + .slider::before { transform: translateX(16px); }
</style>
