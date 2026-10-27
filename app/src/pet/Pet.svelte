<script lang="ts">
  // 桌宠：右下角透明小窗。人物沿用当前皮肤（动态看板 / 静态立绘），上方是对话气泡，下方是输入条。
  // 只做显示与输入：状态来自主窗口（pet://state），操作发回主窗口（pet://cmd）。
  import { onMount, tick } from "svelte";
  import { emit, listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { api } from "../lib/ipc";
  import { app } from "../lib/state.svelte";
  import { t } from "../lib/i18n";
  import { md } from "../lib/markdown";
  import { activateSkin } from "../lib/skins.svelte";
  import { loadSkinMotion } from "../lib/skin-fx/motion.svelte";
  import { emitSkinEvent, onSkinEvent } from "../lib/skin-fx/bus";
  import SkinCharacter from "../lib/components/SkinCharacter.svelte";
  // 麦克风在桌宠窗口里开（可见窗口的 AudioContext 才能运行；主窗口藏进托盘后录音会挂起）
  import { vc, listenOnce, stopCapture, refreshVoice } from "../lib/assistant/voice.svelte";
  import type { PetCmd, PetState } from "../lib/assistant/pet.svelte";
  import logo from "../assets/logo.png";

  let st = $state<PetState | null>(null);
  let input = $state("");
  let inputEl = $state<HTMLTextAreaElement | null>(null);
  let composing = $state(false);

  const cmd = (c: PetCmd) => void emit("pet://cmd", c).catch(() => {});

  $effect(() => activateSkin(app.skin, () => { app.skin = ""; }));

  const status = $derived(
    !st ? "" : vc.recording ? t("正在听…") : vc.transcribing ? t("识别中…") : st.thinking ? t("思考中…") : st.busy ? t("回复中…") : st.speaking ? t("朗读中…") : "",
  );
  const showBubble = $derived(!!st && (!!st.text || !!st.error || st.thinking));

  // 人物表情跟着主窗口的回合状态走
  let prevBusy = false;
  $effect(() => {
    const b = !!st?.busy;
    if (b && !prevBusy) emitSkinEvent("agent.streaming");
    if (!b && prevBusy) emitSkinEvent(st?.error ? "agent.error" : "agent.done");
    prevBusy = b;
  });

  /** 自动接续：唤醒后的语音轮——听完发送、等回复说完再听，直到没听清或用户停止 */
  let autoTalk = $state(false);
  let autoGen = 0;
  async function startListen(auto = false) {
    if (vc.recording) { stopCapture(true); return; }
    if (vc.transcribing || st?.busy) return;
    if (auto) autoTalk = true;
    const my = ++autoGen;
    let text = "";
    try {
      text = (await listenOnce(auto ? 8 : 10)).trim();
    } catch (e) {
      autoTalk = false;
      st = st ? { ...st, error: e instanceof Error ? e.message : String(e) } : st;
      return;
    }
    if (my !== autoGen) return;
    if (!text) { autoTalk = false; return; }
    emitSkinEvent("message.send");
    cmd({ kind: "send", text });
  }
  // 回复（和朗读）结束且处于自动接续：接着听下一轮
  let wasBusy = false;
  $effect(() => {
    const busy = !!st?.busy || !!st?.speaking;
    if (wasBusy && !busy && autoTalk && !vc.recording && !vc.transcribing) {
      const id = setTimeout(() => { if (autoTalk && !st?.busy && !st?.speaking) void startListen(true); }, 400);
      wasBusy = busy;
      return () => clearTimeout(id);
    }
    wasBusy = busy;
  });
  function stopAll() {
    autoTalk = false;
    autoGen++;
    if (vc.recording || vc.transcribing) stopCapture(true);
    cmd({ kind: "stop" });
  }

  function openInput() {
    composing = true;
    void tick().then(() => inputEl?.focus());
  }

  function submit() {
    const text = input.trim();
    if (!text || st?.busy) return;
    input = "";
    emitSkinEvent("message.send");
    cmd({ kind: "send", text });
  }

  function onKey(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === "Escape") { e.preventDefault(); composing ? (composing = false) : cmd({ kind: "close" }); return; }
    if (e.key !== "Enter") return;
    const enterSend = st?.enterSend ?? true;
    if (enterSend ? !e.shiftKey && !e.ctrlKey : e.ctrlKey || e.metaKey) { e.preventDefault(); submit(); }
  }

  // 链接 / 本地路径交给系统打开
  function onMdClick(e: MouseEvent) {
    const a = (e.target as Element).closest("a");
    if (!a) return;
    e.preventDefault();
    const target = a.getAttribute("data-path") || (/^https?:\/\//i.test(a.getAttribute("href") ?? "") ? a.getAttribute("href")! : "");
    if (target) void api.fsOpenDefault(target).catch(() => {});
  }

  // 只有气泡、输入条、人物区域接收鼠标，窗口其余透明部分点击穿透到下面的程序
  let lastRegions = "";
  function reportRegions() {
    const els = [".bubble", ".dock .bar", ".avatar", ".skin-scene", ".skin-scene-static", ".skin-character"];
    const rects: number[][] = [];
    for (const sel of els) {
      for (const el of document.querySelectorAll(sel)) {
        const r = el.getBoundingClientRect();
        if (r.width > 2 && r.height > 2) rects.push([Math.floor(r.left), Math.floor(r.top), Math.ceil(r.width), Math.ceil(r.height)]);
      }
    }
    const sig = JSON.stringify(rects);
    if (sig === lastRegions) return;
    lastRegions = sig;
    void invoke("pet_regions", { regions: rects }).catch(() => {});
  }

  onMount(() => {
    void loadSkinMotion();
    const regionTimer = setInterval(reportRegions, 300);
    void api.settingsGet("ui.skin").then((s) => { if (st === null) app.skin = s ?? ""; }).catch(() => {});
    void refreshVoice();
    const offs = [
      listen("pet://listen", () => { void tick().then(() => void startListen(true)); }),
      listen<PetState>("pet://state", (e) => {
        st = e.payload;
        if (app.skin !== e.payload.skin) app.skin = e.payload.skin;
        if (e.payload.locale === "en" || e.payload.locale === "zh") app.locale = e.payload.locale;
      }),
      listen("pet://shown", () => cmd({ kind: "ready" })),
    ];
    const offClick = onSkinEvent((ev) => { if (ev === "character.click") openInput(); });
    cmd({ kind: "ready" });
    return () => { clearInterval(regionTimer); offClick(); for (const o of offs) void o.then((f) => f()); };
  });
</script>

<div class="pet" class:has-skin={!!app.skin}>
  {#if showBubble && st}
    <div class="bubble" class:err={!!st.error}>
      <div class="who">{st.name}</div>
      {#if st.text}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="md" onclick={onMdClick}>{@html md(st.text)}</div>
      {:else if st.thinking}
        <span class="dots"><i></i><i></i><i></i></span>
      {/if}
      {#if st.error}<div class="error">{st.error}</div>{/if}
    </div>
  {/if}

  <div class="stage-wrap">
    {#if app.skin}
      <SkinCharacter />
    {:else}
      <button class="avatar" type="button" title={t("和助手说点什么…")} onclick={openInput}><img src={logo} alt="" draggable="false" /></button>
    {/if}
  </div>

  <div class="dock">
    <div class="bar">
      <span class="drag" data-tauri-drag-region title={t("拖动")}>⋮⋮</span>
      {#if composing}
        <textarea bind:this={inputEl} bind:value={input} rows="1" placeholder={t("问问屏幕上的内容…")} onkeydown={onKey}></textarea>
        <button class="b primary" type="button" disabled={!input.trim() || st?.busy} onclick={submit}>{t("发送")}</button>
      {:else}
        <button class="b grow" type="button" onclick={openInput}>{status || t("问问屏幕上的内容…")}</button>
      {/if}
      {#if st?.voiceIn}
        <button class="b mic" class:rec={vc.recording} type="button" style="--lv:{vc.level}" disabled={vc.transcribing || (st?.busy && !vc.recording)} title={t("语音输入")} aria-label={t("语音输入")} onclick={() => void startListen(false)}>🎙</button>
      {/if}
      {#if st?.busy || st?.speaking || st?.conv || vc.recording || autoTalk}
        <button class="b" type="button" title={t("停止")} aria-label={t("停止")} onclick={stopAll}>■</button>
      {/if}
      <button class="b" type="button" title={t("打开使驾")} aria-label={t("打开使驾")} onclick={() => cmd({ kind: "open-main" })}>⤢</button>
      <button class="b" type="button" title={t("收起")} aria-label={t("收起")} onclick={() => cmd({ kind: "close" })}>×</button>
    </div>
    {#if composing || status}<div class="hint">{status || t("提问时会附上当前屏幕截图（不含使驾窗口）")}</div>{/if}
  </div>
</div>
