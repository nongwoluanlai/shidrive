<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { app, chatKey, currentContext, finishTurn, setChatRows, pushLocal, toast, clearChat, sharedContextPrompt, switchAgent, loadChatLocal, saveCfgPref, fullAccessDefault, touchChat } from "../state.svelte";
import { confirmDialog, promptDialog } from "../dialog.svelte";
  import { api } from "../ipc";
  import MessageItem from "./MessageItem.svelte";
  import { t } from "../i18n";
  import ContextMenu from "./ContextMenu.svelte";
  import ChatTimeline from "./ChatTimeline.svelte";
  import type { AgentType, SessionReadyInfo } from "../types";
import type { DisplayItem } from "../state.svelte";

  let input = $state("");
  let listEl: HTMLDivElement | undefined = $state();
  let msgsInnerEl: HTMLDivElement | undefined = $state();
  let stickToBottom = $state(true);
  /** queued image attachments (base64 + data-url preview) */
  let pendingImages = $state<{ data: string; mime: string; preview: string }[]>([]);

  const ctx = $derived(currentContext());
  const key = $derived(chatKey(ctx?.id ?? null, app.agent));
  // 按会话记录“正在发送”，而不是整个组件一个开关：某个会话的回合进行中不应锁住其他会话。
  const sending = $derived(app.streaming[key] ?? false);
  const items = $derived(app.chat[key] ?? []);
  // ============ 有界滑窗渲染：内存/卡顿与定位能力兼得 ============
  // 设计：消息数据全量在内存（供搜索/时间轴索引），DOM 只渲染 [startIdx, endIdx)
  // 窗口（最多 MAX_RENDER 条）。窗口外的消息用「高度缓存 + 估算」占位，保持滚动条
  // 比例；所有定位（搜索跳转/时间轴/↑键）都通过 scrollToItem 原语：先把目标纳入
  // 窗口，再精准滚动。DOM 有界 → 长会话不再卡顿；索引全量 → 定位不丢。
  const CHUNK = 30;
  const MAX_RENDER = 240;
  let startIdx = $state(0);
  let endIdx = $state(0);
  const shownItems = $derived(items.slice(startIdx, endIdx));
  const olderCount = $derived(startIdx);
  const newerHidden = $derived(Math.max(0, items.length - endIdx));

  // 高度缓存：渲染过的消息记实测高度，未渲染的按类型/长度估算
  const hMap = new Map<string, number>();
  let hVersion = $state(0);
  function estHeight(it: DisplayItem): number {
    // tools/thought 默认折叠为一行摘要：按折叠高度估算（此前按全文长度估，
    // 一个 115 次调用的工具块会被估到 4000px 上限，而实际只有 ~60px，
    // 折叠块一多，顶部占位出现数千像素空白——滚动上翻大片空白的根因）。
    if (it.kind === "tools") return 88;
    if (it.kind === "thought") return 64;
    const base = it.kind === "user" ? 84 : it.kind === "error" ? 76 : 72;
    return Math.min(base + Math.round((it.text?.length ?? 0) * 0.28), 2600);
  }
  function hOf(it: DisplayItem): number {
    void hVersion;
    return hMap.get(it.id) ?? estHeight(it);
  }
  // 窗口外占位高度（保持滚动条长度与位置大致成比例）
  // 占位高度增量缓存：会话回放/流式期间 items 每条追加都会让 $derived 失效，
  // 全量 reduce 会造成 O(n²)（数千条的长会话回放直接把主线程打满——页面表现为
  // 「服务繁忙」：下拉打不开、历史迟迟不出）。策略：
  // - 顶部前缀 [0..startIdx) 只在 整表替换(chatRev)/窗口移动/测量版本 变化时重算，
  //   尾部追加不触碰前缀 → 回放期间 O(1)；
  // - 底部后缀按追加增量累加（每条只消费一次）→ 摊还 O(1)。
  const chatRevNum = () => app.chatRev[key] ?? 0;
  const padCache = { rev: -1, start: -1, end: -1, hV: -1, len: -1, top: 0, bot: 0 };
  const topPad = $derived.by(() => {
    const rev = chatRevNum();
    if (padCache.rev !== rev) {
      // 整表替换（重绑/重放）：顺带清理 hMap 中已死条目的 id —— 每次重绑产生全新
      // id 序列，不清会随重绑次数线性累积（内存持续上涨的来源之一）
      hMap.clear();
      padCache.rev = rev; padCache.start = -1; padCache.end = -1; padCache.len = -1;
    }
    if (padCache.hV !== hVersion) {
      padCache.hV = hVersion; padCache.start = -1; padCache.end = -1;
    }
    if (padCache.start < 0 || padCache.start > startIdx) {
      let sum = 0;
      for (let i = 0; i < startIdx; i++) sum += hOf(items[i]);
      padCache.top = sum; padCache.start = startIdx;
    } else if (padCache.start < startIdx) {
      // 窗口下滑（贴底流式回收顶部 / 向下跳转 / 下探扩窗）：从窗口顶端离场的条目
      // 并入顶部占位，高度要「加」上去。v0.3.9 这里写成了减——长会话流式一段时间后
      // 顶部占位被扣成负数、钳到 0：上方明明还有成百上千条，滚动条却显示已到顶，
      // 上翻立刻撞到 scrollTop=0，搜索/时间轴按占位算出的落点也随之失真。
      for (let i = padCache.start; i < startIdx; i++) padCache.top += hOf(items[i]);
      padCache.start = startIdx;
    }
    return Math.max(0, padCache.top);
  });
  const botPad = $derived.by(() => {
    topPad; // 顶部缓存先落地，保证 rev/hV 一致
    if (padCache.end < 0 || padCache.end > endIdx || padCache.len > items.length) {
      let sum = 0;
      for (let i = endIdx; i < items.length; i++) sum += hOf(items[i]);
      padCache.bot = sum; padCache.end = endIdx; padCache.len = items.length;
    } else {
      if (padCache.end < endIdx) {
        // 窗口下沿前移：进入窗口的项从底部占位扣除
        for (let i = padCache.end; i < endIdx; i++) padCache.bot -= hOf(items[i]);
        padCache.end = endIdx;
      }
      if (padCache.len < items.length) {
        // 尾部追加：增量累加（回放/流式每条只消费一次）
        for (let i = padCache.len; i < items.length; i++) padCache.bot += hOf(items[i]);
        padCache.len = items.length;
      }
    }
    return Math.max(0, padCache.bot);
  });

  // 记录当前已渲染消息的实测高度（滚动/开窗后惰性调用）。
  // 缩放防护：rAF 合并多次请求；视口宽度未变时跳过（高度自然变化不重测，
  // 贴底由 ResizeObserver 负责）——否则拖拽缩放时每帧 240 次 getBoundingClientRect
  // 强制回流 + 占位高度重算 + 再触发布局，形成正反馈把主线程打满。
  let measureScheduled = false;
  let lastMeasureWidth = -1;
  function measureRendered(force = false) {
    if (!listEl) return;
    if (force) { doMeasure(); return; }
    if (measureScheduled) return;
    measureScheduled = true;
    requestAnimationFrame(() => {
      measureScheduled = false;
      if (!listEl) return;
      const w = listEl.clientWidth;
      if (w === lastMeasureWidth) return;
      lastMeasureWidth = w;
      doMeasure();
    });
  }
  // content-visibility:auto 下离屏条目被跳过排版，getBoundingClientRect 给出的是
  // contain-intrinsic-size 占位（未渲染过的一律 120px）——记进 hMap 会用 120 覆盖更准的
  // 估算值，占位高度随之失真。只记真正排过版的条目。
  function isSkipped(el: HTMLElement): boolean {
    const cv = (el as HTMLElement & { checkVisibility?: (o: { contentVisibilityAuto: boolean }) => boolean }).checkVisibility;
    return typeof cv === "function" && !cv.call(el, { contentVisibilityAuto: true });
  }
  function doMeasure() {
    if (!listEl) return;
    lastMeasureWidth = listEl.clientWidth;
    const wrappers = listEl.querySelectorAll<HTMLDivElement>('.msgs-inner > div[id^="msg-"]');
    let changed = false;
    for (const el of wrappers) {
      if (isSkipped(el)) continue;
      const id = el.id.slice(4);
      const h = Math.round(el.getBoundingClientRect().height);
      if (h > 0 && hMap.get(id) !== h) { hMap.set(id, h); changed = true; }
    }
    if (changed) hVersion++;
  }

  const idToIndex = $derived.by(() => {
    const m = new Map<string, number>();
    items.forEach((it, i) => m.set(it.id, i));
    return m;
  });

  // 把 index 纳入渲染窗口（以目标为中心开窗，双端不超 MAX_RENDER）
  function ensureRendered(index: number) {
    const len = items.length;
    if (index >= startIdx && index < endIdx) return;
    let e = Math.min(len, index + Math.ceil(MAX_RENDER / 2));
    let s = Math.max(0, e - MAX_RENDER);
    if (index < s) s = Math.max(0, index - CHUNK);
    e = Math.min(len, Math.max(e, s + 1));
    if (e - s > MAX_RENDER) s = e - MAX_RENDER;
    startIdx = Math.max(0, s);
    endIdx = Math.max(startIdx + 1, e);
  }

  // ============ 统一定位原语：确保渲染 → 精准滚动 → 闪烁提示 ============
  // 目标与当前视口之间隔着的条目多半处于 content-visibility:auto 的「跳过」态，
  // 它们的盒子只是 120px 占位；按这些占位算出的 scrollTop 落地后，浏览器把新视口
  // 附近的条目真正排版（普遍高于 120px），目标随即被顶出视口——单次校准只能追上
  // 第一波变化，每按一次「上一个」目标就再偏一截（搜索向上跳、看着没动，手动一滚
  // 才出现）。所以改成逐帧收敛：连续两帧不需要修正才算落定，上限 12 帧。
  let scrollToItemSeq = 0;
  async function scrollToItem(id: string, align: "center" | "start" = "center") {
    const idx = idToIndex.get(id);
    if (idx === undefined || !listEl) return;
    const seq = ++scrollToItemSeq;
    stickToBottom = false;
    ensureRendered(idx);
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    if (seq !== scrollToItemSeq) return; // 已有更新的定位请求
    const el = document.getElementById("msg-" + id) as HTMLElement | null;
    if (!el || !listEl) return;
    const place = (target: HTMLElement): boolean => {
      if (!listEl) return true;
      const listRect = listEl.getBoundingClientRect();
      const rel = target.getBoundingClientRect().top - listRect.top;
      // 比视口还高的条目「居中」等于把开头顶出视口：这种情况按顶部对齐
      const fits = target.offsetHeight < listEl.clientHeight - 48;
      const want = align === "center" && fits ? listEl.clientHeight / 2 - target.offsetHeight / 2 : 24;
      const delta = rel - want;
      if (Math.abs(delta) <= 1) return true;
      lastProgScrollAt = performance.now();
      const before = listEl.scrollTop;
      listEl.scrollTop = Math.max(0, before + delta);
      prevScrollTop = listEl.scrollTop;
      // 已顶到边界（scrollTop 没变）也视为落定，避免空转
      return listEl.scrollTop === before;
    };
    place(el);
    el.classList.remove("flash");
    void el.offsetWidth;
    el.classList.add("flash");
    // 逐帧收敛：markdown/图片异步撑高、跳过态条目补排版都会移动目标
    let stable = 0;
    for (let frame = 0; frame < 12 && stable < 2; frame++) {
      await new Promise((r) => requestAnimationFrame(r));
      if (seq !== scrollToItemSeq || !listEl) return;
      const cur = document.getElementById("msg-" + id) as HTMLElement | null;
      if (!cur) return;
      stable = place(cur) ? stable + 1 : 0;
    }
    measureRendered(true);
  }

  const chatRev = () => app.chatRev[key] ?? 0;

  // 切换会话/历史重载时：窗口重置到最新一页。
  // items.length 用 untrack 读取：否则回放/流式期间每条追加都会重跑本 effect，
  // startIdx 跟着尾巴滑动 → 顶部占位前缀逐条重算（O(n²)，长会话回放直接卡死，
  // 用户表现为"服务繁忙"）。窗口尾随由下方的追加 effect 负责。
  $effect(() => {
    key;
    chatRev();
    untrack(() => {
      endIdx = items.length;
      startIdx = Math.max(0, endIdx - CHUNK);
    });
  });

  function loadOlder() {
    startIdx = Math.max(0, startIdx - CHUNK);
    if (endIdx - startIdx > MAX_RENDER) endIdx = startIdx + MAX_RENDER;
  }

  // 首条 / 末条已渲染消息相对视口顶部的位置（null = 没有渲染任何消息）
  function firstRenderedTop(): number | null {
    if (!listEl) return null;
    const first = listEl.querySelector('.msgs-inner > div[id^="msg-"]') as HTMLElement | null;
    return first ? first.getBoundingClientRect().top - listEl.getBoundingClientRect().top : null;
  }
  // 注意不能用 `div[id^="msg-"]:last-of-type`：:last-of-type 看的是「最后一个 div」，
  // 窗口下面还有底部占位 <div class="win-pad"> 时它永远匹配不到消息（原 extendBottomIfNeeded
  // 就是这样失效的，只是此前尾巴总被整段渲染、没有底部占位，问题被盖住了）
  function lastRenderedEl(): HTMLElement | null {
    if (!listEl) return null;
    const all = listEl.querySelectorAll<HTMLElement>('.msgs-inner > div[id^="msg-"]');
    return all.length ? all[all.length - 1] : null;
  }
  function lastRenderedBottom(): number | null {
    const last = lastRenderedEl();
    return last && listEl ? last.getBoundingClientRect().bottom - listEl.getBoundingClientRect().top : null;
  }
  // 距窗口边缘多近就顺序扩窗（像素）；再远就是「深入占位区」，按比例重开窗
  const EDGE_ZONE = 480;

  // 上翻自动扩窗：视口接近已渲染窗口的顶边即触发，每轮以首条消息锚定视线，避免跳动。
  // 判据是「距首条已渲染消息的距离」而不是 scrollTop 的绝对值：顶部占位随历史长度可达
  // 几万像素，按 scrollTop<480 判断意味着要先滚过整段空白才会加载。
  let loadingOlder = false;
  async function onListScrollTop() {
    if (loadingOlder || startIdx === 0 || !listEl) return;
    const near = () => {
      const t = firstRenderedTop();
      return t !== null && t > -EDGE_ZONE;
    };
    if (!near()) return;
    loadingOlder = true;
    try {
      for (let round = 0; round < 5; round++) {
        const first = listEl.querySelector('.msgs-inner > div[id^="msg-"]') as HTMLElement | null;
        const anchorTop = first?.getBoundingClientRect().top ?? 0;
        const prevStart = startIdx;
        startIdx = Math.max(0, startIdx - CHUNK);
        if (endIdx - startIdx > MAX_RENDER) endIdx = startIdx + MAX_RENDER;
        await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
        if (!listEl) break;
        if (first) {
          lastProgScrollAt = performance.now();
          listEl.scrollTop += first.getBoundingClientRect().top - anchorTop;
          prevScrollTop = listEl.scrollTop;
        }
        measureRendered(true);
        if (startIdx === 0 || startIdx === prevStart) break;
        // 仍贴着窗口顶边才继续扩；已离开则停
        if (!near()) break;
      }
    } finally {
      loadingOlder = false;
    }
  }

  // 下探扩窗：跳转到中部历史后继续向下读，视口接近窗口底边时把窗口滑向下。
  // 判据同样是「距末条已渲染消息的距离」：scrollHeight-scrollTop-clientHeight 把底部
  // 占位也算进去了，只要窗口下面还有几百条未渲染，它就永远大于阈值，窗口永远不会下滑。
  let extendingBottom = false;
  function extendBottomIfNeeded() {
    if (extendingBottom || newerHidden === 0 || !listEl) return;
    const bottom = lastRenderedBottom();
    if (bottom === null || bottom - listEl.clientHeight > EDGE_ZONE) return;
    const anchor = lastRenderedEl();
    const anchorBottom = anchor?.getBoundingClientRect().bottom ?? 0;
    extendingBottom = true;
    endIdx = Math.min(items.length, endIdx + CHUNK);
    if (endIdx - startIdx > MAX_RENDER) startIdx = endIdx - MAX_RENDER;
    requestAnimationFrame(() => {
      extendingBottom = false;
      const a2 = lastRenderedEl();
      if (anchor && a2 && listEl && anchor !== a2) {
        // 顶部被回收时末条锚点会上移：把差值补回去，视线不动
        const delta = anchor.isConnected ? anchor.getBoundingClientRect().bottom - anchorBottom : 0;
        if (delta) {
          lastProgScrollAt = performance.now();
          listEl.scrollTop += delta;
          prevScrollTop = listEl.scrollTop;
        }
      }
      measureRendered(true);
    });
  }

  // 定位上一条发出的消息气泡：数据驱动（不再依赖已渲染 DOM，未加载的旧消息也能定位）
  function jumpToPrevUserMessage() {
    if (!items.length || !listEl) return;
    const listRect = listEl.getBoundingClientRect();
    let cur = startIdx;
    for (const el of Array.from(listEl.querySelectorAll('.msgs-inner > div[id^="msg-"]')) as HTMLElement[]) {
      if (el.getBoundingClientRect().bottom > listRect.top + 80) {
        cur = idToIndex.get(el.id.slice(4)) ?? startIdx;
        break;
      }
    }
    let target = -1;
    for (let i = Math.min(cur, items.length - 1); i >= 0; i--) {
      if (items[i].kind === "user") { target = i; break; }
    }
    if (target < 0) {
      for (let i = items.length - 1; i >= 0; i--) {
        if (items[i].kind === "user") { target = i; break; }
      }
    }
    if (target < 0) { toast("info", t("没有更早的发出的消息")); return; }
    void scrollToItem(items[target].id, "start");
  }
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
    input = app.drafts[chatKey(ctxId, agent)] ?? ""; // 恢复该会话未发送草稿
    void (async () => {
      try {
        const k = chatKey(ctxId, agent);
        // 记录“正在查看”，并把久未查看的其它会话从内存中释放（本地库仍有快照）
        untrack(() => touchChat(k));
        // 先读本地快照，立即渲染（不等适配器）；本地为空时保持旧行为
        const local = await loadChatLocal(k);
        if (local && !app.chat[k]?.length && app.contextId === ctxId && app.agent === agent) {
          app.chat[k] = local;
          app.chatRev[k] = (app.chatRev[k] ?? 0) + 1;
        }
        const b = await api.bindingGet(ctxId, agent);
        app.bindingSession[k] = b?.session_id ?? null;
        app.bindingTitleMap[k] = b?.title ?? null;
        bindingTitle = b?.title ?? null;
        // 历史展示以本地库为准（秒开）；仅本地为空时才等适配器全量重放。
        // 有本地记录时跳过重放，回合结束后的 ensure_session 会在后台把会话装载回适配器。
        if (b?.session_id && ctx && ctx.id === ctxId && !app.chat[k]?.length) {
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
    // 流式追加：贴底时窗口尾随到最新并回收顶部；阅读中途（未贴底）只在窗口未满时
    // 顺延——此前无条件 endIdx = n，任何一次搜索/时间轴跳转之后整段尾巴（可达数千条）
    // 都被塞进 DOM：ensureRendered 的有界窗口形同虚设，「加载更新的 N 条」按钮永远
    // 不会出现，跳转后每一步都在成百上千个 content-visibility 占位上折腾。
    if (n > endIdx) {
      if (stickToBottom) {
        endIdx = n;
        if (endIdx - startIdx > MAX_RENDER) startIdx = endIdx - MAX_RENDER;
      } else if (endIdx - startIdx < MAX_RENDER) {
        endIdx = Math.min(n, startIdx + MAX_RENDER);
      }
    }
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
  let scrollBottomScheduled = false;
  function scrollToBottom() {
    if (!listEl) return;
    // rAF 合并：流式追加/缩放时 ResizeObserver 可能每帧多次触发
    if (scrollBottomScheduled) return;
    scrollBottomScheduled = true;
    requestAnimationFrame(() => {
      scrollBottomScheduled = false;
      if (!listEl) return;
      lastProgScrollAt = performance.now();
      listEl.scrollTop = listEl.scrollHeight;
      prevScrollTop = listEl.scrollTop; // 程序置底后的基准（此前误写 scrollHeight，
      // 导致贴底后首个滚动事件被误判为"用户上滚"而脱离贴底）
    });
  }
  let prevClientH = 0;
  function onScroll() {
    if (!listEl) return;
    const now = performance.now();
    // 视口尺寸变化（缩放/拖拽窗口边）引发的滚动事件：只更新基准，不做扩窗/导航——
    // 否则「重排→滚动→重开窗→测量→占位高度变→再重排」的正反馈会在拖拽缩放时把主线程打满。
    // 注意不能把 scrollHeight 变化也当作几何变化：content-visibility:auto 下每滚一段就有
    // 条目被真正排版，长会话里几乎每个 scroll 事件 scrollHeight 都在变——v0.3.9 以此早退，
    // 结果上翻进入占位区后再也不扩窗，视口停在大片空白里。
    const viewportChanged = listEl.clientHeight !== prevClientH;
    prevClientH = listEl.clientHeight;
    // 1) 我们自己的程序滚动 echo（含 rAF 校准）一律忽略
    if (now - lastProgScrollAt < 300) return;
    // 2) 内容撑高/锚定只会让 scrollTop 增大或不变；只有用户向上滚才会让 scrollTop 减小
    const scrolledUp = listEl.scrollTop < prevScrollTop - 2;
    prevScrollTop = listEl.scrollTop;
    if (scrolledUp) stickToBottom = false;
    if (viewportChanged) return;
    if (scrolledUp) {
      if (startIdx > 0) {
        const t = firstRenderedTop();
        if (t !== null && t > 240) void navigatePadIfNeeded(); // 已深入顶部占位区：按比例重开窗
        else if (t !== null && t > -EDGE_ZONE) void onListScrollTop(); // 贴近窗口顶边：顺序扩窗
      }
      return;
    }
    const dist = listEl.scrollHeight - listEl.scrollTop - listEl.clientHeight;
    if (dist < 60) stickToBottom = true;
    if (newerHidden > 0) {
      const b = lastRenderedBottom();
      if (b !== null && b < listEl.clientHeight - 240) void navigateBottomPadIfNeeded(); // 已深入底部占位区
      else extendBottomIfNeeded();
    }
  }

  // 拖动滚动条直接跳进顶部占位区深处：按滚动比例换算目标索引，整窗重开到该处，
  // 而不是从当前窗口按 30 条逐步扩（那要滚很多轮才能把数千像素占位消费完）。
  let lastNavAt = 0;
  async function navigatePadIfNeeded() {
    if (!listEl || startIdx === 0 || topPad <= 0) return;
    if (performance.now() - lastNavAt < 400) return; // 防重排风暴下连环重开窗
    lastNavAt = performance.now();
    const firstTop = firstRenderedTop();
    if (firstTop === null || firstTop < 240) return; // 首条已接近视口顶部，走正常分块扩窗路径
    const frac = Math.min(0.999, Math.max(0, listEl.scrollTop / Math.max(1, topPad)));
    const target = Math.max(0, Math.min(startIdx - 1, Math.round(frac * startIdx)));
    const targetId = items[target]?.id;
    if (!targetId) return;
    // 交给统一定位原语：开窗 + 逐帧收敛（占位条目补排版会移动目标）
    await scrollToItem(targetId, "start");
  }

  // 对称：拖动滚动条跳进底部占位区深处（搜索/时间轴跳到中部历史后再往下拖）
  async function navigateBottomPadIfNeeded() {
    if (!listEl || newerHidden === 0 || botPad <= 0) return;
    if (performance.now() - lastNavAt < 400) return;
    lastNavAt = performance.now();
    const bottom = lastRenderedBottom();
    if (bottom === null || bottom > listEl.clientHeight - 240) return;
    // 视口顶部落在底部占位里的比例 → 换算成 [endIdx, items.length) 里的目标索引
    const padStart = listEl.scrollTop + bottom; // 占位区在滚动坐标系里的起点
    const frac = Math.min(0.999, Math.max(0, (listEl.scrollTop - padStart) / Math.max(1, botPad)));
    const target = Math.min(items.length - 1, endIdx + Math.floor(frac * newerHidden));
    const targetId = items[target]?.id;
    if (!targetId) return;
    await scrollToItem(targetId, "start");
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
      toast("warn", t("会话正在进行中，请先停止"));
      return;
    }
    if (!(await confirmDialog({ title: t("新建会话"), message: t("创建新会话？当前绑定的 AI 会话将被解绑（AI 端历史仍保留）。") }))) return;
    try {
      let sid: string;
      if (app.agent === "deepseek") {
        // 失败时保留旧绑定与聊天记录；只在真正建成新会话后替换。
        sid = await api.acpDeepseekNew(ctx);
        clearChat(key);
      } else {
        await api.bindingUnbind(ctx.id, app.agent);
        clearChat(key);
        sid = await api.acpSessionNew(ctx, app.agent);
      }
      app.bindingSession[key] = sid;
      bindingTitle = t("新会话");
      stickToBottom = true;
      toast("ok", t("已创建新会话（{agent}）", { agent: agentLabel(app.agent) }));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function resumeSession() {
    if (!ctx) return;
    try {
      const sid = await api.acpSessionNew(ctx, app.agent);
      app.bindingSession[key] = sid;
      toast("ok", t("已恢复绑定的会话"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function send() {
    if (!ctx || sending) return;
    const text = input.trim();
    if (!text && !pendingImages.length) return;
    // 固定本回合的会话身份：await 期间用户可能切换 agent/上下文，之后 ctx/key/app.agent
    // 都会指向别的会话，回合结束的状态必须写回发起时的那一份。
    const turnCtx = ctx;
    const turnAgent = app.agent;
    const turnKey = key;
    const imgs = pendingImages.map((p) => ({ data: p.data, mime: p.mime }));
    input = "";
    app.drafts[turnKey] = "";
    pendingImages = [];
    stickToBottom = true;
    pushLocal(turnKey, { kind: "user", text: text + (imgs.length ? `\n\n[图片 ×${imgs.length}]` : "") });
    app.streaming[turnKey] = true;
    try {
      const res = await api.acpPrompt(turnCtx, turnAgent, text, imgs);
      const stop = res?.stopReason ?? "end_turn";
      if (stop !== "end_turn" && key === turnKey) toast("info", t("回合结束（{reason}）", { reason: String(stop) }));
    } catch (e) {
      pushLocal(turnKey, { kind: "error", text: String(e) });
    } finally {
      finishTurn(turnAgent, turnCtx.id);
      // refresh binding (session id may have just been created)
      api
        .bindingGet(turnCtx.id, turnAgent)
        .then((b) => {
          app.bindingSession[turnKey] = b?.session_id ?? null;
          if (key === turnKey) bindingTitle = b?.title ?? null;
        })
        .catch(() => {});
    }
  }

  async function stop() {
    if (!ctx) return;
    try {
      await api.acpCancel(ctx.id, app.agent);
      toast("info", t("已发送取消请求"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function setModel(modelId: string) {
    if (!ctx) return;
    try {
      await api.acpSetConfigOption(ctx.id, app.agent, "model", modelId);
      toast("ok", t("模型已切换"));
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
    t((id ? OPT_LABELS[id.toLowerCase()] : undefined) ?? OPT_LABELS[fallback.toLowerCase()] ?? fallback);
  const zhValue = (v: string) => t(OPT_VALUES[v.toLowerCase()] ?? v);

  const caps = $derived.by(() => {
    const c = app.agentCaps[app.agent];
    if (c) return c as { models?: SessionReadyInfo["response"]["models"]; configOptions?: any[] };
    return {} as { models?: SessionReadyInfo["response"]["models"]; configOptions?: any[] };
  });

  // 显示层允许 caps 兜底（恢复会话的 session-ready 常不带 models/configOptions，
  // 没有兜底配置栏会整个消失）；选到过期 id 的 -32602 由 setCfg 的错误分支处理
  const liveModels = $derived(sessionInfo?.response?.models ?? caps.models ?? null);

  const cfgItems = $derived.by((): CfgItem[] => {
    const out: CfgItem[] = [];
    const mapOpts = (list: any[]) =>
      (list ?? []).map((x: any) => {
        const v = String(x.value);
        const zh = zhValue(v);
        // 值有中文映射用映射；否则尝试按英文名映射；都没有保留原名
        const name = zh !== v ? zh : t(MODE_NAME_TEXT[String(x.name ?? "").toLowerCase()] ?? x.name);
        return { value: v, name };
      });
    const cfgOpts: any[] = sessionInfo?.response?.configOptions ?? caps.configOptions ?? [];
    for (const o of cfgOpts) {
      if (o.type && o.type !== "select") continue;
      if (out.some((x) => x.id === o.id)) continue;
      // 模型项必须用 configOptions 里的 value：适配器只认自己给出的 id。
      // models 能力的 modelId 是"模型 (思考等级)"复合 id，codex 会报 -32602。
      const opts = mapOpts(o.options ?? []);
      if (!opts.length) continue;
      out.push({ id: o.id, label: zhLabel(o.id, o.name ?? o.id), value: String(o.currentValue ?? ""), options: opts });
    }
    // 回退：适配器没提供 model 配置项时才用 models 能力
    const models = liveModels;
    if (!out.some((x) => x.id === "model") && models?.availableModels?.length) {
      const cur = models.currentModelId ?? models.currentModel ?? "";
      out.push({
        id: "model",
        label: t("模型"),
        value: cur,
        options: models.availableModels.map((m) => ({ value: m.modelId, name: m.name })),
      });
    }
    return out;
  });

  function setCfg(id: string, value: string) {
    // 记住用户选择：跨会话/重启生效（session-ready 后自动回放）
    saveCfgPref(app.agent, id, value);
    // 更新本地显示（无论是否已有会话）
    if (id === "model" && sessionInfo?.response?.models) sessionInfo.response.models.currentModelId = value;
    if (sessionInfo?.response?.configOptions) {
      const opt = sessionInfo.response.configOptions.find((o) => o.id === id);
      if (opt) opt.currentValue = value;
    }
    if (!sessionId) {
      // 会话尚未创建：暂存，session-ready 后自动应用
      app.pendingCfg[app.agent] = { id, value };
      toast("info", t("已记录选择，创建会话后自动应用"));
      return;
    }
    if (!ctx) return;
    void api
      .acpSetConfigOption(ctx.id, app.agent, id, value)
      .then(() => toast("ok", t("已应用")))
      .catch((e) => {
        const msg = String(e);
        if (msg.includes("-32602") || msg.includes("Invalid params")) {
          // 会话真实列表与界面不一致（caps 缓存过期/会话列表变化）：
          // 清掉缓存，重连后拿适配器真实列表
          delete app.agentCaps[app.agent];
          void api.settingsSet("caps." + app.agent, "").catch(() => {});
          toast(
            "error",
            t("该选项在当前会话不可用（列表已过期）。请点「重新连接」刷新后重试。"),
          );
        } else {
          toast("error", msg);
        }
      });
  }

  // 会话配置记忆：session-ready / 重放刷新后，把用户记住的配置（模型/模式等）
  // 重新套用，避免被适配器默认值刷掉。无记忆时，给支持"完全访问"类档位的
  // 配置项一次性套默认完全访问（用户随后可改，改动即被记住）。
  let cfgAppliedKey = "";
  // 正在飞行的自动回放请求数（v0.3.9 用布尔值：当某个会话无需回放任何项时没有
  // 请求去把它复位，之后所有会话的配置记忆都被永久跳过）
  let cfgInflight = $state(0);
  let cfgAppliedSession = "";
  $effect(() => {
    const info = sessionInfo;
    const sid = sessionId;
    if (!info?.response || !sid || !ctx) return;
    const sig = JSON.stringify(info.response.configOptions ?? []) + JSON.stringify(info.response.models ?? {});
    if (sig === cfgAppliedKey) return;
    // 同一会话只自动回放一次；正在回放时跳过——防止「回放→session-ready 刷新→
    // 再回放」的 IPC 风暴（曾把 Win32 消息队列灌爆导致整个应用假死）。
    // cfgInflight 是 $state：请求飞完归零会重跑本 effect，期间被跳过的新会话得以补做
    if (cfgInflight > 0 || cfgAppliedSession === sid) return;
    cfgAppliedKey = sig;
    cfgAppliedSession = sid;
    for (const item of cfgItems) {
      const remembered = app.cfgPref[`${app.agent}:${item.id}`];
      const desired = remembered ?? (item.id === "model" ? undefined : fullAccessDefault(item.id, item.options.map((o) => o.value)));
      if (!desired || desired === item.value) continue;
      if (!item.options.some((o) => o.value === desired)) continue;
      // 本地立即生效 + 适配器同步（失败静默：个别项可能在当前会话不可切换）
      if (item.id === "model" && info.response.models) info.response.models.currentModelId = desired;
      const opt = info.response.configOptions?.find((o) => o.id === item.id);
      if (opt) opt.currentValue = desired;
      saveCfgPref(app.agent, item.id, desired);
      cfgInflight++;
      void api
        .acpSetConfigOption(ctx.id, app.agent, item.id, desired)
        .catch(() => {})
        .finally(() => {
          cfgInflight = Math.max(0, cfgInflight - 1);
        });
    }
  });

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
          toast("info", t("原模型已下线，已切换为 {model}", { model: models[0]?.name ?? target }));
        })
        .catch(() => {});
    }
  });

  // ---------- Ctrl+F 正文搜索 ----------
  let searchOpen = $state(false);
  let searchQuery = $state("");
  let searchHits = $state<{ id: string; text: string; index: number }[]>([]);
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
      .map((it, index) => ({ id: it.id, text: it.text, index, kind: it.kind }))
      .filter((it) => it.kind === "user" || it.kind === "assistant" || it.kind === "error")
      .filter((it) => it.text.toLowerCase().includes(q))
      .map(({ id, text, index }) => ({ id, text, index }));
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
    // 数据驱动：先把目标纳入渲染窗口（可在任意未加载的历史位置），再精准滚动
    void scrollToItem(hit.id, "center");
  }

  function closeSearch() {
    searchOpen = false;
    searchQuery = "";
    searchHits = [];
    searchIdx = -1;
  }

  // ---------- 执行后动作（回合结束后触发；配置全局生效，存 localStorage） ----------
  type AfterActionKind = "sound" | "shutdown" | "command";
  let afterAction = $state<{ kind: AfterActionKind; cmd: string }>({ kind: "sound", cmd: "" });

  $effect(() => {
    // 挂载时读一次持久化配置（与 events.ts 的 loadAfterAction 同一存储）
    try {
      const raw = localStorage.getItem("shidrive.afterAction");
      if (raw) {
        const v = JSON.parse(raw);
        if (v && (v.kind === "sound" || v.kind === "shutdown" || v.kind === "command")) {
          afterAction = { kind: v.kind, cmd: typeof v.cmd === "string" ? v.cmd : "" };
        }
      }
    } catch {}
  });

  function saveAfterAction() {
    try { localStorage.setItem("shidrive.afterAction", JSON.stringify(afterAction)); } catch {}
    if (afterAction.kind === "shutdown") toast("warn", t("回合结束后将安排关机（30 秒缓冲，cmd 执行 shutdown /a 可取消）"));
    else if (afterAction.kind === "command" && !afterAction.cmd.trim()) toast("warn", t("请填写要执行的 cmd 命令"));
  }

  // ---------- 输入框右键粘贴 ----------
  let pasteMenu = $state<{ x: number; y: number } | null>(null);
  let pasteRange = { start: 0, end: 0 };
  function onInputContext(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    const ta = e.currentTarget as HTMLTextAreaElement;
    pasteRange = { start: ta.selectionStart, end: ta.selectionEnd };
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
      const start = Math.min(pasteRange.start, input.length);
      const end = Math.min(pasteRange.end, input.length);
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
          {bindingTitle || t("会话 {id}", { id: sessionId.slice(0, 8) + "…" })}
        </span>
        <button
          class="btn ghost sm"
          title={t("修改会话标题（本地备注）")}
          onclick={async () => {
            if (!ctx) return;
            const newTitle = await promptDialog({ title: t("会话标题"), label: t("标题（本地备注）"), initial: bindingTitle ?? "" });
            if (newTitle === null) return;
            try {
              await api.bindingSetTitle(ctx.id, app.agent, newTitle);
              bindingTitle = newTitle;
              app.bindingTitleMap[chatKey(ctx.id, app.agent)] = newTitle;
              toast("ok", t("标题已更新"));
            } catch (e) {
              toast("error", String(e));
            }
          }}
        >✏️</button>
      {:else}
        <span class="badge">{t("未绑定会话")}</span>
      {/if}
      <button class="btn sm" onclick={() => (app.historyBind = app.agent)} title={t("绑定 / 切换适配器中的历史会话")}>绑定</button>
      <button class="btn sm" onclick={sessionId ? resumeSession : newSession} title={sessionId ? t("重新加载会话") : t("创建新会话")}>
        {sessionId ? t("重新连接") : t("新会话")}
      </button>
      <button
        class="btn sm"
        title={t("复制共享上下文接入提示词，发送给 Agent 后即可通过 MCP 同步上下文")}
        onclick={() => {
          if (!ctx) return;
          navigator.clipboard
            .writeText(sharedContextPrompt(ctx.id))
            .then(() => toast("ok", t("接入提示词已复制，粘贴发送给 Agent 即可")))
            .catch((e) => toast("error", String(e)));
        }}
      >
        {t("复制接入提示词")}
      </button>
    </div>
  </div>

  <div class="msgs-row">
    <ChatTimeline {items} onJump={(id) => void scrollToItem(id, "start")} />
    <div class="msgs" bind:this={listEl} onscroll={onScroll}>
      <div class="msgs-inner" bind:this={msgsInnerEl}>
    {#if loadingHere && items.length === 0}
      <div class="chat-loading">
        <span class="chat-spinner"></span>
        <p class="lt">{t("正在从 {agent} 会话加载历史…", { agent: agentLabel(app.agent) })}</p>
        <p class="ld dim">{t("适配器启动与历史回放可能需要几秒，请稍候")}</p>
      </div>
    {:else}
      {#if items.length === 0}
        <div class="empty">
          <p>{t("{agent} 会话（{context}）", { agent: agentLabel(app.agent), context: ctx?.name ?? "" })}</p>
          <p class="dim">{t("发送第一条消息开始工作。Agent 会读取项目目录并执行任务；")}<br/>{t("权限请求会弹出对话框，工具调用会实时展示。")}</p>
          {#if !sessionId}
            <p class="dim">{t("首次发送时将自动创建并绑定新会话。")}</p>
          {/if}
        </div>
      {/if}
      {#if topPad > 0}
        <div class="win-pad" style="height:{topPad}px"></div>
      {/if}
      {#if olderCount > 0}
        <button class="load-older" onclick={() => loadOlder()}>{t("加载更早的 {count} 条消息", { count: Math.min(CHUNK, olderCount) })}</button>
      {/if}
      {#each shownItems as item (item.id)}
        <div id={"msg-" + item.id}>
          <MessageItem {item} />
        </div>
      {/each}
      {#if newerHidden > 0}
        <button class="load-older" onclick={() => { endIdx = Math.min(items.length, endIdx + CHUNK); if (endIdx - startIdx > MAX_RENDER) startIdx = endIdx - MAX_RENDER; }}>{t("加载更新的 {count} 条消息", { count: Math.min(CHUNK, newerHidden) })}</button>
      {/if}
      {#if botPad > 0}
        <div class="win-pad" style="height:{botPad}px"></div>
      {/if}
      {#if loadingHere}
        <div class="syncing"><span class="chat-spinner sm"></span> {t("正在同步最新历史…")}</div>
      {/if}
      {#if streaming}
        <div class="thinking"><span class="dot accent pulse"></span> {t("{agent} 正在工作中…", { agent: agentLabel(app.agent) })}</div>
      {/if}
    {/if}
      </div>
    </div>
  </div>

  <div class="composer">
    <button class="btn ghost sm up up-prev" title={t("定位上一条发出的消息")} onclick={jumpToPrevUserMessage}>↑</button>
    <button class="btn ghost sm up" title={t("回到最新")} onclick={() => { stickToBottom = true; if (listEl) listEl.scrollTop = listEl.scrollHeight; }}>↓</button>
    {#if pendingImages.length}
      <div class="thumbs">
        {#each pendingImages as img, i}
          <div class="thumb">
            <img src={img.preview} alt={t("待发送图片")} />
            <button class="thumb-x" title={t("移除")} onclick={() => removeImage(i)}>✕</button>
          </div>
        {/each}
      </div>
    {/if}
    {#if searchOpen}
    <div class="chat-search">
      <input placeholder={t("搜索正文…")} bind:value={searchQuery} oninput={runSearch} onkeydown={onSearchKeydown} />
      <span class="count">{searchHits.length ? `${searchIdx + 1}/${searchHits.length}` : searchQuery ? "0/0" : ""}</span>
      <button class="btn ghost sm" title={t("上一个（Shift+Enter）")} onclick={() => stepSearch(-1)}>↑</button>
      <button class="btn ghost sm" title={t("下一个（Enter）")} onclick={() => stepSearch(1)}>↓</button>
      <button class="btn ghost sm" title={t("关闭（Esc）")} onclick={closeSearch}>✕</button>
    </div>
  {/if}

  <div class="cfg-bar">
      <label class="cfg">
        <span class="cfg-label" title={t("回合结束（完成/中断）后自动执行")}>⚙ {t("执行后动作")}</span>
        <select class="cfg-select" value={afterAction.kind} onchange={(e) => { afterAction.kind = (e.target as HTMLSelectElement).value as AfterActionKind; saveAfterAction(); }}>
          <option value="sound">{t("声音提示")}</option>
          <option value="shutdown">{t("关机")}</option>
          <option value="command">{t("执行命令")}</option>
        </select>
      </label>
      {#if afterAction.kind === "command"}
        <input
          class="cfg-cmd"
          placeholder={t("回合结束后执行的 cmd 命令，如 build.bat")}
          bind:value={afterAction.cmd}
          onchange={saveAfterAction}
          onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); saveAfterAction(); } }}
        />
      {/if}
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
    <div class="grow-handle" title={t("拖拽调整输入框高度")} onpointerdown={startResize}><span></span></div>
    <div class="input-row">
    <textarea
      rows="3"
      style={composerH ? `height:${composerH}px` : ""}
      placeholder={t("给 {agent} 下达任务…（{shortcut}，可粘贴图片）", { agent: agentLabel(app.agent), shortcut: app.enterSend ? t("Enter 发送，Shift+Enter 换行") : t("Enter 换行，Ctrl+Enter 发送") })}
      value={input}
      oninput={(e) => {
        input = (e.target as HTMLTextAreaElement).value;
        app.drafts[key] = input; // 未发送草稿按会话保留
      }}
      onkeydown={onKeydown}
      onpaste={onPaste}
      oncontextmenu={onInputContext}
    ></textarea>
    {#if streaming}
      <button class="btn danger" onclick={stop}>■ {t("停止")}</button>
    {:else}
      <button class="btn primary" disabled={(!input.trim() && !pendingImages.length) || sending || !ctx} onclick={send}>{t("发送")} ➤</button>
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
  .win-pad { flex: none; }
  /* 离屏消息跳过排版与绘制（contain-intrinsic-size 撑住滚动条），长会话不卡 */
  .msgs-inner > div[id^="msg-"] {
    content-visibility: auto;
    contain-intrinsic-size: auto 120px;
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
  .cfg-cmd {
    min-width: 220px;
    max-width: 380px;
    height: 26px;
    border: 1px solid var(--border-soft);
    border-radius: 6px;
    background: var(--bg-panel);
    color: var(--text);
    padding: 0 8px;
    font-family: var(--mono);
    font-size: 0.82em;
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
  .up-prev {
    bottom: calc(100% + 40px);
  }
  .load-older {
    margin: 10px auto 4px;
    display: block;
    padding: 5px 14px;
    font-size: 0.82em;
    color: var(--text-dim);
    background: var(--bg-elev);
    border: 1px solid var(--border-soft);
    border-radius: 999px;
  }
  .load-older:hover { color: var(--accent); border-color: var(--accent); }
</style>
