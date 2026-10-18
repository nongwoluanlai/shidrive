<script lang="ts">
  import { tick } from "svelte";
  import { api, type RunSummary, type WorkflowSummary } from "./api";
  import { ui, currentProject, toast, errText } from "./store.svelte";
  import Icon from "./Icon.svelte";

  let loading = $state(false);
  let selectedId = $state<string | null>(null);
  let runs = $state<RunSummary[]>([]);
  let runsLoading = $state(false);
  let runId = $state<string | null>(null);
  let runLog = $state("");
  let runStatus = $state("");
  let logEl: HTMLPreElement | undefined = $state();
  let lastKey = "";

  const project = $derived(currentProject());
  const selected = $derived(ui.workflows.find((w) => w.id === selectedId) ?? null);

  async function load() {
    const pid = project?.id;
    if (!pid) { ui.workflows = []; return; }
    loading = true;
    try {
      const list = await api.workflows(pid);
      ui.workflows = list;
      for (const w of list) { if (w.running) ui.wfRunning[w.id] = w.running; else delete ui.wfRunning[w.id]; }
    } catch (e) {
      toast("error", errText(e));
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const k = `${project?.id ?? ""}|${ui.wfRev}`;
    if (k === lastKey) return;
    const projectChanged = !lastKey.startsWith(`${project?.id ?? ""}|`);
    lastKey = k;
    if (projectChanged) { selectedId = null; runId = null; }
    void load().then(() => { if (selectedId) void loadRuns(selectedId); });
  });

  async function loadRuns(id: string) {
    runsLoading = true;
    try { runs = await api.runs(id, 20); } catch (e) { toast("error", errText(e)); } finally { runsLoading = false; }
  }

  function pick(w: WorkflowSummary) {
    selectedId = w.id;
    runId = null;
    runs = [];
    void loadRuns(w.id);
  }

  async function openRun(id: string) {
    runId = id;
    runLog = "";
    try {
      const r = await api.run(id);
      if (runId !== id) return;
      runStatus = r.status;
      // 运行中的日志以实时流为准：快照 + 之后收到的增量
      const live = ui.wfLogs[id];
      runLog = live && live.length > r.log.length ? live : r.log;
      if (r.status === "running") ui.wfLogs[id] = runLog;
      await tick();
      if (logEl) logEl.scrollTop = logEl.scrollHeight;
    } catch (e) {
      toast("error", errText(e));
    }
  }

  // 实时日志
  $effect(() => {
    if (!runId) return;
    const live = ui.wfLogs[runId];
    if (live !== undefined && live.length >= runLog.length) {
      const atBottom = !logEl || logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 60;
      runLog = live;
      if (atBottom) void tick().then(() => { if (logEl) logEl.scrollTop = logEl.scrollHeight; });
    }
    const wfId = ui.workflows.find((w) => w.id === selectedId)?.id;
    if (wfId && runStatus === "running" && ui.wfRunning[wfId] !== runId) {
      runStatus = "";
      void openRun(runId);
      void loadRuns(wfId);
    }
  });

  async function run(w: WorkflowSummary) {
    if (!confirm(`在电脑上运行工作流「${w.name}」？`)) return;
    try {
      const r = await api.runWorkflow(w.id);
      ui.wfRunning[w.id] = r.runId;
      ui.wfLogs[r.runId] ??= "";
      toast("ok", "已开始运行");
      if (selectedId !== w.id) pick(w);
      runStatus = "running";
      void openRun(r.runId);
      void loadRuns(w.id);
    } catch (e) {
      toast("error", errText(e));
    }
  }

  async function stop(w: WorkflowSummary) {
    const rid = ui.wfRunning[w.id];
    if (!rid || !confirm(`停止正在运行的「${w.name}」？`)) return;
    try { await api.stopRun(rid); toast("info", "已请求停止"); } catch (e) { toast("error", errText(e)); }
  }

  const WEEK = ["", "一", "二", "三", "四", "五", "六", "日"];
  function trig(w: WorkflowSummary) {
    if (w.trigger_type !== "schedule" || !w.schedule) return "手动";
    const s = w.schedule;
    switch (s.kind) {
      case "interval": return `每 ${s.every_minutes} 分钟`;
      case "daily": return `每天 ${s.time}`;
      case "weekly": return `每周${(s.weekdays ?? []).map((d) => WEEK[d]).join("、")} ${s.time}`;
      case "once": return `单次 ${s.at}`;
      default: return "定时";
    }
  }
  const STATUS: Record<string, [string, string]> = {
    running: ["run", "运行中"], success: ["ok", "成功"], failed: ["err", "失败"], stopped: ["warn", "已停止"],
  };
  const KIND: Record<string, string> = { start: "开始", shell: "命令", python: "Python", agent: "Agent", prompt: "Agent", http: "HTTP", delay: "等待", condition: "条件", end: "结束" };
  const short = (t: string | null) => (t ? t.replace(/^\d{4}-/, "").slice(0, 14) : "—");
