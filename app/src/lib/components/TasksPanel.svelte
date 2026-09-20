<script lang="ts">
  // 运行中任务: currently executing workflow runs with stop + log.
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import Icon from "./Icon.svelte";

  interface ActiveRun {
    run_id: string;
    workflow_id: string;
    name: string;
    started_at: string;
    log: string;
    trigger: string;
  }

  let runs = $state<ActiveRun[]>([]);
  let timer: ReturnType<typeof setInterval> | null = null;
  let openRun = $state<string | null>(null);
  let logs = $state<Record<string, string>>({});

  async function refresh() {
    try {
      const active = await api.activeRuns();
      runs = active.map(([run_id, workflow_id, name, started_at]) => ({
        run_id,
        workflow_id,
        name,
        started_at,
        log: logs[run_id] ?? "",
        trigger: "",
      }));
      for (const r of runs) {
        if (!logs[r.run_id]) {
          const hist = await api.runsList(r.workflow_id, 5).catch(() => []);
          const match = hist.find((h) => h.id === r.run_id);
          if (match) logs[r.run_id] = match.log;
        }
      }
    } catch {
      /* ignore */
    }
  }

  $effect(() => {
    if (app.overlay !== "tasks") return;
    void refresh();
    timer = setInterval(refresh, 2500);
    return () => {
      if (timer) clearInterval(timer);
    };
  });

  async function stop(run_id: string) {
    try {
      await api.workflowStop(run_id);
      toast("ok", "已发送停止请求");
    } catch (e) {
      toast("error", String(e));
    }
  }
</script>

{#if app.overlay === "tasks"}
  <div class="panel" role="dialog" aria-label="运行中任务">
    <div class="head">
      <Icon name="tasks" size={15} /> 运行中任务
      <span class="spacer"></span>
      <button class="btn ghost sm" onclick={() => (app.overlay = null)}>✕</button>
    </div>
    <div class="list">
      {#each runs as r (r.run_id)}
        <div class="row">
          <div class="info">
            <div class="l1"><span class="dot accent pulse"></span><b>{r.name}</b></div>
            <div class="l2 dim">开始于 {r.started_at.slice(5, 16)}</div>
          </div>
          <div class="acts">
            <button class="btn sm" onclick={() => (openRun = openRun === r.run_id ? null : r.run_id)}>{openRun === r.run_id ? "收起日志" : "日志"}</button>
            <button class="btn sm danger" onclick={() => stop(r.run_id)}>■ 停止</button>
          </div>
        </div>
        {#if openRun === r.run_id}
          <pre class="rlog">{logs[r.run_id] || "（暂无日志）"}</pre>
        {/if}
      {:else}
        <p class="none">当前没有运行中的任务</p>
      {/each}
    </div>
    <p class="tip">定时与手动运行的工作流会在这里实时显示；也可在「工作流」页查看历史。</p>
  </div>
{/if}

<style>
  .panel {
    position: fixed;
    top: 50px;
    right: 12px;
    width: 520px;
    max-height: 70vh;
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
  .spacer {
    flex: 1;
  }
  .list {
    overflow: auto;
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--border-soft);
    border-radius: 8px;
  }
  .info {
    flex: 1;
    min-width: 0;
  }
  .l1 {
    display: flex;
    gap: 8px;
    align-items: center;
    font-size: 0.92em;
  }
  .l2 {
    font-size: 0.8em;
    margin-top: 2px;
  }
  .dim {
    color: var(--text-faint);
  }
  .acts {
    display: flex;
    gap: 4px;
    flex: none;
  }
  .rlog {
    font-family: var(--mono);
    font-size: 0.78em;
    background: var(--code-bg);
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    padding: 10px 12px;
    max-height: 220px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-all;
    user-select: text;
    margin: 0;
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
  }
</style>
