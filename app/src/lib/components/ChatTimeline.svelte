<script lang="ts">
  import { t } from "../i18n";
  // 会话时间线：按消息流定位的导航条，hover 预览发出消息，点击滚动定位。
  // 轮次多时（超过导航条能容纳的点数）改为「窗口」显示：只画当前位置附近的一段点，
  // 上下 ▲N / ▼N 表示窗口外还有多少条（点击翻页），滚轮在时间线上滚动也可翻页；
  // 窗口跟随消息区的阅读位置自动移动，左侧细条显示窗口在整场会话中的位置。
  import { untrack } from "svelte";
  import type { DisplayItem } from "../state.svelte";
  import SkinTimelineArt from "./SkinTimelineArt.svelte";

  let { items, onJump, scroller = null }: { items: DisplayItem[]; onJump: (id: string) => void; scroller?: HTMLElement | null } = $props();

  const markers = $derived(
    items
      .map((item, idx) => ({ item, idx }))
      .filter(({ item }) => item.kind === "user"),
  );
  const idxOf = $derived(new Map(items.map((it, i) => [it.id, i])));

  const STEP = 15; // 每个点占用的最小像素
  const IND = 16; // 上下溢出指示占用的像素
  const MIN_H = 160; // 非窗口模式最小高度（与 CSS min-height 一致）
  let tlH = $state(MIN_H);
  // 所在行（消息区一整列）的高度：窗口模式的判定只依赖它，不依赖时间线自身高度。
  // 旧逻辑用时间线自身高度判定，而窗口模式会把时间线从 160px 撑到 62%——
  // 用户消息数落在两者容量之间（约 11~28 条）时「窗口↔非窗口」每帧来回切换，
  // 宽度 18↔22px 跟着变，带动消息区重排、贴底重滚，整页持续抖动。
  let rowH = $state(0);
  let tlEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    const el = tlEl;
    if (!el) return;
    const row = el.parentElement;
    const ro = new ResizeObserver(() => {
      const h = el.clientHeight || MIN_H;
      if (h !== tlH) tlH = h;
      const rh = row?.clientHeight ?? 0;
      if (rh && Math.abs(rh - rowH) > 1) rowH = rh;
    });
    ro.observe(el);
    if (row) ro.observe(row);
    return () => ro.disconnect();
  });

  const n = $derived(markers.length);
  // 非窗口模式的最大可用高度 = 行高 55% - 上下 margin（与 CSS max-height 对应）
  const normMax = $derived(rowH ? Math.max(MIN_H, rowH * 0.55 - 20) : MIN_H);
  const fitAll = $derived(Math.floor(normMax / STEP));
  const windowed = $derived(n > Math.max(fitAll, 6));
  // 非窗口模式按点数给定高度（点距 >= STEP），上限由 CSS max-height 约束
  const normH = $derived(Math.max(MIN_H, n * STEP));
  const cap = $derived(windowed ? Math.max(4, Math.floor((tlH - 2 * IND) / STEP)) : n);

  // ---- 当前阅读位置（消息区滚动 → 当前所在的用户消息序号）----
  let active = $state(-1);
  function computeActive() {
    const sc = scroller;
    const list = markers;
    if (!sc || !list.length) {
      active = -1;
      return;
    }
    let idx = -1;
    if (sc.scrollTop + sc.clientHeight >= sc.scrollHeight - 4) {
      idx = items.length - 1;
    } else {
      const r = sc.getBoundingClientRect();
      const y = r.top + Math.min(120, r.height * 0.3);
      for (const el of sc.querySelectorAll<HTMLElement>('.msgs-inner > div[id^="msg-"]')) {
        if (el.getBoundingClientRect().bottom >= y) {
          idx = idxOf.get(el.id.slice(4)) ?? -1;
          break;
        }
      }
    }
    if (idx < 0) return;
    // 最后一条「消息序号 <= idx」的用户消息（二分）
    let lo = 0;
    let hi = list.length - 1;
    let ans = 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (list[mid].idx <= idx) {
        ans = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    active = ans;
  }
  let raf = 0;
  function schedule() {
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      computeActive();
    });
  }
  $effect(() => {
    const sc = scroller;
    if (!sc) return;
    sc.addEventListener("scroll", schedule, { passive: true });
    schedule();
    return () => {
      sc.removeEventListener("scroll", schedule);
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
    };
  });
  // 消息变化（新回合/切换会话）后重新计算
  $effect(() => {
    void items;
    schedule();
  });

  // ---- 窗口起点：跟随 active，手动翻页后保持到下一次阅读位置变化 ----
  let ws = $state(0);
  const maxWs = $derived(Math.max(0, n - cap));
  function clampWs(v: number) {
    return Math.max(0, Math.min(maxWs, v));
  }
  $effect(() => {
    const a = active;
    const c = cap;
    void n;
    if (!windowed) return;
    untrack(() => {
      const cur = clampWs(ws);
      const a2 = a < 0 ? n - 1 : a;
      // 当前项离窗口边缘不足 1 个点时，把窗口重新居中到当前项
      if (a2 < cur + 1 || a2 > cur + c - 2) ws = clampWs(a2 - Math.floor(c / 2));
      else ws = cur;
    });
  });
  const start = $derived(windowed ? clampWs(ws) : 0);
  const visible = $derived(markers.slice(start, start + cap).map((m, k) => ({ ...m, mi: start + k })));
  const above = $derived(start);
  const below = $derived(Math.max(0, n - start - visible.length));

  function page(dir: number) {
    ws = clampWs(start + dir * Math.max(1, cap - 1));
  }
  let wheelAcc = 0;
  function onWheel(e: WheelEvent) {
    if (!windowed) return;
    e.preventDefault();
    wheelAcc += e.deltaY;
    const stepPx = 40;
    const k = Math.trunc(wheelAcc / stepPx);
    if (!k) return;
    wheelAcc -= k * stepPx;
    ws = clampWs(start + k * Math.max(1, Math.floor(cap / 4)));
  }

  // 滚轮需要 preventDefault（不让消息区跟着滚），必须是非 passive 监听
  $effect(() => {
    const el = tlEl;
    if (!el) return;
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  });

  // 位置（百分比）：非窗口模式与原先一致（等距分布）；窗口模式在上下指示之间等距
  function posOf(k: number): string {
    if (!windowed) {
      const cnt = Math.max(n, 1);
      return `${Math.round(((k + 0.5) / cnt) * 100)}%`;
    }
    const cnt = Math.max(visible.length, 1);
    return `calc(${IND}px + (100% - ${2 * IND}px) * ${(k + 0.5) / cnt})`;
  }

  let hover = $state<{ top: number; left: number; text: string; time: string; no: string } | null>(null);
  function excerpt(text: string): string {
    const s = text.replace(/\s+/g, " ").trim();
    return s.length > 120 ? s.slice(0, 120) + "…" : s;
  }
  function onEnter(e: MouseEvent, m: { item: DisplayItem; mi: number }) {
    hover = {
      top: e.clientY,
      left: e.clientX,
      text: excerpt(m.item.text),
      time: (m.item.time ?? "").slice(5, 16),
      no: `${m.mi + 1}/${n}`,
    };
  }
  function onKey(e: KeyboardEvent, id: string) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onJump(id);
    }
  }
