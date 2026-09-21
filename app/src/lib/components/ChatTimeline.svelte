<script lang="ts">
  import { t } from "../i18n";
  // 会话时间线：按消息流定位的导航条，hover 预览发出消息，点击滚动定位。
  import type { DisplayItem } from "../state.svelte";
  import SkinTimelineArt from "./SkinTimelineArt.svelte";

  let { items, onJump }: { items: DisplayItem[]; onJump: (id: string) => void } = $props();

  const markers = $derived(
    items
      .map((item, idx) => ({ item, idx }))
      .filter(({ item }) => item.kind === "user"),
  );

  let hover = $state<{ top: number; left: number; text: string; time: string } | null>(null);

  // 等距分布：点只代表“第几条用户消息”，间距均匀、条密度高，
  // 鼠标滑动距离短，不用拖着找位置。
  function posOf(i: number): number {
    const n = Math.max(markers.length, 1);
    return Math.round(((i + 0.5) / n) * 100);
  }

  function excerpt(text: string): string {
    const t = text.replace(/\s+/g, " ").trim();
    return t.length > 120 ? t.slice(0, 120) + "…" : t;
  }

  function onEnter(e: MouseEvent, m: { item: DisplayItem; idx: number }) {
    hover = {
      top: e.clientY,
      left: e.clientX,
      text: excerpt(m.item.text),
      time: (m.item.time ?? "").slice(5, 16),
    };
  }
</script>

<div class="timeline" role="navigation" aria-label={t("会话时间线")}>
  <SkinTimelineArt />
  <div class="rail"></div>
  {#each markers as m, mi (m.item.id)}
    <div
      class="mark"
      class:done={m.item.kind === "user"}
      style="top:{posOf(mi)}%"
      role="button"
      tabindex="0"
      aria-label={t("跳转到该消息")}
      onclick={() => onJump(m.item.id)}
      onmouseenter={(e) => onEnter(e, m)}
      onmouseleave={() => (hover = null)}
    ></div>
  {/each}
</div>

{#if hover}
  <div class="preview" style="left:{Math.min(hover.left + 18, window.innerWidth - 360)}px; top:{Math.min(hover.top - 20, window.innerHeight - 140)}px">
    {#if hover.time}<div class="ptime">{hover.time}</div>{/if}
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
  .rail {
    position: absolute;
    right: 5px;
    top: 0;
    bottom: 0;
    width: 2px;
    background: var(--border);
    border-radius: 2px;
    opacity: 0.6;
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
    transition: transform 0.12s;
  }
  .mark::after {
    /* 加大悬停命中区，点小也能轻松指到 */
    content: "";
    position: absolute;
    inset: -6px;
  }
  .mark:hover {
    transform: translateY(-50%) scale(1.45);
  }
  .empty {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%) rotate(-90deg);
    color: var(--text-faint);
    font-size: 0.66em;
    white-space: nowrap;
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
