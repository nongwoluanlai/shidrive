<script lang="ts">
  // 全局助手：无皮肤 → 左下角简易助手（一张卡片）；有皮肤人物 → 拆成两块：
  //   · 说话气泡：贴在人物头部旁的临时提示（不可点击、没有按钮），显示回复；朗读时显示与音频同步的字幕，读完即收起；
  //   · 控制台：在人物脚下（舞台底部），平时只有输入框 + 语音 / 状态反馈，对话记录和记忆是可展开的面板。
  // 牧濑红莉栖·动态 用终端风格（style id 仍叫 amadeus），其它皮肤用通用轻量样式。关闭后不留浮标，点人物唤起；
  // 双击 Ctrl = 语音输入；开启唤醒词后喊「Christina」也能唤起。
  import { onMount, tick, untrack } from "svelte";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { md } from "../markdown";
  import { t } from "../i18n";
  import { confirmDialog } from "../dialog.svelte";
  import { onSkinEvent } from "../skin-fx/bus";
  import { see } from "../skin-fx/seethrough.svelte";
  import { vc, inputEnabled, listenOnce, stopCapture, stopSpeaking, outputVoice, warmVoice } from "../assistant/voice.svelte";
  import { wk, wakeReady, setWake, chime, wakeQuip } from "../assistant/wake.svelte";
  import { ga, enabled, persona, initAssistant, send, cancel, resetSession, hideBubble, showBubble, loadMemories, summarize, visibleText, isThinking, openComposer, memoNote, clearMessages, settingsRequest, conv, subText } from "../assistant/store.svelte";
  import { startConversation, endConversation } from "../assistant/conv.svelte";
  import { initCare } from "../assistant/care.svelte";

  const p = $derived(persona());
  const style = $derived(p.style);
  const title = $derived(t(p.title || p.name));
  const nick = $derived(p.nick || p.name);

  // ---------------- 位置：贴着人物舞台 ----------------
  let stage = $state<{ left: number; top: number; width: number; height: number } | null>(null);
  let winH = $state(innerHeight);
  function measure() {
    winH = innerHeight;
    const el = document.querySelector(".skin-character-stage");
    const r = el?.getBoundingClientRect();
    stage = r && r.width > 40 && r.height > 40 ? { left: r.left, top: r.top, width: r.width, height: r.height } : null;
  }
  $effect(() => {
    void app.skin; void style;
    let ro: ResizeObserver | null = null;
    const id = requestAnimationFrame(() => {
      measure();
      const el = document.querySelector(".skin-character-stage");
      if (el) { ro = new ResizeObserver(measure); ro.observe(el); }
    });
    return () => { cancelAnimationFrame(id); ro?.disconnect(); };
  });
  /** 实际呈现样式：皮肤人物被隐藏（窗口太矮）时退回左下角简易助手 */
  const mode = $derived(style === "plain" || !stage ? "plain" : style);
  const pos = $derived.by(() => {
    if (mode === "plain" || !stage) return { left: 14, bottom: 62 };
    const [ax, ay] = p.anchor ?? [54, 78];
    const left = Math.round(stage.left + (stage.width * ax) / 100);
    const bottom = Math.round(winH - (stage.top + stage.height) + (stage.height * ay) / 100);
    return { left: Math.max(8, left), bottom: Math.min(Math.max(8, bottom), winH - 160) };
  });

  /** 控制台：舞台底部（人物脚下），宽度跟舞台走；展开的面板不高过说话气泡的底边 */
  const dock = $derived.by(() => {
    if (!stage) return { left: 10, bottom: 10, width: 320, panelMax: 300 };
    const bottom = Math.max(8, Math.round(winH - (stage.top + stage.height)) + 10);
    const left = Math.max(8, Math.round(stage.left + 10));
    const width = Math.round(Math.min(380, Math.max(260, stage.width - 20)));
    const room = ga.bubble.visible ? pos.bottom - bottom - 96 : Math.min(winH * 0.62, 540);
    return { left, bottom, width, panelMax: Math.round(Math.max(140, room)) };
  });
  /** 聊天输入框聚焦且控制台压在聊天区上：控制台淡出（悬停恢复） */
  const dockFaded = $derived(see.active && !!see.client && dock.left + dock.width > see.client.x);

  // ---------------- 卡片内容 ----------------
  const cardOpen = $derived(ga.bubble.visible || ga.composer || ga.panel !== null);
  const streamingText = $derived(visibleText(ga.streaming));
  /** 气泡里流式显示的：字幕层优先；还没有字幕时先显示正文 */
  const liveText = $derived(ga.subs.length ? subText(ga.subs) : streamingText);
  const page = $derived(ga.bubble.pages[ga.bubble.index] ?? "");
  const pageCount = $derived(ga.bubble.pages.length);
  const busy = $derived(ga.busy === "send" || ga.busy === "greet" || ga.busy === "care" || ga.busy === "reset");
  /** 朗读刚读完：气泡继续显示最后一句字幕，片刻后收起 */
  let holdSub = $state(false);
  /** 人物气泡在朗读：显示与音频同步的字幕 */
  const subtitleOn = $derived(mode !== "plain" && (vc.voiceTurn || holdSub));
  const status = $derived(
    vc.recording ? t("正在听…") : vc.transcribing ? t("识别中…") : ga.busy === "reset" ? t("正在新建会话…") : busy ? t("回复中…") : vc.speaking ? t("朗读中…") : ga.busy === "summarize" ? t("处理中…") : "",
  );
  /** 输入框下方的按键提示 */
  const keys = $derived(app.enterSend ? [["Enter", t("发送")], ["Shift+Enter", t("换行")]] : [["Ctrl+Enter", t("发送")], ["Enter", t("换行")]]);
  const placeholder = $derived(
    vc.recording ? t("正在听…") : vc.transcribing ? t("识别中…") : busy ? t("回复中…") : t("和{name}说点什么…", { name: nick }),
  );
  const divergence = $derived.by(() => {
    // Amadeus 头部的世界线变动率：随消息数轻微跳动，只是装饰
    const n = ga.messages.length;
    return (1.048596 + ((n * 7919) % 1000) / 1e6).toFixed(6);
  });

  let input = $state("");
  let inputEl = $state<HTMLTextAreaElement | null>(null);
  let bodyEl = $state<HTMLDivElement | null>(null);
  let hovered = $state(false);

  // 只响应聚焦请求：连续对话 / 主动关怀打开控制台时不抢焦点
  $effect(() => {
    void ga.focusRev;
    if (untrack(() => ga.composer)) void tick().then(() => inputEl?.focus());
  });

  // 流式时贴底
  $effect(() => {
    void liveText;
    if (bodyEl && liveText) bodyEl.scrollTop = bodyEl.scrollHeight;
  });

  // 自动翻页与自动收起
  $effect(() => {
    void ga.bubble.rev;
    const idx = ga.bubble.index, n = pageCount, text = page;
    const staged = mode !== "plain";
    // 简易卡片：悬停暂停，输入框 / 面板开着时不自动收起；人物气泡是临时提示（不可悬停），只在朗读字幕时暂停
    if (!ga.bubble.visible || busy || vc.voiceTurn || holdSub || (!staged && (hovered || ga.composer || ga.panel))) return;
    if (idx < n - 1) {
      const ms = Math.min(12000, Math.max(3500, 1200 + text.length * 70));
      const id = setTimeout(() => { if (ga.bubble.index === idx) ga.bubble.index = idx + 1; }, ms);
      return () => clearTimeout(id);
    }
    const secs = ga.view?.config.bubble_seconds ?? 30;
    // 人物气泡没有关闭按钮：出错提示也会自动收起
    if (ga.bubble.error) {
      if (!staged) return;
      const id = setTimeout(() => hideBubble(), 15000);
      return () => clearTimeout(id);
    }
    if (!secs) return;
    const id = setTimeout(() => hideBubble(), secs * 1000 + Math.min(8000, text.length * 40));
    return () => clearTimeout(id);
  });

  // 朗读读完：人物气泡（字幕）随语音一起消失；完整回复可在控制台的对话记录里看
  let spokenSeen = vc.spokenRev;
  $effect(() => {
    const rev = vc.spokenRev;
    if (rev === spokenSeen) return;
    spokenSeen = rev;
    if (mode === "plain") return;
    holdSub = true;
    const id = setTimeout(() => {
      holdSub = false;
      if (!busy && !vc.voiceTurn) hideBubble();
    }, 1200);
    return () => { clearTimeout(id); holdSub = false; };
  });

  function stopTalking() {
    stopSpeaking();
    if (mode !== "plain") hideBubble();
  }

  // 气泡被关掉时 mouseleave 不会触发：复位悬停，免得下一条回复不再自动收起
  $effect(() => { if (!ga.bubble.visible) hovered = false; });

  // 记忆提示：显示 4 秒
  let memoShown = $state("");
  $effect(() => {
    void memoNote.rev;
    if (!memoNote.text) return;
    memoShown = memoNote.text;
    const id = setTimeout(() => (memoShown = ""), 4000);
    return () => clearTimeout(id);
  });

  function prev() { if (ga.bubble.index > 0) ga.bubble.index--; }
  function next() { if (ga.bubble.index < pageCount - 1) ga.bubble.index++; }

  function close() {
    endConversation();
    ga.composer = false;
    ga.panel = null;
    hideBubble();
  }

  async function submit() {
    const text = input.trim();
    if (!text || busy) return;
    input = "";
    ga.panel = ga.panel === "chat" ? "chat" : null;
    await send(text);
  }

  function onKey(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === "Escape") {
      e.preventDefault();
      if (conv.active) { endConversation(); return; }
      if (mode !== "plain") { ga.composer = false; ga.panel = null; return; }
      ga.composer = false;
      if (!ga.panel && !ga.bubble.visible) close();
      return;
    }
    if (e.key !== "Enter") return;
    // 与聊天输入框同一个设置（设置 → Enter 发送消息）
    if (app.enterSend) {
      if (!e.shiftKey && !e.ctrlKey && !e.metaKey) { e.preventDefault(); void submit(); }
    } else if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      void submit();
    }
  }

  // ---------------- 语音 ----------------
  /** 点人物唤起时：pointerdown 里聚焦的输入框会被随后 mousedown 的默认行为抢走焦点，松开后再聚焦一次 */
  function focusAfterClick() {
    let done = false;
    const go = () => {
      if (done) return;
      done = true;
      removeEventListener("pointerup", up, true);
      void tick().then(() => requestAnimationFrame(() => inputEl?.focus()));
    };
    const up = () => setTimeout(go, 30);
    addEventListener("pointerup", up, true);
    setTimeout(go, 500);
  }

  /** 双击 Ctrl / 唤醒词：语音输入（未开启语音输入时只打开输入框） */
  function voiceHotkey(fromWake = false) {
    if (!enabled()) return;
    if (!inputEnabled()) {
      openComposer();
      if (!fromWake) toast("info", t("语音输入未开启：设置 → 全局助手 → 语音"));
      return;
    }
    if (vc.recording) { stopCapture(true); return; }
    if (vc.transcribing) return;
    if (fromWake && vc.view?.config.wake_multiturn) { void startConversation("wake"); return; }
    openComposer();
    void onMic();
  }

  /** 连续对话中用户开始打字：改为手动输入 */
  function onInput() {
    if (conv.active && input.trim()) endConversation();
  }

  function stopAll() {
    endConversation();
    void cancel();
  }

  async function onMic() {
    if (vc.recording) { stopCapture(true); return; }
    if (vc.transcribing) return;
    try {
      const text = (await listenOnce()).trim();
      if (!text) { toast("info", t("没有听清，请再说一次")); return; }
      if (vc.view?.config.input_auto_send && !busy) {
        ga.panel = ga.panel === "chat" ? "chat" : null;
        await send(text);
      } else {
        if (!ga.composer) openComposer();
        input = input.trim() ? `${input.trim()} ${text}` : text;
        void tick().then(() => inputEl?.focus());
      }
    } catch (e) {
      toast("error", e instanceof Error ? e.message : String(e));
    }
  }
  const micTitle = $derived(vc.recording ? t("正在听…（说完自动结束，点击提前结束）") : vc.transcribing ? t("识别中…") : t("语音输入"));

  function openPanel(tab: "chat" | "memory", keep = false) {
    ga.panel = ga.panel === tab && !keep ? null : tab;
    // 人物模式下面板在控制台里：展开面板时控制台跟着出现
    if (ga.panel && mode !== "plain" && !ga.composer) openComposer();
    if (ga.panel === "memory") void loadMemories();
    if (ga.panel === "chat") void tick().then(() => { if (listEl) listEl.scrollTop = listEl.scrollHeight; });
  }

  function onLauncher() {
    if (cardOpen) close();
    else onChip();
  }

  function onChip() {
    // 简易卡片里气泡就是卡片正文，打开时带上上次回复；人物模式的说话气泡只在助手回复 / 朗读时出现，
    // 点人物只打开输入框，不把旧气泡重新翻出来（完整记录在控制台「对话记录」里）
    if (mode === "plain" && (ga.bubble.pages.length || ga.bubble.error || busy)) ga.bubble.visible = true;
    openComposer();
  }

  // ---------------- 语音预热 ----------------
  // 启用朗读后在后台提前启动语音服务、加载当前角色语音（换皮肤 / 换语音会重新预热），第一次说话就有声音
  $effect(() => {
    if (!enabled() || !app.ready) return;
    const v = outputVoice();
    if (!v) return;
    const id = setTimeout(() => void warmVoice(v), 2500);
    return () => clearTimeout(id);
  });

  // ---------------- 唤醒词 ----------------
  $effect(() => {
    void wk.retry;
    const want = enabled() && wakeReady() && (wk.visible || !!vc.view?.config.wake_background);
    setWake(want, onWake);
  });
  $effect(() => () => setWake(false, onWake));

  function onWake(word: string) {
    if (wk.testing) { if (vc.view?.config.wake_chime) chime(vc.view.config.volume); return; }
    if (conv.active) return;
    const cfg = vc.view?.config;
    if (!cfg || !enabled()) return;
    if (cfg.wake_chime) chime(cfg.volume);
    if (document.visibilityState !== "visible" || !document.hasFocus()) void api.voiceWakeRaise().catch(() => {});
    const quip = wakeQuip(p.name, word, app.locale);
    if (quip && !busy) showBubble(quip);
    if (cfg.wake_action === "open" || !inputEnabled()) { openComposer(); return; }
    // 等提示音放完再开始录，免得把提示音录进去
    setTimeout(() => voiceHotkey(true), cfg.wake_chime ? 260 : 0);
  }

  async function newSession() {
    const ok = await confirmDialog({
      title: t("新建长期会话"),
      message: t("将为助手新开一个长期会话（对话记录和长期记忆会保留，新会话会带上记忆与最近对话回顾）。继续吗？"),
      confirmText: t("新建"),
    });
    if (ok) await resetSession();
  }

  function openSettings() {
    settingsRequest.tab = "assistant";
    app.settingsOpen = true;
  }

  // 链接与本地路径：交给系统打开，不在应用窗口里跳转
  function onMdClick(e: MouseEvent) {
    const a = (e.target as Element).closest("a");
    if (!a) return;
    e.preventDefault();
    const path = a.getAttribute("data-path");
    const href = a.getAttribute("href") ?? "";
    const target = path || (/^https?:\/\//i.test(href) ? href : "");
    if (target) void api.fsOpenDefault(target).catch((err) => toast("error", String(err)));
  }

  // ---------------- 记忆 ----------------
  let listEl = $state<HTMLDivElement | null>(null);
  let memDraft = $state("");
  let editing = $state<{ id: string; content: string } | null>(null);
  let memBusy = $state(false);

  async function addMemory() {
    const c = memDraft.trim();
    if (!c) return;
    try {
      const ok = await api.assistantMemoryAdd(c, false);
      if (!ok) toast("info", t("已有相同的记忆"));
      memDraft = "";
      await loadMemories();
    } catch (e) { toast("error", String(e)); }
  }
  async function saveEdit() {
    if (!editing) return;
    const m = ga.memories.find((x) => x.id === editing!.id);
    try {
      await api.assistantMemoryUpdate(editing.id, editing.content, m?.pinned ?? false);
      editing = null;
      await loadMemories();
    } catch (e) { toast("error", String(e)); }
  }
  async function togglePin(id: string, content: string, pinned: boolean) {
    await api.assistantMemoryUpdate(id, content, !pinned).catch((e) => toast("error", String(e)));
    await loadMemories();
  }
  async function delMemory(id: string) {
    await api.assistantMemoryDelete(id).catch((e) => toast("error", String(e)));
    await loadMemories();
  }
  async function doSummarize(mode: "consolidate" | "extract") {
    if (mode === "consolidate") {
      const ok = await confirmDialog({
        title: t("整理记忆"),
        message: t("让助手合并重复、删除过时的记忆（置顶的记忆不会改动）。整理结果会替换现有的非置顶记忆，继续吗？"),
        confirmText: t("整理"),
      });
      if (!ok) return;
    }
    memBusy = true;
    const n = await summarize(mode);
    memBusy = false;
    if (n !== null) toast("ok", mode === "consolidate" ? t("整理完成：现有 {n} 条记忆", { n }) : n ? t("提炼出 {n} 条新记忆", { n }) : t("没有发现新的值得记住的内容"));
  }
  async function doClear() {
    const ok = await confirmDialog({ title: t("清空对话记录"), message: t("清空助手的全部对话记录？长期记忆不受影响。"), danger: true, confirmText: t("清空") });
    if (ok) await clearMessages().catch((e) => toast("error", String(e)));
  }

  const srcLabel: Record<string, string> = { auto: "自动", manual: "手动", summary: "整理" };
  const fmtTime = (s: string) => s.replace("T", " ").slice(5, 16);

  onMount(() => {
    void initAssistant();
    const offCare = initCare();
    const off = onSkinEvent((ev) => { if (ev === "character.click" && enabled()) { onChip(); focusAfterClick(); } });
    const onResize = () => measure();
    addEventListener("resize", onResize);
    // 双击 Ctrl（两次单独按下松开 Ctrl，中间不按别的键）：语音输入
    let downAt = 0, clean = false, lastTap = 0;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Control") { if (!e.repeat) { downAt = performance.now(); clean = true; } }
      else { clean = false; lastTap = 0; }
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key !== "Control") return;
      const now = performance.now();
      const tap = clean && now - downAt < 350;
      clean = false;
      if (!tap) { lastTap = 0; return; }
      if (now - lastTap < 450) { lastTap = 0; voiceHotkey(); } else lastTap = now;
    };
    const onPointer = () => { clean = false; lastTap = 0; };
    const onBlur = () => { clean = false; lastTap = 0; };
    addEventListener("keydown", onKeyDown, true);
    addEventListener("keyup", onKeyUp, true);
    addEventListener("pointerdown", onPointer, true);
    addEventListener("blur", onBlur);
    // 输入框的 Enter 行为跟随聊天设置（聊天页没打开过时这里自己读一次）
    void api.settingsGet("chat.enter_send").then((v) => (app.enterSend = v === "1")).catch(() => {});
    return () => {
      off();
      offCare();
      endConversation();
      removeEventListener("resize", onResize);
      removeEventListener("keydown", onKeyDown, true);
      removeEventListener("keyup", onKeyUp, true);
      removeEventListener("pointerdown", onPointer, true);
      removeEventListener("blur", onBlur);
    };
  });