</script>

<div class="timeline" class:windowed style={windowed ? undefined : `height:${normH}px`} bind:this={tlEl} role="navigation" aria-label={t("会话时间线")}>
  <SkinTimelineArt />
  <div class="rail"></div>
  {#if windowed}
    <div class="track" aria-hidden="true">
      <div class="thumb" style="top:{(start / n) * 100}%; height:{Math.max(4, (visible.length / n) * 100)}%"></div>
    </div>
    <button class="more up" disabled={!above} title={t("前面还有 {n} 条，点击向前翻页（也可在时间线上滚动滚轮）", { n: above })} onclick={() => page(-1)}>
      {#if above}▲{above > 99 ? "99+" : above}{/if}
    </button>
    <button class="more down" disabled={!below} title={t("后面还有 {n} 条，点击向后翻页", { n: below })} onclick={() => page(1)}>
      {#if below}▼{below > 99 ? "99+" : below}{/if}
    </button>
  {/if}
  {#each visible as m, k (m.item.id)}
    <div
      class="mark"
      class:active={m.mi === active}
      style="top:{posOf(k)}"
      role="button"
      tabindex="0"
      aria-label={t("跳转到该消息")}
      onclick={() => onJump(m.item.id)}
      onkeydown={(e) => onKey(e, m.item.id)}
      onmouseenter={(e) => onEnter(e, m)}
      onmouseleave={() => (hover = null)}
    ></div>
  {/each}
</div>

{#if hover}
  <div class="preview" style="left:{Math.min(hover.left + 18, window.innerWidth - 360)}px; top:{Math.min(hover.top - 20, window.innerHeight - 140)}px">
    <div class="ptime">{hover.no}{#if hover.time} · {hover.time}{/if}</div>
    <div class="ptext">{hover.text || t("（空消息）")}</div>
  </div>
{/if}

<style>
  .timeline {
    position: relative;
    width: 18px;
    flex: none;
    margin: 10px 0;
    border-right: 1px solid var(--border-soft);
    /* 不随消息区拉满整列：限高居中，导航滑动距离更短 */
    align-self: center;
    max-height: 55%;
    min-height: 160px;
  }
  .timeline.windowed {
    /* 轮次多时适当加高以容纳更多点，仍不拉满整列 */
    width: 22px;
    height: 62%;
    max-height: 62%;
  }
  .rail {
    position: absolute;
    right: 5px;
    top: 0;
    bottom: 0;
    width: 2px;
    background: var(--skin-tl-rail, var(--border));
    border-radius: 2px;
    opacity: 0.6;
  }
  .track {
    position: absolute;
    left: 1px;
    top: 0;
    bottom: 0;
    width: 2px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--border) 50%, transparent);
  }
  .thumb {
    position: absolute;
    left: 0;
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
    opacity: 0.55;
    transition: top 0.15s;
  }
  .more {
    position: absolute;
    left: 3px;
    right: 0;
    height: 14px;
    padding: 0;
    border: none;
    background: none;
    color: var(--text-faint);
    font-size: 8px;
    line-height: 14px;
    text-align: center;
    cursor: pointer;
    white-space: nowrap;
    overflow: visible;
  }
  .more:hover:not(:disabled) {
    color: var(--accent);
  }
  .more:disabled {
    cursor: default;
  }
  .more.up {
    top: 0;
  }
  .more.down {
    bottom: 0;
  }
  .mark {
    position: absolute;
    right: 1px;
    width: 8px;
    height: 8px;
    transform: translateY(-50%);
    border-radius: 50%;
    background: var(--accent);
    border: 1.5px solid var(--bg);
    cursor: pointer;
    transition: transform 0.12s, top 0.15s;
    opacity: 0.75;
  }
  .mark::after {
    /* 加大悬停命中区，点小也能轻松指到 */
    content: "";
    position: absolute;
    inset: -6px;
  }
  .mark:hover {
    transform: translateY(-50%) scale(1.45);
    opacity: 1;
  }
  .mark.active {
    opacity: 1;
    transform: translateY(-50%) scale(1.3);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 35%, transparent);
  }
  .preview {
    position: fixed;
    left: 0;
    top: 0;
    max-width: 340px;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow);
    padding: 8px 12px;
    z-index: 50;
    pointer-events: none;
  }
  .ptime {
    color: var(--text-faint);
    font-size: 0.74em;
    margin-bottom: 3px;
  }
  .ptext {
    color: var(--text);
    font-size: 0.84em;
    line-height: 1.45;
    word-break: break-all;
    user-select: text;
  }
</style>
