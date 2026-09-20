<script lang="ts">
  import { onMount } from "svelte";
  import { app, chatKey, currentContext, finishTurn, setChatRows, pushLocal, toast, clearChat, sharedContextPrompt, switchAgent } from "../state.svelte";
import { confirmDialog, promptDialog } from "../dialog.svelte";
  import { api } from "../ipc";
  import MessageItem from "./MessageItem.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import ChatTimeline from "./ChatTimeline.svelte";
  import type { AgentType, SessionReadyInfo } from "../types";

  let input = $state("");
  let sending = $state(false);
  let listEl: HTMLDivElement | undefined = $state();
  let msgsInnerEl: HTMLDivElement | undefined = $state();
  let stickToBottom = $state(true);
  /** queued image attachments (base64 + data-url preview) */
  let pendingImages = $state<{ data: string; mime: string; preview: string }[]>([]);

  const ctx = $derived(currentContext());
  const key = $derived(chatKey(ctx?.id ?? null, app.agent));
  const items = $derived(app.chat[key] ?? []);
  const sessionId = $derived(app.bindingSession[key] ?? null);
  let bindingTitle = $state<string | null>(null);

  const sessionInfo = $derived(app.sessionInfo[key] ?? null);
  const models = $derived(sessionInfo?.response?.models?.availableModels ?? []);
  const currentModel = $derived(sessionInfo?.response?.models?.currentModelId ?? sessionInfo?.response?.models?.currentModel ?? "");
  const status = $derived(app.agentStatus[app.agent]);
  const streaming = $derived(app.streaming[key] ?? false);
  const loadingHere = $derived(app.chatLoading[key] ?? false);

  // 从绑定的 ACP 会话重放历史（不读本地库）。适配器冷启动 + session/load
  // 可能要几秒，期间显示忙碌动画。
  async function refreshFromAdapter(ctxId: string, agent: AgentType, sessionId: string, title?: string) {
    const k = chatKey(ctxId, agent);
    if (app.streaming[k]) return;
    app.chatLoading[k] = true;
    try {
      const rows = await api.acpSessionBind(ctx!, agent, sessionId, title, true);
      // 用户可能已切走：只把结果写回仍在查看的会话
      if (app.contextId === ctxId && app.agent === agent) {
        // 空结果说明该会话已在本连接加载过（内存即最新），不要清空现有内容
        if (rows.length || !app.chat[k]?.length) setChatRows(k, rows);
      }
    } catch {
      // 静默刷新失败不打扰（未连接适配器/加载失败时界面保持空白态）
    } finally {
      app.chatLoading[k] = false;
    }
  }

  // seed the bound session id whenever context or agent changes
  $effect(() => {
    const ctxId = ctx?.id;
    const agent = app.agent;
    if (!ctxId) return;
    stickToBottom = true; // 切换会话后回到贴底状态
    void (async () => {
      try {
        const b = await api.bindingGet(ctxId, agent);
        const k = chatKey(ctxId, agent);
        app.bindingSession[k] = b?.session_id ?? null;
        app.bindingTitleMap[k] = b?.title ?? null;
        bindingTitle = b?.title ?? null;
        if (b?.session_id && ctx && ctx.id === ctxId) {
          await refreshFromAdapter(ctxId, agent, b.session_id, b.title ?? undefined);
        }
      } catch {
        app.chatLoading[chatKey(ctxId, agent)] = false;
      }
    })();
  });

  // follow the bottom: on session switch AND after every history load/clear
  // (chatRev bump) force-anchor to the latest message; while streaming, every
  // appended chunk/text growth re-anchors the view.
  let lastKeyScrolled = "";
  let lastRevSeen: Record<string, number> = {};
  $effect(() => {
    const k = key;
    const n = items.length;
    const tailLen = items[n - 1]?.text.length ?? 0;
    const rev = app.chatRev[k] ?? 0;
    if (k !== lastKeyScrolled) {
      lastKeyScrolled = k;
      lastRevSeen[k] = rev;
      stickToBottom = true;
    } else if (rev !== (lastRevSeen[k] ?? 0)) {
      // 历史加载完成（或清空重开）：无条件定位到底部
      lastRevSeen[k] = rev;
      stickToBottom = true;
    }
    void n;
    void tailLen;
    if (!stickToBottom || !listEl) return;
    scrollToBottom();
    // markdown/字体/图片异步撑高后再校准两次
    requestAnimationFrame(() => {
      if (stickToBottom && listEl) scrollToBottom();
      requestAnimationFrame(() => {
        if (stickToBottom && listEl) scrollToBottom();
      });
    });
  });

  // 程序滚动时间戳：浏览器在内容撑高时会做「滚动锚定」调整并触发 scroll 事件，
  // 该事件不是用户操作，不能据此取消贴底。
  let lastProgScrollAt = 0;
  let prevScrollTop = 0;
  function scrollToBottom() {
    if (!listEl) return;
    lastProgScrollAt = performance.now();
    prevScrollTop = listEl.scrollHeight; // 程序置底后的基准
    listEl.scrollTop = listEl.scrollHeight;
  }
  function onScroll() {
    if (!listEl) return;
    const now = performance.now();
    // 1) 我们自己的程序滚动 echo（含 rAF 校准）一律忽略
    if (now - lastProgScrollAt < 300) return;
    // 2) 内容撑高/锚定只会让 scrollTop 增大或不变；只有用户向上滚才会让 scrollTop 减小
    const scrolledUp = listEl.scrollTop < prevScrollTop - 2;
    prevScrollTop = listEl.scrollTop;
    if (scrolledUp) {
      stickToBottom = false;
      return;
    }
    const dist = listEl.scrollHeight - listEl.scrollTop - listEl.clientHeight;
    if (dist < 60) stickToBottom = true;
  }

  // 内容高度一变（历史重放渲染完成、流式追加、图片/字体加载）即贴底：
  // Markdown 渲染晚于滚动赋值时，仅靠 effect/rAF 会停在半空。
  onMount(() => {
    const ro = new ResizeObserver(() => {
      if (stickToBottom && listEl) scrollToBottom();
    });
    if (msgsInnerEl) ro.observe(msgsInnerEl);
    return () => ro.disconnect();
  });

  async function newSession() {
    if (!ctx) return;
    if (streaming) {
      toast("warn", "会话正在进行中，请先停止");
      return;
    }
    if (!(await confirmDialog({ title: "新建会话", message: "创建新会话？当前绑定的 AI 会话将被解绑（AI 端历史仍保留）。" }))) return;
    try {
      await api.bindingUnbind(ctx.id, app.agent);
      clearChat(key);
      const sid = await api.acpSessionNew(ctx, app.agent);
      app.bindingSession[key] = sid;
      bindingTitle = "新会话";
      stickToBottom = true;
      toast("ok", `已创建新会话（${app.agent === "codex" ? "Codex" : "ZCode"}）`);
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function resumeSession() {
    if (!ctx) return;
    try {
      const sid = await api.acpSessionNew(ctx, app.agent);
      app.bindingSession[key] = sid;
      toast("ok", "已恢复绑定的会话");
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function send() {
    if (!ctx || sending) return;
    const text = input.trim();
    if (!text && !pendingImages.length) return;
    const imgs = pendingImages.map((p) => ({ data: p.data, mime: p.mime }));
    input = "";
    pendingImages = [];
    stickToBottom = true;
    pushLocal(key, { kind: "user", text: text + (imgs.length ? `\n\n[图片 ×${imgs.length}]` : "") });
    sending = true;
    app.streaming[key] = true;
    try {
      const res = await api.acpPrompt(ctx, app.agent, text, imgs);
      const stop = res?.stopReason ?? "end_turn";
      if (stop !== "end_turn") toast("info", `回合结束（${stop}）`);
    } catch (e) {
      pushLocal(key, { kind: "error", text: String(e) });
    } finally {
      finishTurn(app.agent, ctx.id);
      sending = false;
      // refresh binding (session id may have just been created)
      api
        .bindingGet(ctx.id, app.agent)
        .then((b) => {
          app.bindingSession[key] = b?.session_id ?? null;
          bindingTitle = b?.title ?? null;
        })
        .catch(() => {});
    }
  }

  async function stop() {
    if (!ctx) return;
    try {
      await api.acpCancel(ctx.id, app.agent);
      toast("info", "已发送取消请求");
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function setModel(modelId: string) {
    if (!ctx) return;
    try {
      await api.acpSetConfigOption(ctx.id, app.agent, "model", modelId);
      toast("ok", "模型已切换");
    } catch (e) {
      toast("error", String(e));
    }
  }

  // 通用会话配置条：codex models + zcode configOptions(select)
  type CfgOpt = { value: string; name: string };
  type CfgItem = { id: string; label: string; value: string; options: CfgOpt[] };

  // 适配器给出的 id/name 多为英文，这里映射为中文；未收录的原样展示
  const OPT_LABELS: Record<string, string> = {
    model: "模型",
    mode: "模式",
    thinking: "思考等级",
    thought: "思考等级",
    "thought level": "思考等级",
    thought_level: "思考等级",
    reasoning: "思考等级",
    "reasoning_effort": "思考等级",
    "reasoning-effort": "思考等级",
    effort: "思考等级",
    verbosity: "回答详细度",
    approval: "审批模式",
    approval_policy: "审批模式",
    sandbox: "沙箱模式",
    sandbox_mode: "沙箱模式",
    "collaboration-mode": "协作模式",
    collaboration_mode: "协作模式",
  };
  const OPT_VALUES: Record<string, string> = {
    // zcode 模式（CONFIG_META.mode）
    plan: "规划",
    build: "构建",
    edit: "编辑",
    yolo: "自动执行",
    auto: "自动",
    // 思考等级（GLM low/high/max 等）
    off: "关闭",
    none: "无",
    minimal: "极简",
    low: "低",
    medium: "中",
    high: "高",
    xhigh: "极高",
    max: "最高",
    // codex 侧常见值
    concise: "简洁",
    default: "默认",
    detailed: "详细",
    never: "从不",
    all: "全部",
    "on-request": "按需",
    "on-failure": "失败时",
    untrusted: "不可信时确认",
    "read-only": "只读",
    agent: "自动批准",
    "agent-full-access": "完全访问",
    "workspace-write": "工作区可写",
    "danger-full-access": "完全访问",
  };
  const MODE_NAMES: Record<string, string> = {
    // zcode 会话模式
    plan: "规划",
    build: "构建",
    edit: "编辑",
    yolo: "自动执行",
    auto: "自动",
    // codex 会话模式（id）
    "read-only": "只读",
    agent: "自动批准",
    "agent-full-access": "完全访问",
    write: "编辑",
    ask: "询问",
    execute: "执行",
    code: "编码",
    architect: "架构",
    "accept-edits": "自动接受编辑",
    bypasspermissions: "跳过确认",
    default: "默认",
  };
  // codex 等适配器直接给英文模式名（无独立 id 时）按 name 匹配
  const MODE_NAME_TEXT: Record<string, string> = {
    "approve for me": "自动批准",
    "ask for approval": "询问批准",
    "full access": "完全访问",
    "agent (full access)": "Agent 执行（完全访问）",
    "read only": "只读",
  };
  const zhMode = (name: string, id?: string) =>
    MODE_NAMES[(id ?? "").toLowerCase()] ?? MODE_NAMES[name.toLowerCase()] ?? MODE_NAME_TEXT[name.toLowerCase()] ?? name;
  const zhLabel = (id: string | undefined, fallback: string) =>
    (id ? OPT_LABELS[id.toLowerCase()] : undefined) ?? OPT_LABELS[fallback.toLowerCase()] ?? fallback;
  const zhValue = (v: string) => OPT_VALUES[v.toLowerCase()] ?? v;

  const caps = $derived.by(() => {
    const c = app.agentCaps[app.agent];
    if (c) return c as { models?: SessionReadyInfo["response"]["models"]; configOptions?: any[] };
    return {} as { models?: SessionReadyInfo["response"]["models"]; configOptions?: any[] };
  });

  const liveModels = $derived(sessionInfo?.response?.models ?? caps.models ?? null);

  const cfgItems = $derived.by((): CfgItem[] => {
    const out: CfgItem[] = [];
    const models = liveModels;
    if (models?.availableModels?.length) {
      const cur = models.currentModelId ?? models.currentModel ?? "";
      out.push({
        id: "model",
        label: "模型",
        value: cur,
        options: models.availableModels.map((m) => ({ value: m.modelId, name: m.name })),
      });
    }
    const cfgOpts: any[] = sessionInfo?.response?.configOptions ?? caps.configOptions ?? [];
    for (const o of cfgOpts) {
      if (o.type && o.type !== "select") continue;
      if (out.some((x) => x.id === o.id)) continue;
      const opts = (o.options ?? []).map((x: any) => {
        const v = String(x.value);
        const zh = zhValue(v);
        // 值有中文映射用映射；否则尝试按英文名映射；都没有保留原名
        const name = zh !== v ? zh : (MODE_NAME_TEXT[String(x.name ?? "").toLowerCase()] ?? x.name);
        return { value: v, name };
      });
      if (!opts.length) continue;
      out.push({ id: o.id, label: zhLabel(o.id, o.name ?? o.id), value: String(o.currentValue ?? ""), options: opts });
    }
    return out;
  });

  function setCfg(id: string, value: string) {
    // 更新本地显示（无论是否已有会话）
    if (id === "model" && sessionInfo?.response?.models) sessionInfo.response.models.currentModelId = value;
    if (sessionInfo?.response?.configOptions) {
      const opt = sessionInfo.response.configOptions.find((o) => o.id === id);
      if (opt) opt.currentValue = value;
    }
    if (!sessionId) {
      // 会话尚未创建：暂存，session-ready 后自动应用
      app.pendingCfg[app.agent] = { id, value };
      toast("info", "已记录选择，创建会话后自动应用");
      return;
    }
    if (!ctx) return;
    void api
      .acpSetConfigOption(ctx.id, app.agent, id, value)
      .then(() => toast("ok", "已应用"))
      .catch((e) => toast("error", String(e)));
  }

  // 旧模型下线时自动迁移到可用列表的第一个
  $effect(() => {
    if (!models.length || !ctx) return;
    if (currentModel && models.some((m) => m.modelId === currentModel)) return;
    const target = models[0]?.modelId;
    if (target && currentModel) {
      // 仅在已设置过模型时迁移；未设置时交由适配器默认
      void api
        .acpSetConfigOption(ctx.id, app.agent, "model", target)
        .then(() => {
          if (sessionInfo?.response?.models) sessionInfo.response.models.currentModelId = target;
          toast("info", `原模型已下线，已切换为 ${models[0]?.name ?? target}`);
        })
        .catch(() => {});
    }
  });

  // ---------- Ctrl+F 正文搜索 ----------
  let searchOpen = $state(false);
  let searchQuery = $state("");
  let searchHits = $state<{ id: string; text: string }[]>([]);
  let searchIdx = $state(-1);

  function onPageKeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
      e.preventDefault();
      openSearch();
    }
  }

  $effect(() => {
    window.addEventListener("keydown", onPageKeydown);
    return () => window.removeEventListener("keydown", onPageKeydown);
  });

  function openSearch() {
    searchOpen = true;
    stickToBottom = false;
    setTimeout(() => (document.querySelector(".chat-search input") as HTMLInputElement | null)?.focus(), 30);
  }

  function runSearch() {
    const q = searchQuery.trim().toLowerCase();
    if (!q) {
      searchHits = [];
      searchIdx = -1;
      return;
    }
    searchHits = items
      .filter((it) => (it.kind === "user" || it.kind === "assistant" || it.kind === "error") && it.text.toLowerCase().includes(q))
      .map((it) => ({ id: it.id, text: it.text }));
    searchIdx = searchHits.length ? searchIdx >= 0 && searchIdx < searchHits.length ? searchIdx : 0 : -1;
    jumpToHit();
  }

  function stepSearch(dir: 1 | -1) {
    if (!searchHits.length) return;
    searchIdx = (searchIdx + dir + searchHits.length) % searchHits.length;
    jumpToHit();
  }

  function jumpToHit() {
    const hit = searchHits[searchIdx];
    if (!hit) return;
    const el = document.getElementById("msg-" + hit.id);
    el?.scrollIntoView({ block: "center", behavior: "smooth" });
    el?.classList.remove("flash");
    void el?.offsetWidth;
    el?.classList.add("flash");
  }

  function closeSearch() {
    searchOpen = false;
    searchQuery = "";
    searchHits = [];
    searchIdx = -1;
  }

  // ---------- 输入框右键粘贴 ----------
  let pasteMenu = $state<{ x: number; y: number } | null>(null);
  function onInputContext(e: MouseEvent) {
    e.preventDefault();
    pasteMenu = { x: e.clientX, y: e.clientY };
  }
  async function pasteIntoInput() {
    pasteMenu = null;
    try {
      const text = await api.clipboardReadText();
      if (!text) return;
      const ta = document.querySelector(".input-row textarea") as HTMLTextAreaElement | null;
      if (!ta) {
        input = (input ? input + "\n" : "") + text;
        return;
      }
      const start = ta.selectionStart ?? input.length;
      const end = ta.selectionEnd ?? input.length;
      input = input.slice(0, start) + text + input.slice(end);
      setTimeout(() => {
        ta.focus();
        const pos = start + text.length;
        ta.setSelectionRange(pos, pos);
      }, 0);
    } catch (e) {
      toast("error", String(e));
    }
  }

  // ---------- 发送快捷键（默认 Enter 换行 / Ctrl+Enter 发送，设置可切换） ----------
  $effect(() => {
    void api
      .settingsGet("chat.enter_send")
      .then((v) => (app.enterSend = v === "1"))
      .catch(() => {});
  });

  // ---------- 输入框高度拖拽 ----------
  let composerH = $state(Number(localStorage.getItem("shidrive.composer.h")) || 0);

  function startResize(e: PointerEvent) {
    e.preventDefault();
    const startY = e.clientY;
    const ta = document.querySelector(".input-row textarea") as HTMLElement | null;
    const startH = composerH || ta?.offsetHeight || 84;
    const move = (ev: PointerEvent) => {
      composerH = Math.min(460, Math.max(52, Math.round(startH - (ev.clientY - startY))));
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      localStorage.setItem("shidrive.composer.h", String(composerH));
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  function onSearchKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      stepSearch(e.shiftKey ? -1 : 1);
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeSearch();
      stickToBottom = true;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
      e.preventDefault();
      openSearch();
      return;
    }
    if (e.key !== "Enter" || e.isComposing) return;
    // 默认：Enter 换行、Ctrl+Enter 发送；可在设置切换为 Enter 发送 / Shift+Enter 换行
    const enterSend = app.enterSend;
    if (enterSend) {
      if (!e.shiftKey && !e.ctrlKey && !e.metaKey) {
        e.preventDefault();
        void send();
      }
    } else if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      void send();
    }
  }

  // ---------- 图片粘贴 ----------
  // ACP prompt 支持图片内容块：codex-acp 转 data-url，zcode-acp-server
  // 走 base64 附件，两侧均支持，粘贴后随文本一起发送。
  function onPaste(e: ClipboardEvent) {
    const files = Array.from(e.clipboardData?.files ?? []).filter((f) => f.type.startsWith("image/"));
    if (!files.length) return;
    e.preventDefault();
    for (const f of files.slice(0, 4)) {
      const rd = new FileReader();
      rd.onload = () => {
        const url = String(rd.result ?? "");
        const data = url.split(",")[1] ?? "";
        if (data) pendingImages.push({ data, mime: f.type, preview: url });
      };
      rd.readAsDataURL(f);
    }
  }
  function removeImage(i: number) {
    pendingImages.splice(i, 1);
  }

  // consume inserted prompts from the 常用提示词 panel
  $effect(() => {
    const p = app.insertPrompt;
    if (!p) return;
    if (app.tab === "chat") {
      input = input ? input + (input.endsWith("\n") ? "" : "\n") + p.text : p.text;
      app.insertPrompt = null;
    }
  });

  const agentLabel = (a: string) => app.agents.find((x) => x.id === a)?.name ?? a;
</script>

<div class="chat">
  <div class="session-bar">
    <div class="agents">
      {#each app.agents as a (a.id)}
        <button
          class="agent-tab"
          class:active={app.agent === a.id}
          class:on={app.agentStatus[a.id] === "connected"}
          onclick={() => switchAgent(a.id)}
        >
          <span class="dot {app.agentStatus[a.id] === 'connected' ? 'ok' : ''}"></span>
          {a.name}
        </button>
      {/each}
    </div>
    <div class="session-meta">
      {#if sessionId}
        <span class="badge accent badge-title" title={bindingTitle ?? sessionId}>
          {bindingTitle || "会话 " + sessionId.slice(0, 8) + "…"}
        </span>
        <button
          class="btn ghost sm"
          title="修改会话标题（本地备注）"
          onclick={async () => {
            if (!ctx) return;
            const t = await promptDialog({ title: "会话标题", label: "标题（本地备注）", initial: bindingTitle ?? "" });
            if (t === null) return;
            try {
              await api.bindingSetTitle(ctx.id, app.agent, t);
              bindingTitle = t;
              app.bindingTitleMap[chatKey(ctx.id, app.agent)] = t;
              toast("ok", "标题已更新");
            } catch (e) {
              toast("error", String(e));
            }
          }}
        >✏️</button>
      {:else}
        <span class="badge">未绑定会话</span>
      {/if}
      <button class="btn sm" onclick={() => (app.historyBind = app.agent)} title="绑定 / 切换适配器中的历史会话">绑定</button>
      <button class="btn sm" onclick={sessionId ? resumeSession : newSession} title={sessionId ? "重新加载会话" : "创建新会话"}>
        {sessionId ? "重新连接" : "新会话"}
      </button>
      <button
        class="btn sm"
        title="复制共享上下文接入提示词，发送给 Agent 后即可通过 MCP 同步上下文"
        onclick={() => {
          if (!ctx) return;
          navigator.clipboard
            .writeText(sharedContextPrompt(ctx.id))
            .then(() => toast("ok", "接入提示词已复制，粘贴发送给 Agent 即可"))
            .catch((e) => toast("error", String(e)));
        }}
      >
        复制接入提示词
      </button>
    </div>
  </div>

  <div class="msgs-row">
    <ChatTimeline {items} onJump={(id) => document.getElementById("msg-" + id)?.scrollIntoView({ behavior: "smooth", block: "start" })} />
    <div class="msgs" bind:this={listEl} onscroll={onScroll}>
      <div class="msgs-inner" bind:this={msgsInnerEl}>
    {#if loadingHere && items.length === 0}
      <div class="chat-loading">
        <span class="chat-spinner"></span>
        <p class="lt">正在从 {agentLabel(app.agent)} 会话加载历史…</p>
        <p class="ld dim">适配器启动与历史回放可能需要几秒，请稍候</p>
      </div>
    {:else}
      {#if items.length === 0}
        <div class="empty">
          <p><b>{agentLabel(app.agent)}</b> 会话（{ctx?.name}）</p>
          <p class="dim">发送第一条消息开始工作。Agent 会读取项目目录并执行任务；<br/>权限请求会弹出对话框，工具调用会实时展示。</p>
          {#if !sessionId}
            <p class="dim">首次发送时将自动创建并绑定新会话。</p>
          {/if}
        </div>
      {/if}
      {#each items as item (item.id)}
        <div id={"msg-" + item.id}>
          <MessageItem {item} />
        </div>
      {/each}
      {#if loadingHere}
        <div class="syncing"><span class="chat-spinner sm"></span> 正在同步最新历史…</div>
      {/if}
      {#if streaming}
        <div class="thinking"><span class="dot accent pulse"></span> {agentLabel(app.agent)} 正在工作中…</div>
      {/if}
    {/if}
      </div>
    </div>
  </div>

  <div class="composer">
    <button class="btn ghost sm up" title="回到最新" onclick={() => { stickToBottom = true; if (listEl) listEl.scrollTop = listEl.scrollHeight; }}>↓</button>
    {#if pendingImages.length}
      <div class="thumbs">
        {#each pendingImages as img, i}
          <div class="thumb">
            <img src={img.preview} alt="待发送图片" />
            <button class="thumb-x" title="移除" onclick={() => removeImage(i)}>✕</button>
          </div>
        {/each}
      </div>
    {/if}
    {#if searchOpen}
    <div class="chat-search">
      <input placeholder="搜索正文…" bind:value={searchQuery} oninput={runSearch} onkeydown={onSearchKeydown} />
      <span class="count">{searchHits.length ? `${searchIdx + 1}/${searchHits.length}` : searchQuery ? "0/0" : ""}</span>
      <button class="btn ghost sm" title="上一个（Shift+Enter）" onclick={() => stepSearch(-1)}>↑</button>
      <button class="btn ghost sm" title="下一个（Enter）" onclick={() => stepSearch(1)}>↓</button>
      <button class="btn ghost sm" title="关闭（Esc）" onclick={closeSearch}>✕</button>
    </div>
  {/if}

  {#if cfgItems.length}
      <div class="cfg-bar">
        {#each cfgItems as c (c.id)}
          <label class="cfg">
            <span class="cfg-label">{c.label}</span>
            <select class="cfg-select" value={c.value} onchange={(e) => setCfg(c.id, (e.target as HTMLSelectElement).value)}>
              {#each c.options as o (o.value)}
                <option value={o.value}>{o.name}</option>
              {/each}
            </select>
          </label>
        {/each}
      </div>
    {/if}
    <div class="grow-handle" title="拖拽调整输入框高度" onpointerdown={startResize}><span></span></div>
    <div class="input-row">
    <textarea
      rows="3"
      style={composerH ? `height:${composerH}px` : ""}
      placeholder="给 {agentLabel(app.agent)} 下达任务…（{app.enterSend ? "Enter 发送，Shift+Enter 换行" : "Enter 换行，Ctrl+Enter 发送"}，可粘贴图片）"
      bind:value={input}
      onkeydown={onKeydown}
      onpaste={onPaste}
      oncontextmenu={onInputContext}
    ></textarea>
    {#if streaming}
      <button class="btn danger" onclick={stop}>■ 停止</button>
    {:else}
      <button class="btn primary" disabled={(!input.trim() && !pendingImages.length) || sending || !ctx} onclick={send}>发送 ➤</button>
    {/if}
    </div>
  </div>
</div>
  {#if pasteMenu}
    <ContextMenu x={pasteMenu.x} y={pasteMenu.y} items={[{ label: "📋 粘贴", run: () => void pasteIntoInput() }]} onclose={() => (pasteMenu = null)} />
  {/if}

<style>
  .chat {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .session-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--border-soft);
    flex-wrap: wrap;
  }
  .agents {
    display: flex;
    gap: 6px;
  }
  .agent-tab {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 5px 14px;
    border-radius: 99px;
    border: 1px solid var(--border);
    color: var(--text-dim);
  }
  .agent-tab:hover {
    background: var(--bg-elev);
  }
  .agent-tab.active {
    background: var(--bg-elev2);
    color: var(--text);
    border-color: var(--accent);
    font-weight: 600;
  }
  .session-meta {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .mode {
    font-size: 0.86em;
  }
  .msgs-row {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .msgs {
    flex: 1;
    overflow-y: auto;
    overflow-anchor: none;
    padding: 18px 0;
    min-height: 0;
  }
  .msgs-inner {
    display: flex;
    flex-direction: column;
    min-height: 100%;
  }
  .msgs .dim {
    font-size: 0.88em;
  }
  .thinking {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-faint);
    padding: 4px 24px;
    font-size: 0.88em;
  }
  .chat-loading {
    flex: 1;
    align-self: center;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 60px 20px;
    color: var(--text-dim);
  }
  .chat-loading .lt {
    color: var(--text);
    font-weight: 600;
  }
  .chat-loading .ld {
    font-size: 0.85em;
  }
  .syncing {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-faint);
    padding: 6px 24px;
    font-size: 0.84em;
  }
  .chat-spinner {
    width: 22px;
    height: 22px;
    flex: none;
    border-radius: 50%;
    border: 2.5px solid var(--border);
    border-top-color: var(--accent);
    animation: spin 0.8s linear infinite;
  }
  .chat-spinner.sm {
    width: 14px;
    height: 14px;
    border-width: 2px;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .chat-search {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 16px;
    border-top: 1px solid var(--border-soft);
    background: var(--bg-panel);
  }
  .chat-search input {
    width: 260px;
  }
  .chat-search .count {
    color: var(--text-faint);
    font-size: 0.84em;
    min-width: 48px;
    text-align: center;
  }
  :global(.msgs-inner > .flash) {
    animation: search-flash 1.6s ease;
  }
  @keyframes search-flash {
    0%, 60% {
      background: color-mix(in srgb, var(--warn) 22%, transparent);
    }
    100% {
      background: transparent;
    }
  }
  .cfg-bar {
    display: flex;
    gap: 6px 16px;
    padding: 7px 12px 6px;
    border-bottom: 1px solid var(--border-soft);
    flex-wrap: wrap;
    align-items: center;
  }
  .cfg {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 0.84em;
    color: var(--text-dim);
  }
  .cfg-label {
    color: var(--text-faint);
    letter-spacing: 0.02em;
    white-space: nowrap;
  }
  .badge-title {
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cfg-select {
    padding: 5px 26px 5px 10px;
    font-size: 0.9em;
    border-radius: 8px;
    border: 1px solid var(--border);
    background-color: var(--bg-panel);
    font-weight: 600;
    appearance: none;
    background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='11' height='11' viewBox='0 0 24 24' fill='none' stroke='%239ba3b8' stroke-width='2.4' stroke-linecap='round'><path d='M6 9l6 6 6-6'/></svg>");
    background-repeat: no-repeat;
    background-position: right 8px center;
    cursor: pointer;
    max-width: 240px;
  }
  .cfg-select:hover {
    border-color: var(--accent);
  }
  .thumbs {
    position: absolute;
    left: 12px;
    bottom: calc(100% + 8px);
    display: flex;
    gap: 8px;
    padding: 6px;
    background: var(--bg-panel);
    border: 1px solid var(--border-soft);
    border-radius: 10px;
    box-shadow: var(--shadow);
    z-index: 5;
  }
  .thumb {
    position: relative;
    width: 52px;
    height: 52px;
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    border-radius: 6px;
    display: block;
  }
  .thumb-x {
    position: absolute;
    top: -7px;
    right: -7px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    border: 1px solid var(--border);
    background: var(--bg-panel);
    color: var(--text-dim);
    font-size: 10px;
    line-height: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
  }
  .thumb-x:hover {
    color: var(--danger);
    border-color: var(--danger);
  }
  /* 输入面板：配置条与输入框一体（同一圆角面板，配置条贴在输入区上沿） */
  .composer {
    display: flex;
    flex-direction: column;
    margin: 8px 14px 12px;
    border: 1px solid var(--border-soft);
    border-radius: calc(var(--radius) + 2px);
    background: var(--bg-panel);
    position: relative;
  }
  .composer:focus-within {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--border-soft));
  }
  .input-row {
    display: flex;
    align-items: flex-end;
    gap: 10px;
    padding: 8px 12px 10px;
  }
  .input-row textarea {
    flex: 1;
    max-height: 460px;
    background: transparent;
    border: none;
    padding: 6px 4px;
    resize: none;
  }
  .grow-handle {
    height: 8px;
    cursor: ns-resize;
    display: flex;
    align-items: center;
    justify-content: center;
    flex: none;
    touch-action: none;
  }
  .grow-handle span {
    width: 42px;
    height: 3px;
    border-radius: 2px;
    background: var(--border);
  }
  .grow-handle:hover span {
    background: var(--text-faint);
  }
  .up {
    position: absolute;
    right: 14px;
    bottom: calc(100% + 8px);
    background: var(--bg-panel);
    border: 1px solid var(--border);
  }
</style>