</script>

{#snippet micBtn()}
  {#if inputEnabled()}
    <button class="ga-mic" class:rec={vc.recording} class:wait={vc.transcribing} style="--lv:{vc.level}" disabled={vc.transcribing || (busy && !vc.recording)} title={micTitle} aria-label={micTitle} onclick={onMic}>
      <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true"><path fill="currentColor" d="M12 14a3 3 0 0 0 3-3V5a3 3 0 1 0-6 0v6a3 3 0 0 0 3 3Zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11h-2Z"/></svg>
    </button>
  {/if}
{/snippet}

{#snippet stopSpeakBtn()}
  {#if vc.speaking}
    <button class="ga-icon ga-speaking" title={t("停止朗读")} aria-label={t("停止朗读")} onclick={stopTalking}>
      <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path fill="currentColor" d="M4 9v6h4l5 4V5L8 9H4Zm12.5 3a4.5 4.5 0 0 0-2.5-4v8a4.5 4.5 0 0 0 2.5-4Z"/></svg>
    </button>
  {/if}
{/snippet}

{#snippet convChip()}
  {#if conv.active}
    <span class="ga-conv" title={t("连续语音对话：她说完会接着听，聊完会自己结束")}><i></i>{t("连续对话")}</span>
    <button class="ga-btn sm" title={t("结束连续对话")} onclick={endConversation}>{t("结束")}</button>
  {/if}
{/snippet}

{#snippet corners()}
  {#if mode === "amadeus"}<span class="ga-corner tl"></span><span class="ga-corner tr"></span><span class="ga-corner bl"></span><span class="ga-corner br"></span>{/if}
{/snippet}

{#snippet chatPanel()}
  <div class="ga-list" bind:this={listEl}>
    {#each ga.messages as m (m.id)}
      <div class="ga-msg {m.role}">
        <div class="ga-msg-meta">{m.role === "user" ? t("你") : p.name} · {fmtTime(m.created_at)}{m.interrupted ? ` · ${t("已停止")}` : ""}</div>
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="ga-md" onclick={onMdClick}>{@html md(m.content)}</div>
      </div>
    {:else}
      <div class="ga-empty">{t("还没有对话。")}</div>
    {/each}
    {#if busy}
      <div class="ga-msg assistant"><div class="ga-msg-meta">{p.name}</div><div class="ga-md">{#if streamingText || ga.subs.length}{@html md(streamingText || subText(ga.subs))}{:else}<span class="ga-dots"><i></i><i></i><i></i></span>{/if}</div></div>
    {/if}
  </div>
  <div class="ga-tools">
    <button class="ga-btn" disabled={!!ga.busy} onclick={newSession}>{t("新建长期会话")}</button>
    <button class="ga-btn" disabled={!!ga.busy || !ga.messages.length} onclick={doClear}>{t("清空记录")}</button>
    <span class="ga-grow"></span>
    <button class="ga-btn" onclick={openSettings}>{t("设置")}</button>
  </div>
{/snippet}

{#snippet memoryPanel()}
  <div class="ga-list">
    <p class="ga-hint">{t("长期记忆独立于各客户端会话保存，切换客户端或新建会话后仍然有效。")}</p>
    {#each ga.memories as m (m.id)}
      <div class="ga-mem" class:pinned={m.pinned}>
        {#if editing?.id === m.id}
          <textarea class="ga-input" rows="2" bind:value={editing.content} onkeydown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); void saveEdit(); } else if (e.key === "Escape") editing = null; }}></textarea>
          <div class="ga-mem-acts"><button class="ga-btn" onclick={saveEdit}>{t("保存")}</button><button class="ga-btn" onclick={() => (editing = null)}>{t("取消")}</button></div>
        {:else}
          <div class="ga-mem-text">{m.content}</div>
          <div class="ga-mem-acts">
            <span class="ga-mem-src">{t(srcLabel[m.source] ?? m.source)} · {fmtTime(m.updated_at)}</span>
            <span class="ga-grow"></span>
            <button class="ga-icon sm" class:on={m.pinned} title={m.pinned ? t("取消置顶") : t("置顶（总是带上，整理时不改动）")} onclick={() => togglePin(m.id, m.content, m.pinned)}>📌</button>
            <button class="ga-icon sm" title={t("编辑")} onclick={() => (editing = { id: m.id, content: m.content })}>✎</button>
            <button class="ga-icon sm" title={t("删除")} onclick={() => delMemory(m.id)}>✕</button>
          </div>
        {/if}
      </div>
    {:else}
      <div class="ga-empty">{t("还没有记忆。聊天中提到的偏好、约定等会被自动记住，也可以手动添加。")}</div>
    {/each}
  </div>
  <div class="ga-tools">
    <input class="ga-input grow" placeholder={t("手动添加一条记忆…")} bind:value={memDraft} onkeydown={(e) => { if (e.key === "Enter" && !e.isComposing) void addMemory(); }} />
    <button class="ga-btn" disabled={!memDraft.trim()} onclick={addMemory}>{t("添加")}</button>
  </div>
  <div class="ga-tools">
    <button class="ga-btn" disabled={!!ga.busy || memBusy} onclick={() => doSummarize("extract")}>{memBusy && ga.busy === "summarize" ? t("处理中…") : t("从最近对话提炼")}</button>
    <button class="ga-btn" disabled={!!ga.busy || memBusy || ga.memories.filter((m) => !m.pinned).length < 2} onclick={() => doSummarize("consolidate")}>{t("整理记忆")}</button>
  </div>
{/snippet}

{#snippet replyView()}
  {#if busy && !liveText}
    <span class="ga-dots" aria-label={t("思考中")}><i></i><i></i><i></i></span>
    {#if ga.busy === "reset"}<span class="ga-sub">{t("正在新建会话…")}</span>{/if}
  {:else if busy}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="ga-md" onclick={onMdClick}>{@html md(liveText)}<span class="ga-caret"></span></div>
  {:else}
    {#if page}
      {#key ga.bubble.index + ga.bubble.rev * 1000}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="ga-md ga-page" onclick={onMdClick}>{@html md(page)}</div>
      {/key}
    {:else if !ga.bubble.error}
      <div class="ga-sub">{t("想聊点什么？")}</div>
    {/if}
    {#if ga.bubble.more && page && ga.bubble.index >= pageCount - 1}
      {#if mode === "plain"}
        <button class="ga-more" onclick={() => openPanel("chat", true)}>{t("查看完整回复")} ›</button>
      {:else}
        <div class="ga-sub ga-more-hint">{t("完整回复在对话记录里")}</div>
      {/if}
    {/if}
    {#if ga.bubble.error}<div class="ga-error">{ga.bubble.error}</div>{/if}
  {/if}
{/snippet}

{#snippet pager()}
  <button class="ga-icon sm" disabled={ga.bubble.index === 0} onclick={prev} aria-label={t("上一页")}>‹</button>
  <span>{ga.bubble.index + 1} / {pageCount}</span>
  <button class="ga-icon sm" disabled={ga.bubble.index >= pageCount - 1} onclick={next} aria-label={t("下一页")}>›</button>
{/snippet}

{#snippet composeRow()}
  <div class="ga-compose">
    {#if mode === "amadeus"}<span class="ga-prompt">&gt;</span>{/if}
    <textarea class="ga-input grow" rows="1" bind:this={inputEl} bind:value={input} {placeholder} onkeydown={onKey} oninput={onInput}></textarea>
    {@render micBtn()}
    {#if busy}
      <button class="ga-btn" onclick={stopAll}>{mode === "amadeus" ? "[ STOP ]" : t("停止")}</button>
    {:else}
      <button class="ga-btn primary" disabled={!input.trim()} onclick={submit}>{mode === "amadeus" ? "[ SEND ]" : t("发送")}</button>
    {/if}
    <div class="ga-keys" aria-hidden="true">{#each keys as [k, what], i (k)}{#if i}<span class="ga-keys-sep">·</span>{/if}<kbd>{k}</kbd>{what}{/each}</div>
  </div>
{/snippet}

{#if enabled()}
  {#if mode === "plain"}
    <div class="ga-root ga-plain" role="complementary" aria-label={t("全局助手")}>
      <button class="ga-launcher" class:on={cardOpen} class:busy title={t("全局助手")} onclick={onLauncher}>
        <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true"><path fill="currentColor" d="M12 3C6.9 3 3 6.4 3 10.6c0 2.3 1.2 4.4 3.1 5.8L5.4 20l4-2.1c.8.2 1.7.3 2.6.3 5.1 0 9-3.4 9-7.6S17.1 3 12 3Zm-4 8.8a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4Zm4 0a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4Zm4 0a1.2 1.2 0 1 1 0-2.4 1.2 1.2 0 0 1 0 2.4Z"/></svg>
      </button>

      {#if cardOpen}
        <div
          class="ga-card"
          class:wide={ga.panel !== null}
          role="dialog"
          tabindex="-1"
          aria-label={title}
          onmouseenter={() => (hovered = true)}
          onmouseleave={() => (hovered = false)}
        >
          <header class="ga-head">
            <span class="ga-title">{title}</span>
            <span class="ga-grow"></span>
            {@render convChip()}
            {@render stopSpeakBtn()}
            <button class="ga-icon" class:on={ga.panel === "chat"} title={t("对话记录")} onclick={() => openPanel("chat")}>☰</button>
            <button class="ga-icon" class:on={ga.panel === "memory"} title={t("长期记忆")} onclick={() => openPanel("memory")}>✦</button>
            <button class="ga-icon" title={t("关闭")} onclick={close}>✕</button>
          </header>

          {#if ga.panel === "chat"}
            {@render chatPanel()}
          {:else if ga.panel === "memory"}
            {@render memoryPanel()}
          {:else}
            <div class="ga-body" bind:this={bodyEl}>{@render replyView()}</div>
            {#if pageCount > 1 && !busy}<div class="ga-pager">{@render pager()}</div>{/if}
          {/if}

          {#if memoShown}<div class="ga-memo">✦ {memoShown}</div>{/if}

          {#if ga.composer}
            {@render composeRow()}
          {:else}
            <div class="ga-foot">
              {#if busy}<button class="ga-btn" onclick={stopAll}>{t("停止")}</button>{/if}
              <span class="ga-grow"></span>
              {@render micBtn()}
              <button class="ga-btn" onclick={openComposer}>{t("回复")}</button>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {:else}
    <div class="ga-root ga-{mode} ga-staged" role="complementary" aria-label={t("全局助手")}>
      {#if ga.bubble.visible}
        <!-- 说话气泡：人物头部旁的临时提示（鼠标穿透，没有按钮） -->
        <div
          class="ga-say"
          class:subtitle={subtitleOn}
          style="left:{pos.left}px; bottom:{pos.bottom}px; --say-max:{Math.max(140, winH - pos.bottom - 12)}px"
          role="status"
          aria-live="polite"
        >
          {@render corners()}
          <header class="ga-say-head">
            {#if mode === "amadeus"}<span class="ga-led" class:busy class:speak={vc.speaking}></span>{/if}
            <span class="ga-title">{title}</span>
            <span class="ga-grow"></span>
            {#if mode === "amadeus"}<span class="ga-nixie" title={t("世界线变动率")}>{divergence}</span>{/if}
          </header>
          <div class="ga-body" bind:this={bodyEl}>
            {#if subtitleOn}
              {#if vc.subtitle}
                {#key vc.subRev}<div class="ga-subline">{vc.subtitle}</div>{/key}
              {:else}
                <span class="ga-dots" aria-label={t("思考中")}><i></i><i></i><i></i></span>
              {/if}
            {:else}
              {@render replyView()}
            {/if}
          </div>
          {#if !subtitleOn && pageCount > 1 && !busy}
            <div class="ga-say-dots" aria-hidden="true">{#each ga.bubble.pages as _, i (i)}<i class:on={i === ga.bubble.index}></i>{/each}</div>
          {/if}
        </div>
      {/if}

      {#if ga.composer}
        <!-- 控制台：人物脚下 -->
        <div
          class="ga-dock"
          class:faded={dockFaded}
          style="left:{dock.left}px; bottom:{dock.bottom}px; width:{dock.width}px; --panel-max:{dock.panelMax}px"
          role="dialog"
          tabindex="-1"
          aria-label={title}
        >
          {@render corners()}
          {#if ga.panel}
            <section class="ga-panel">
              <header class="ga-panel-head">
                <button class="ga-tab" class:on={ga.panel === "chat"} onclick={() => openPanel("chat", true)}>{t("对话记录")}</button>
                <button class="ga-tab" class:on={ga.panel === "memory"} onclick={() => openPanel("memory", true)}>{t("长期记忆")}</button>
                <span class="ga-grow"></span>
                <button class="ga-icon sm" title={t("收起面板")} aria-label={t("收起面板")} onclick={() => (ga.panel = null)}>▾</button>
              </header>
              {#if ga.panel === "chat"}{@render chatPanel()}{:else}{@render memoryPanel()}{/if}
            </section>
          {/if}
          <div class="ga-bar">
            {#if mode === "amadeus"}<span class="ga-led" class:busy class:speak={vc.speaking}></span>{:else}<span class="ga-chip-dot" class:busy class:speak={vc.speaking}></span>{/if}
            <span class="ga-status" class:live={!!status}>{status || title}</span>
            {#if memoShown}<span class="ga-memo">✦ {memoShown}</span>{/if}
            <span class="ga-grow"></span>
            {@render convChip()}
            {@render stopSpeakBtn()}
            <button class="ga-icon sm" class:on={ga.panel === "chat"} title={t("对话记录")} aria-label={t("对话记录")} onclick={() => openPanel("chat")}>☰</button>
            <button class="ga-icon sm" class:on={ga.panel === "memory"} title={t("长期记忆")} aria-label={t("长期记忆")} onclick={() => openPanel("memory")}>✦</button>
            <button class="ga-icon sm" title={t("关闭（点击人物重新打开）")} aria-label={t("关闭")} onclick={close}>✕</button>
          </div>
          {@render composeRow()}
        </div>
      {/if}
    </div>
  {/if}
{/if}

<style>
  .ga-root {
    position: fixed;
    z-index: 320;
    pointer-events: none;
    font-size: 13px;
  }
  .ga-root > * { pointer-events: auto; }
  .ga-root.ga-plain { left: 14px; bottom: 14px; }

  /* ---------- 通用卡片 ---------- */
  .ga-card {
    position: relative;
    width: 300px;
    max-width: 100%;
    max-height: min(46vh, 420px);
    display: flex;
    flex-direction: column;
    background: var(--bg-elev);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 14px;
    box-shadow: 0 10px 28px #0005;
    overflow: hidden;
    animation: ga-pop .16s ease-out;
  }
  .ga-card.wide { width: 360px; max-height: min(64vh, 560px); }
  .ga-plain .ga-card { position: absolute; left: 0; bottom: 48px; border-radius: 12px; max-width: calc(100vw - 28px); max-height: min(46vh, 420px, calc(100vh - 110px)); }
  .ga-plain .ga-card.wide { max-height: min(64vh, 560px, calc(100vh - 110px)); }
  @keyframes ga-pop { from { opacity: 0; transform: translateY(6px) scale(.98); } to { opacity: 1; transform: none; } }

  .ga-head { display: flex; align-items: center; gap: 6px; padding: 7px 8px 5px 12px; border-bottom: 1px solid var(--border-soft); flex: none; }
  .ga-title { font-weight: 600; font-size: 12.5px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .ga-grow { flex: 1; min-width: 0; }
  .ga-icon {
    border: 0; background: transparent; color: var(--text-dim); cursor: pointer;
    width: 24px; height: 22px; border-radius: 6px; font-size: 12px; line-height: 1; flex: none;
  }
  .ga-icon:hover:not(:disabled), .ga-icon.on { background: var(--accent-soft); color: var(--text); }
  .ga-icon:disabled { opacity: .35; cursor: default; }
  .ga-icon.sm { width: 22px; height: 20px; font-size: 11px; }

  .ga-body { padding: 10px 12px 6px; overflow: auto; min-height: 34px; flex: 1 1 auto; line-height: 1.6; }
  .ga-md :global(p) { margin: 0 0 .45em; }
  .ga-md :global(p:last-child) { margin-bottom: 0; }
  .ga-md :global(pre) { font-size: 12px; max-height: 220px; overflow: auto; background: var(--code-bg); padding: 6px 8px; border-radius: 6px; margin: 4px 0; }
  .ga-md :global(code) { font-family: var(--mono); font-size: .92em; }
  .ga-md :global(ul), .ga-md :global(ol) { margin: 0 0 .4em; padding-left: 1.3em; }
  .ga-md :global(a) { color: var(--accent); }
  .ga-page { animation: ga-fade .2s ease-out; }
  @keyframes ga-fade { from { opacity: 0; } to { opacity: 1; } }
  .ga-sub { color: var(--text-faint); font-size: 12px; }
  .ga-more { display: inline-block; margin-top: 6px; padding: 0; background: none; border: 0; color: var(--accent); font-size: 12px; cursor: pointer; }
  .ga-more:hover { text-decoration: underline; }
  .ga-more-hint { margin-top: 4px; }
  .ga-error { color: var(--danger); font-size: 12px; margin-top: 4px; white-space: pre-wrap; word-break: break-word; }
  .ga-caret { display: inline-block; width: 6px; height: 1em; vertical-align: -2px; margin-left: 2px; background: currentColor; opacity: .6; animation: ga-blink 1s steps(2) infinite; }
  @keyframes ga-blink { 50% { opacity: 0; } }
  .ga-dots { display: inline-flex; gap: 4px; padding: 4px 0; }
  .ga-dots i { width: 6px; height: 6px; border-radius: 50%; background: var(--text-dim); animation: ga-dot 1.1s ease-in-out infinite; }
  .ga-dots i:nth-child(2) { animation-delay: .15s; }
  .ga-dots i:nth-child(3) { animation-delay: .3s; }
  @keyframes ga-dot { 0%, 80%, 100% { opacity: .25; transform: none; } 40% { opacity: 1; transform: translateY(-2px); } }

  .ga-pager { display: flex; align-items: center; justify-content: center; gap: 6px; font-size: 11px; color: var(--text-faint); padding: 0 0 2px; flex: none; }
  .ga-foot, .ga-tools { display: flex; align-items: center; gap: 6px; padding: 4px 8px 7px; flex: none; }
  .ga-tools { border-top: 1px solid var(--border-soft); padding-top: 6px; }
  .ga-tools + .ga-tools { border-top: 0; padding-top: 0; }
  .ga-btn {
    border: 1px solid var(--border); background: var(--bg-elev2); color: var(--text);
    border-radius: 7px; padding: 2px 9px; font-size: 12px; cursor: pointer; white-space: nowrap; height: 24px;
  }
  .ga-btn:hover:not(:disabled) { border-color: var(--accent); }
  .ga-btn:disabled { opacity: .45; cursor: default; }
  .ga-btn.primary { background: var(--accent); color: var(--accent-contrast); border-color: var(--accent); }
  .ga-compose { display: flex; align-items: flex-end; gap: 6px; padding: 6px 8px 8px; border-top: 1px solid var(--border-soft); flex: none; }
  .ga-input {
    border: 1px solid var(--border); background: var(--bg); color: var(--text); border-radius: 8px;
    padding: 4px 8px; font: inherit; font-size: 12.5px; resize: none; min-width: 0; max-height: 96px; field-sizing: content;
  }
  .ga-input:focus { outline: none; border-color: var(--accent); }
  .ga-compose { flex-wrap: wrap; row-gap: 4px; }
  .ga-keys { flex: 1 0 100%; display: flex; align-items: center; gap: 3px; font-size: 10.5px; line-height: 1; color: var(--text-faint); padding-left: 2px; white-space: nowrap; overflow: hidden; opacity: .8; }
  .ga-keys kbd { font: inherit; font-size: 10px; padding: 1px 4px; border: 1px solid var(--border); border-bottom-width: 2px; border-radius: 4px; background: var(--bg-elev2, transparent); color: var(--text-dim); margin-right: 2px; }
  .ga-keys-sep { margin: 0 3px; opacity: .6; }
  .ga-conv { display: inline-flex; align-items: center; gap: 4px; font-size: 11px; color: var(--accent); white-space: nowrap; }
  .ga-conv i { width: 6px; height: 6px; border-radius: 50%; background: currentColor; animation: ga-conv-pulse 1.4s ease-in-out infinite; }
  @keyframes ga-conv-pulse { 0%, 100% { opacity: .35; } 50% { opacity: 1; } }
  .ga-root .ga-btn.sm { padding: 1px 7px; font-size: 11px; }
  .grow { flex: 1; }
  .ga-memo { font-size: 11px; color: var(--accent); padding: 0 12px 4px; flex: none; }

  .ga-list { overflow: auto; padding: 8px 10px; flex: 1 1 auto; min-height: 120px; display: flex; flex-direction: column; gap: 8px; }
  .ga-msg { max-width: 92%; }
  .ga-msg.user { align-self: flex-end; }
  .ga-msg .ga-md { padding: 5px 9px; border-radius: 10px; background: var(--bg-elev2); line-height: 1.55; word-break: break-word; }
  .ga-msg.user .ga-md { background: var(--accent-soft); }
  .ga-msg-meta { font-size: 10.5px; color: var(--text-faint); margin: 0 2px 2px; }
  .ga-msg.user .ga-msg-meta { text-align: right; }
  .ga-empty, .ga-hint { color: var(--text-faint); font-size: 12px; margin: 0; line-height: 1.5; }
  .ga-mem { border: 1px solid var(--border-soft); border-radius: 8px; padding: 6px 8px; background: var(--bg-elev2); }
  .ga-mem.pinned { border-color: var(--accent); }
  .ga-mem-text { line-height: 1.5; word-break: break-word; }
  .ga-mem-acts { display: flex; align-items: center; gap: 4px; margin-top: 3px; }
  .ga-mem-src { font-size: 10.5px; color: var(--text-faint); }
  .ga-mem textarea { width: 100%; box-sizing: border-box; }

  /* ---------- 简易助手（无皮肤） ---------- */
  .ga-launcher {
    width: 38px; height: 38px; border-radius: 50%; border: 1px solid var(--border);
    background: var(--bg-elev); color: var(--accent); cursor: pointer; display: grid; place-items: center;
    box-shadow: 0 4px 14px #0003; opacity: .82; transition: opacity .15s, transform .15s;
  }
  .ga-launcher:hover, .ga-launcher.on { opacity: 1; transform: translateY(-1px); }
  .ga-launcher.busy { animation: ga-pulse 1.4s ease-in-out infinite; }
  @keyframes ga-pulse { 50% { box-shadow: 0 0 0 5px var(--accent-soft); } }

  .ga-chip-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex: none; }

  .ga-mic {
    flex: none; width: 28px; height: 28px; display: inline-flex; align-items: center; justify-content: center;
    border: 1px solid var(--border); border-radius: 50%; background: transparent; color: var(--text-dim); cursor: pointer; position: relative;
  }
  .ga-mic:hover:not(:disabled) { color: var(--text); border-color: var(--accent); }
  .ga-mic:disabled { opacity: .5; cursor: default; }
  .ga-mic.rec { color: #fff; background: #e5484d; border-color: #e5484d; box-shadow: 0 0 0 calc(2px + var(--lv, 0) * 7px) #e5484d40; }
  .ga-mic.wait { animation: ga-blink .9s steps(2) infinite; }
  .ga-speaking { color: var(--accent); }
  .ga-amadeus .ga-mic { border-radius: 0; border-color: var(--am-line-2); color: var(--am-dim); }
  .ga-amadeus .ga-mic:hover:not(:disabled) { color: var(--am); border-color: #f0b42999; }
  .ga-amadeus .ga-mic.rec { color: #1a1408; background: var(--am); border-color: var(--am); box-shadow: 0 0 0 calc(1px + var(--lv, 0) * 6px) #f0b42940; }
  .ga-amadeus .ga-speaking { color: var(--am); }

  /* ---------- 终端风格（牧濑红莉栖·动态；style id 沿用 amadeus） ----------
     底色取皮肤自己的深灰（--bg-elev / --bg），亮黄只做点缀：指示灯、辉光管时钟、提示符、四角、主按钮与焦点。 */
  .ga-amadeus {
    --am: #f0b429;
    --am-soft: #f0b42922;
    --am-line: #ffffff1c;
    --am-line-2: #ffffff2e;
    --am-text: #dcdde0;
    --am-dim: #9a9ca3;
    --am-bg: color-mix(in srgb, var(--bg-elev, #1c1d21) 90%, transparent);
    --am-bg-2: color-mix(in srgb, var(--bg, #141518) 70%, transparent);
    font-family: "Cascadia Mono", Consolas, "Courier New", monospace;
  }
  .ga-corner { position: absolute; width: 9px; height: 9px; border: 0 solid var(--am); pointer-events: none; z-index: 1; opacity: .9; }
  .ga-corner.tl { left: 3px; top: 3px; border-left-width: 2px; border-top-width: 2px; }
  .ga-corner.tr { right: 3px; top: 3px; border-right-width: 2px; border-top-width: 2px; }
  .ga-corner.bl { left: 3px; bottom: 3px; border-left-width: 2px; border-bottom-width: 2px; }
  .ga-corner.br { right: 3px; bottom: 3px; border-right-width: 2px; border-bottom-width: 2px; }
  .ga-nixie { flex: none; }
  .ga-nixie {
    font-size: 11.5px; letter-spacing: .1em; color: #ffb63d;
    text-shadow: 0 0 4px #ff8a0099, 0 0 8px #ff6a0044; font-variant-numeric: tabular-nums;
  }
  .ga-led { width: 7px; height: 7px; border-radius: 50%; background: #4a4c52; box-shadow: inset 0 0 2px #000; flex: none; }
  .ga-led.busy { background: var(--am, #f0b429); box-shadow: 0 0 6px #ffa000; animation: ga-blink .8s steps(2) infinite; }
  .ga-led.speak { background: var(--am, #f0b429); box-shadow: 0 0 6px #ffa000; }
  .ga-amadeus .ga-icon { color: var(--am-dim); border-radius: 0; }
  .ga-amadeus .ga-icon:hover:not(:disabled), .ga-amadeus .ga-icon.on { background: #ffffff10; color: var(--am); }
  .ga-amadeus .ga-body { line-height: 1.65; }
  .ga-amadeus .ga-md :global(a) { color: var(--am); }
  .ga-amadeus .ga-md :global(pre) { background: #0006; border: 1px solid var(--am-line); border-radius: 0; }
  .ga-amadeus .ga-md :global(code) { color: #e9e3d2; }
  .ga-amadeus .ga-sub, .ga-amadeus .ga-pager, .ga-amadeus .ga-msg-meta, .ga-amadeus .ga-mem-src, .ga-amadeus .ga-empty, .ga-amadeus .ga-hint { color: var(--am-dim); }
  .ga-amadeus .ga-dots i { background: var(--am); }
  .ga-amadeus .ga-caret { background: var(--am); opacity: .85; }
  .ga-amadeus .ga-btn {
    background: transparent; color: var(--am-text); border: 1px solid var(--am-line-2); border-radius: 0;
    font-family: inherit; letter-spacing: .04em;
  }
  .ga-amadeus .ga-btn:hover:not(:disabled) { background: #ffffff0d; color: #fff; border-color: #ffffff55; }
  .ga-amadeus .ga-btn.primary { background: var(--am-soft); color: var(--am); border-color: #f0b42999; }
  .ga-amadeus .ga-btn.primary:hover:not(:disabled) { background: #f0b42933; color: #ffd36b; border-color: var(--am); }
  .ga-amadeus .ga-compose { border-top: 1px dashed var(--am-line-2); align-items: center; }
  .ga-prompt { color: var(--am); font-weight: 700; text-shadow: 0 0 4px #ff9d0088; }
  .ga-amadeus .ga-input { background: #0005; color: var(--am-text); border: 1px solid var(--am-line-2); border-radius: 0; font-family: inherit; caret-color: var(--am); }
  .ga-amadeus .ga-input:focus { border-color: #f0b429aa; box-shadow: 0 0 0 1px #f0b42933; }
  .ga-amadeus .ga-input::placeholder { color: #8b8d9488; }
  .ga-amadeus .ga-keys { color: var(--am-dim); padding-left: 16px; }
  .ga-amadeus .ga-keys kbd { border-color: var(--am-line-2); border-radius: 0; background: #0004; color: var(--am-text); }
  .ga-amadeus .ga-conv { color: var(--am); text-shadow: 0 0 4px #ff9d0066; }
  .ga-amadeus .ga-tools { border-top-color: var(--am-line); }
  .ga-amadeus .ga-msg .ga-md { background: #ffffff08; border: 1px solid var(--am-line); border-radius: 0; }
  .ga-amadeus .ga-msg.user .ga-md { background: #ffffff10; border-color: var(--am-line-2); border-left: 2px solid #f0b429aa; }
  .ga-amadeus .ga-mem { background: #ffffff06; border-color: var(--am-line); border-radius: 0; }
  .ga-amadeus .ga-mem.pinned { border-left: 2px solid var(--am); }
  .ga-amadeus .ga-memo { color: var(--am); }
  .ga-amadeus .ga-error { color: #ff7a6b; }
  /* ---------- 人物模式：说话气泡 + 脚下控制台 ---------- */
  .ga-staged { position: fixed; left: 0; bottom: 0; width: 0; height: 0; max-width: none; }
  .ga-say {
    position: fixed;
    width: 280px;
    max-width: calc(100vw - 24px);
    max-height: min(40vh, 320px, var(--say-max, 320px));
    display: flex; flex-direction: column;
    background: var(--bg-elev); color: var(--text);
    border: 1px solid var(--border);
    border-radius: 14px 14px 14px 4px;
    box-shadow: 0 10px 28px #0005;
    overflow: hidden;
    animation: ga-pop .16s ease-out;
  }
  .ga-say-head { display: flex; align-items: center; gap: 6px; padding: 6px 10px 0 12px; flex: none; }
  .ga-say .ga-title { font-size: 11.5px; color: var(--text-dim); font-weight: 600; }
  .ga-say .ga-body { padding: 4px 12px 9px; }
  .ga-root .ga-say { pointer-events: none; user-select: none; }
  .ga-say .ga-body { overflow: hidden; }
  .ga-say-dots { display: flex; justify-content: center; gap: 4px; padding: 2px 0 7px; flex: none; }
  .ga-say-dots i { width: 5px; height: 5px; border-radius: 50%; background: var(--text-faint); opacity: .45; }
  .ga-say-dots i.on { background: var(--accent); opacity: 1; }
  .ga-subline { font-size: 13.5px; line-height: 1.65; white-space: pre-wrap; word-break: break-word; animation: ga-sub .22s ease-out; }
  @keyframes ga-sub { from { opacity: 0; transform: translateY(3px); } to { opacity: 1; transform: none; } }

  .ga-dock {
    position: fixed;
    max-width: calc(100vw - 16px);
    display: flex; flex-direction: column;
    background: color-mix(in srgb, var(--bg-elev) 94%, transparent);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: 0 8px 24px #0005;
    backdrop-filter: blur(6px);
    overflow: hidden;
    transition: opacity .2s;
    animation: ga-pop .16s ease-out;
  }
  .ga-dock.faded:not(:hover):not(:focus-within) { opacity: .18; }
  .ga-panel { display: flex; flex-direction: column; max-height: var(--panel-max); min-height: 0; border-bottom: 1px solid var(--border-soft); }
  .ga-panel .ga-list { min-height: 80px; }
  .ga-panel-head { display: flex; align-items: center; gap: 2px; padding: 5px 6px 3px 8px; flex: none; border-bottom: 1px solid var(--border-soft); }
  .ga-tab { border: 0; background: transparent; color: var(--text-dim); font-size: 12px; padding: 2px 8px; border-radius: 6px; cursor: pointer; }
  .ga-tab:hover, .ga-tab.on { color: var(--text); background: var(--accent-soft); }
  .ga-bar { display: flex; align-items: center; gap: 4px; padding: 5px 6px 0 10px; flex: none; min-width: 0; }
  .ga-status { font-size: 11.5px; color: var(--text-faint); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .ga-status.live { color: var(--accent); }
  .ga-bar .ga-memo { padding: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .ga-dock .ga-compose { border-top: 0; padding-top: 4px; }
  .ga-chip-dot.speak { box-shadow: 0 0 0 3px var(--accent-soft); }
  .ga-chip-dot.busy { animation: ga-blink .9s steps(2) infinite; }

  .ga-amadeus .ga-say, .ga-amadeus .ga-dock {
    background:
      repeating-linear-gradient(0deg, #ffffff04 0 1px, transparent 1px 3px),
      var(--am-bg);
    color: var(--am-text);
    border: 1px solid var(--am-line-2);
    border-radius: 2px;
    box-shadow: 0 0 0 1px #0008, 0 12px 30px #0009;
    backdrop-filter: blur(8px);
  }
  .ga-amadeus .ga-say { width: 330px; }
  .ga-amadeus .ga-say-head { padding: 7px 18px 4px 12px; border-bottom: 1px solid var(--am-line); background: var(--am-bg-2); }
  .ga-amadeus .ga-say .ga-title { color: var(--am-text); letter-spacing: .06em; font-weight: 600; }
  .ga-amadeus .ga-say .ga-body { padding-top: 7px; }
  .ga-amadeus .ga-status { color: var(--am-dim); }
  .ga-amadeus .ga-say-dots i { border-radius: 0; background: var(--am-dim); }
  .ga-amadeus .ga-say-dots i.on { background: var(--am); }
  .ga-amadeus .ga-status.live { color: var(--am); }
  .ga-amadeus .ga-subline { color: #ece7da; }
  .ga-amadeus .ga-panel, .ga-amadeus .ga-panel-head { border-color: var(--am-line); }
  .ga-amadeus .ga-tab { color: var(--am-dim); border-radius: 0; font-family: inherit; letter-spacing: .04em; }
  .ga-amadeus .ga-tab:hover, .ga-amadeus .ga-tab.on { color: var(--am); background: #ffffff10; }
  .ga-amadeus .ga-dock .ga-compose { border-top: 0; }
  @media (prefers-reduced-motion: reduce) {
    .ga-card, .ga-page, .ga-say, .ga-dock, .ga-subline { animation: none; }
  }
</style>
