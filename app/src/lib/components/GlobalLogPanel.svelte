<script lang="ts">
  import { t } from "../i18n";
  // 全局日志：定时工作流触发 / 错过计划时间 / 执行后销毁 / 定时发送等应用级事件。
  import { app } from "../state.svelte";
  import { glog, clearGlobalLog, markGlobalLogRead } from "../global-log.svelte";
  import Icon from "./Icon.svelte";

  let filter = $state<"all" | "alert">("all");
  let q = $state("");
  let listEl = $state<HTMLDivElement | null>(null);

  const CAT: Record<string, string> = { schedule: "定时", workflow: "工作流", chat: "聊天", app: "应用" };

  $effect(() => {
    const open = app.overlay === "logs";
    glog.open = open;
    if (open) markGlobalLogRead();
  });

  const shown = $derived.by(() => {
    const kw = q.trim().toLowerCase();
    return glog.items
      .filter((e) => filter === "all" || e.level === "warn" || e.level === "error")
      .filter((e) => !kw || e.message.toLowerCase().includes(kw) || (CAT[e.category] ?? e.category).includes(kw))
      .slice()
      .reverse();
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && app.overlay === "logs") app.overlay = null;
  }
</script>

<svelte:window onkeydown={onKey} />

{#if app.overlay === "logs"}
  <div class="panel" role="dialog" aria-label={t("全局日志")}>
    <div class="head">
      <Icon name="log" size={15} /> {t("全局日志")}
      <span class="count">{glog.items.length}</span>
      <span class="spacer"></span>
      <div class="seg">
        <button class:on={filter === "all"} onclick={() => (filter = "all")}>{t("全部")}</button>
        <button class:on={filter === "alert"} onclick={() => (filter = "alert")}>{t("警告/错误")}</button>
      </div>
      <button class="btn ghost sm" title={t("清空日志")} disabled={!glog.items.length} onclick={() => void clearGlobalLog()}>{t("清空")}</button>
      <button class="btn ghost sm" onclick={() => (app.overlay = null)}>✕</button>
    </div>
    <div class="filter">
      <input class="input sm" placeholder={t("筛选日志…")} bind:value={q} />
    </div>
    <div class="list" bind:this={listEl}>
      {#each shown as e (e.id)}
        <div class="row lv-{e.level}">
          <span class="time">{e.time.slice(5)}</span>
          <span class="cat">{t(CAT[e.category] ?? e.category)}</span>
          <span class="msg">{e.message}</span>
        </div>
      {:else}
        <p class="none">{glog.items.length ? t("没有匹配的日志") : t("暂无日志")}</p>
      {/each}
    </div>
    <p class="tip">{t("记录本次启动以来的应用级事件：定时工作流触发与结果、错过的计划时间（含上次关闭期间）、单次工作流自动销毁、定时发送等。")}</p>
  </div>
{/if}

<style>
  .panel {
    position: fixed;
    top: 50px;
    right: 12px;
    width: 560px;
    max-width: calc(100vw - 24px);
    max-height: 72vh;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    z-index: 90;
    display: flex;
    flex-direction: column;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    font-weight: 600;
    border-bottom: 1px solid var(--border-soft);
  }
  .count {
    font-weight: 400;
    font-size: 0.8em;
    color: var(--text-faint);
  }
  .spacer {
    flex: 1;
  }
  .seg {
    display: flex;
    border: 1px solid var(--border-soft);
    border-radius: 7px;
    overflow: hidden;
    font-weight: 400;
  }
  .seg button {
    padding: 3px 9px;
    font-size: 0.8em;
    color: var(--text-dim);
  }
  .seg button.on {
    background: var(--bg-elev2);
    color: var(--accent);
  }
  .filter {
    padding: 8px 12px 0;
  }
  .filter .input {
    width: 100%;
  }
  .list {
    overflow: auto;
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-height: 80px;
  }
  .row {
    display: flex;
    gap: 8px;
    align-items: baseline;
    padding: 5px 8px;
    border-radius: 6px;
    font-size: 0.86em;
    border-left: 3px solid transparent;
  }
  .row:hover {
    background: var(--bg-elev);
  }
  .row.lv-warn {
    border-left-color: var(--warn);
  }
  .row.lv-error {
    border-left-color: var(--danger);
  }
  .row.lv-success {
    border-left-color: var(--ok, #3fb950);
  }
  .time {
    font-family: var(--mono);
    font-size: 0.85em;
    color: var(--text-faint);
    flex: none;
  }
  .cat {
    flex: none;
    font-size: 0.8em;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--bg-elev2);
    color: var(--text-dim);
  }
  .msg {
    flex: 1;
    min-width: 0;
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
  }
  .none {
    color: var(--text-faint);
    text-align: center;
    padding: 18px;
    font-size: 0.9em;
  }
  .tip {
    padding: 8px 14px 12px;
    color: var(--text-faint);
    font-size: 0.8em;
    border-top: 1px solid var(--border-soft);
    margin: 0;
  }
</style>