</script>

<div class="wf" class:has-detail={!!selected}>
  <div class="list-pane">
    <div class="bar">
      <b>工作流</b><span class="faint">{project?.name ?? ""}</span>
      <span class="grow"></span>
      <button class="icon-btn" title="刷新" onclick={() => void load()}>{#if loading}<span class="spin"></span>{:else}<Icon name="refresh" size={17} />{/if}</button>
    </div>
    <div class="cards scroll">
      {#each ui.workflows as w (w.id)}
        {@const running = !!ui.wfRunning[w.id]}
        <div class="card wcard" class:sel={selectedId === w.id} role="button" tabindex="0" onclick={() => pick(w)} onkeydown={(e) => e.key === "Enter" && pick(w)}>
          <div class="wtop">
            <span class="wname">{w.name}</span>
            {#if running}<span class="badge run"><span class="spin xs"></span>运行中</span>{:else if !w.enabled}<span class="badge">已停用</span>{/if}
          </div>
          {#if w.description}<div class="wdesc">{w.description}</div>{/if}
          <div class="wmeta">
            <span><Icon name="clock" size={13} />{trig(w)}</span>
            <span>{w.steps.filter((s) => s.kind !== "start").length} 个节点</span>
            {#if w.last_run_at}<span>上次 {short(w.last_run_at)}</span>{/if}
          </div>
          <div class="wops">
            {#if running}
              <button class="btn sm danger" onclick={(e) => { e.stopPropagation(); void stop(w); }}><Icon name="stop" size={14} />停止</button>
            {:else}
              <button class="btn sm primary" onclick={(e) => { e.stopPropagation(); void run(w); }}><Icon name="play" size={14} />运行</button>
            {/if}
          </div>
        </div>
      {:else}
        <div class="empty">{loading ? "加载中…" : "该项目还没有工作流"}</div>
      {/each}
    </div>
  </div>

  <div class="detail-pane">
    {#if !selected}
      <div class="empty"><Icon name="flow" size={30} /><div>选择工作流查看节点与运行记录</div></div>
    {:else}
      <div class="bar">
        <button class="icon-btn back" onclick={() => { if (runId) runId = null; else selectedId = null; }} aria-label="返回"><Icon name="back" /></button>
        <b class="ellipsis">{runId ? "运行日志" : selected.name}</b>
        <span class="grow"></span>
        {#if ui.wfRunning[selected.id]}
          <button class="btn sm danger" onclick={() => void stop(selected)}><Icon name="stop" size={14} />停止</button>
        {:else}
          <button class="btn sm primary" onclick={() => void run(selected)}><Icon name="play" size={14} />运行</button>
        {/if}
      </div>
      {#if runId}
        <div class="logwrap">
          <div class="loghead">
            <button class="btn sm ghost" onclick={() => (runId = null)}><Icon name="back" size={14} />返回记录</button>
            {#if STATUS[ui.wfRunning[selected.id] === runId ? "running" : runStatus]}
              {@const st = STATUS[ui.wfRunning[selected.id] === runId ? "running" : runStatus]}
              <span class="badge {st[0]}">{st[1]}</span>
            {/if}
          </div>
          <pre class="log mono scroll" bind:this={logEl}>{runLog || "（暂无日志）"}</pre>
        </div>
      {:else}
        <div class="scroll dbody">
          <div class="sec">
            <div class="sec-t">节点</div>
            <div class="steps">
              {#each selected.steps as s, i (i)}
                <div class="step"><span class="idx">{i + 1}</span><span class="badge">{KIND[s.kind] ?? s.kind}</span><span class="ellipsis">{s.name || "—"}</span></div>
              {/each}
            </div>
            <div class="faint tip">节点编辑请在电脑端进行</div>
          </div>
          <div class="sec">
            <div class="sec-t">运行记录 {#if runsLoading}<span class="spin xs"></span>{/if}</div>
            {#each runs as r (r.id)}
              {@const st = STATUS[ui.wfRunning[selected.id] === r.id ? "running" : r.status] ?? ["", r.status]}
              <button class="run" onclick={() => { runStatus = r.status; void openRun(r.id); }}>
                <span class="badge {st[0]}">{st[1]}</span>
                <span class="mono">{short(r.started_at)}</span>
                <span class="faint">{r.trigger === "schedule" ? "定时" : "手动"}</span>
                <span class="grow"></span>
                <Icon name="chevron" size={14} />
              </button>
            {:else}
              <div class="faint tip">暂无运行记录</div>
            {/each}
          </div>
        </div>
      {/if}
    {/if}
  </div>
</div>

<style>
  .wf { flex: 1; min-height: 0; display: grid; grid-template-columns: minmax(300px, 420px) 1fr; }
  .list-pane { display: flex; flex-direction: column; min-height: 0; border-right: 1px solid var(--border-soft); }
  .bar { display: flex; align-items: center; gap: 8px; padding: 8px 10px; min-height: 52px; border-bottom: 1px solid var(--border-soft); background: var(--bg-panel); }
  .grow { flex: 1; }
  .ellipsis { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .cards { flex: 1; min-height: 0; padding: 10px; display: flex; flex-direction: column; gap: 10px; }
  .wcard { padding: 12px 14px; cursor: pointer; display: flex; flex-direction: column; gap: 6px; transition: border-color .15s; }
  .wcard:hover { border-color: var(--border); }
  .wcard.sel { border-color: var(--accent); }
  .wtop { display: flex; align-items: center; gap: 8px; }
  .wname { font-weight: 600; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .wdesc { font-size: 13px; color: var(--text-dim); display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .wmeta { display: flex; flex-wrap: wrap; gap: 4px 12px; font-size: 12.5px; color: var(--text-faint); }
  .wmeta span { display: inline-flex; align-items: center; gap: 4px; }
  .wops { display: flex; justify-content: flex-end; }
  .spin.xs { width: 10px; height: 10px; border-width: 1.5px; }
  .detail-pane { display: flex; flex-direction: column; min-width: 0; min-height: 0; }
  .back { display: none; }
  .dbody { flex: 1; min-height: 0; padding: 12px 14px 24px; }
  .sec { margin-bottom: 18px; }
  .sec-t { font-size: 12.5px; color: var(--text-faint); margin-bottom: 8px; display: flex; align-items: center; gap: 6px; letter-spacing: .04em; }
  .steps { display: flex; flex-direction: column; gap: 6px; }
  .step { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-radius: 10px; background: var(--bg-panel); border: 1px solid var(--border-soft); font-size: 14px; }
  .idx { width: 22px; height: 22px; border-radius: 50%; background: var(--bg-elev2); display: grid; place-items: center; font-size: 12px; color: var(--text-dim); flex: none; }
  .tip { font-size: 12.5px; padding: 6px 2px; }
  .run { display: flex; align-items: center; gap: 10px; width: 100%; padding: 9px 10px; border-radius: 10px; text-align: left; font-size: 13.5px; color: var(--text-dim); }
  .run:hover { background: var(--bg-elev); }
  .logwrap { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .loghead { display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-bottom: 1px solid var(--border-soft); }
  .log { flex: 1; min-height: 0; margin: 0; padding: 10px 12px calc(10px + var(--safe-b)); background: var(--code-bg); font-size: 12.5px; line-height: 1.5; white-space: pre-wrap; word-break: break-all; user-select: text; }
  @media (max-width: 899px) {
    .wf { grid-template-columns: 1fr; }
    .list-pane { border-right: none; }
    .detail-pane { display: none; }
    .has-detail .list-pane { display: none; }
    .has-detail .detail-pane { display: flex; }
    .back { display: inline-flex; }
  }
</style>
