<script lang="ts">
  // 图形化工作流：SVG 节点画布（并行分支、自由连线）+ 触发按钮 + 运行历史右列。
  import { onMount, untrack } from "svelte";
  import { app, currentProject, refreshWorkflows, toast } from "../state.svelte";
  import { api } from "../ipc";
  import type { Edge, ScheduleConfig, Workflow, WorkflowRun, WorkflowStep } from "../types";
  import { scheduleText } from "./wf-shared";
  import { confirmDialog, promptDialog } from "../dialog.svelte";
  import { firstLineOf } from "./wf-firstline";
  import ContextMenu from "./ContextMenu.svelte";
  import type { MenuItem } from "./menu-item";

  const project = $derived(currentProject());
  const workflows = $derived(app.workflows);
  let contexts = $state<{ id: string; name: string }[]>([]);
  let runs = $state<WorkflowRun[]>([]);
  let openRun = $state<string | null>(null);
  let selectedStep = $state<number | null>(null);
  let propsOpen = $state(false);
  let selectedEdge = $state<number | null>(null);
  let clipboard: WorkflowStep | null = null;
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  let triggerOpen = $state(false);
  let dlgEnvKey = $state("");
  let dlgEnvVal = $state("");
  const HINT_ENV = "{{env.X}}";
  const HINT_DATE = "{{date:yyyy-MM-dd}}";
  let nodeDlg = $state<number | null>(null);

  const selected = $derived(app.workflows.find((w) => w.id === app.workflowSelected) ?? null);
  const running = $derived(app.workflowSelected ? !!app.wfRunning[app.workflowSelected] : false);

  // 删除联动：侧栏或本页删除后 app.workflows 更新，这里自动清掉失效选中
  $effect(() => {
    const id = app.workflowSelected;
    if (id && !app.workflows.find((w) => w.id === id)) {
      app.workflowSelected = null;
      runs = [];
    }
  });

  // 选中变化时重载运行历史 + 重置自动保存基线
  let savedSnap = $state("");
  function snapOf(w: Workflow | null): string {
    if (!w) return "";
    return JSON.stringify({ n: w.name, d: w.description, en: w.enabled, t: w.trigger_type, s: w.schedule, st: w.steps, ed: w.edges, env: w.env });
  }
  $effect(() => {
    const id = app.workflowSelected;
    openRun = null;
    selectedStep = null;
    propsOpen = false;
    if (!id) {
      runs = [];
      savedSnap = "";
      return;
    }
    // 只以“选中 id”为依赖：基线快照与历史拉取都不追踪工作流深层内容，
    // 否则步骤一变更基线就被改写，自动保存永远看不到差异。
    savedSnap = untrack(() => {
      const w = app.workflows.find((x) => x.id === id);
      return snapOf(w ?? null);
    });
    untrack(() => {
      void (async () => {
        try {
          runs = await api.runsList(id);
        } catch (e) {
          toast("error", String(e));
        }
      })();
    });
  });

  // 自动保存：内容与基线不一致时 800ms 防抖保存
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const w = selected;
    if (!w || !savedSnap) return;
    const snap = snapOf(w);
    if (snap === savedSnap) return;
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(async () => {
      try {
        await api.workflowUpdate(w);
        savedSnap = snapOf(w);
      } catch (e) {
        toast("error", "自动保存失败: " + String(e));
      }
    }, 800);
    return () => {
      if (saveTimer) clearTimeout(saveTimer);
    };
  });

  $effect(() => {
    if (project) void refresh();
  });

  async function refresh() {
    if (!project) return;
    try {
      await refreshWorkflows();
      contexts = (await api.contextsList(project.id)).map((c) => ({ id: c.id, name: c.name }));
      if (app.workflowSelected && !app.workflows.find((w) => w.id === app.workflowSelected)) app.workflowSelected = null;
      if (!app.workflowSelected && app.workflows.length) app.workflowSelected = app.workflows[0].id;
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function createWorkflow() {
    if (!project) return;
    const name = await promptDialog({ title: "新建工作流", label: "工作流名称 *" });
    if (name === null || !name.trim()) return;
    try {
      const w = await api.workflowCreate({
        project_id: project.id,
        name: name.trim(),
        description: "",
        enabled: true,
        trigger_type: "manual",
        schedule: null,
        steps: [{ type: "start", name: "", x: 60, y: 40 }],
      });
      await refresh();
      app.workflowSelected = w.id;
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function save() {
    if (!selected) return;
    try {
      await api.workflowUpdate(selected);
      await refreshWorkflows();
      toast("ok", "已保存");
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function del() {
    if (!selected || !(await confirmDialog({ title: "删除工作流", message: `删除工作流「${selected.name}」？运行历史将一并删除。`, danger: true, confirmText: "删除" }))) return;
    try {
      await api.workflowDelete(selected.id);
      app.workflowSelected = null;
      await refresh();
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function runNow() {
    if (!selected) return;
    try {
      await api.workflowRun(selected.id);
      toast("ok", "已启动");
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function stopRun() {
    if (!selected) return;
    const rid = app.wfRunning[selected.id];
    if (!rid) return;
    try {
      await api.workflowStop(rid);
    } catch (e) {
      toast("error", String(e));
    }
  }

  // ---------- steps / canvas ----------
  const NODE_W = 210;
  const NODE_H = 54;
  // 端口上下分布：输入=顶部中点(y+0)，输出=底部中点(y+NODE_H)
  const nodeW = (st: WorkflowStep) => (st.type === "note" || st.type === "env" ? 290 : NODE_W);

  function ensurePositions(w: Workflow) {
    // 旧数据兼容：补齐气泡节点新字段的默认值
    for (const st of w.steps) {
      if (st.type === "balloon") {
        if (st.click_action === undefined) st.click_action = "none";
        if (st.click_target === undefined) st.click_target = "";
        if (st.sound === undefined) st.sound = false;
      }
    }
    w.steps.forEach((s, i) => {
      if (!s.x && !s.y) {
        s.x = 40;
        s.y = 30 + i * (NODE_H + 46);
      }
    });
  }

  $effect(() => {
    if (selected) ensurePositions(selected);
  });

  function addStep(type: WorkflowStep["type"]) {
    if (!selected) return;
    if (type === "start" && selected.steps.some((s) => s.type === "start")) {
      toast("warn", "已存在「开始」节点（工作流从该节点起跑）");
      return;
    }
    const i = selected.steps.length;
    const base = { x: 60, y: 30 + i * (NODE_H + 46) };
    if (type === "shell")
      selected.steps.push({ type, name: "", command: "echo hello", cwd: "", shell: "cmd", timeout_sec: null, continue_on_error: false, ...base });
    else if (type === "agent") {
      if (!contexts.length) {
        toast("warn", "请先创建上下文，Agent 节点需要绑定上下文");
        return;
      }
      selected.steps.push({ type, name: "", context_id: contexts[0].id, agent_type: "codex", prompt: "", session_id: null, timeout_sec: null, continue_on_error: false, ...base });
    } else if (type === "delay") selected.steps.push({ type, name: "", seconds: 5, ...base });
    else if (type === "env") selected.steps.push({ type, name: "", vars: {}, ...base });
    else if (type === "start") selected.steps.unshift({ type, name: "", ...{ x: 60, y: 20 } });
    else if (type === "note") selected.steps.push({ type, name: "", text: "说明…", ...base });
    else if (type === "balloon") selected.steps.push({ type, name: "", title: "提醒", message: "", click_action: "none", click_target: "", sound: false, ...base });
    const idx = type === "start" ? 0 : selected.steps.length - 1;
    const source = selected.steps.findIndex((_, k) => k !== idx && !hasOutgoing(selected, k));
    if (source >= 0 && source !== idx) addEdge(selected, source, idx);
    selectedStep = idx;
    nodeDlg = idx;
  }

  function hasOutgoing(w: Workflow, i: number): boolean {
    return w.edges.some((e) => e.from === i);
  }

  function reaches(w: Workflow, from: number, target: number, seen: Set<number> = new Set()): boolean {
    if (from === target) return true;
    if (seen.has(from)) return false;
    seen.add(from);
    return w.edges.filter((e) => e.from === from).some((e) => reaches(w, e.to, target, seen));
  }

  function addEdge(w: Workflow, from: number, to: number) {
    if (from === to) return;
    if (reaches(w, to, from)) {
      toast("warn", "不能创建循环连线");
      return;
    }
    if (w.edges.some((e) => e.from === from && e.to === to)) return;
    w.edges.push({ from, to });
  }

  function autoChain(w: Workflow) {
    w.steps.forEach((_, i) => {
      if (i > 0 && !w.edges.some((e) => e.from === i - 1 && e.to === i)) w.edges.push({ from: i - 1, to: i });
    });
  }

  function removeEdgeIdx(i: number) {
    if (!selected) return;
    selected.edges.splice(i, 1);
    selectedEdge = null;
  }

  function removeStep(i: number) {
    if (!selected) return;
    selected.steps.splice(i, 1);
    selected.edges = selected.edges
      .filter((e) => e.from !== i && e.to !== i)
      .map((e) => ({ from: e.from > i ? e.from - 1 : e.from, to: e.to > i ? e.to - 1 : e.to }));
    selectedStep = null;
    selectedEdge = null;
    nodeDlg = null;
  }

  function swapStep(i: number, j: number) {
    if (!selected || j < 0 || j >= selected.steps.length || i === j) return;
    // swap steps and remap edges so connections follow the nodes
    const steps = selected.steps;
    const edges = selected.edges;
    const remap = (x: number) => (x === i ? j : x === j ? i : x);
    selected.edges = edges.map((e) => ({ from: remap(e.from), to: remap(e.to) }));
    const tmp = steps[i];
    steps[i] = steps[j];
    steps[j] = tmp;
  }

  // ---------- drag handling (window-level so pointerup is always seen) ----------
  let dragging = $state<number | null>(null);
  let dragOff = { x: 0, y: 0 };
  let canvasEl: HTMLDivElement | null = $state(null);

  function nodeDown(e: PointerEvent, i: number) {
    if (!selected || e.button !== 0) return;
    selectedStep = i;
    selectedEdge = null;
    const s = selected.steps[i];
    dragging = i;
    // step.x/y are canvas-local: convert the viewport point via the canvas rect
    const rect = canvasEl?.getBoundingClientRect();
    const px = rect ? e.clientX - rect.left : e.clientX;
    const py = rect ? e.clientY - rect.top : e.clientY;
    dragOff = { x: px - s.x, y: py - s.y };
    window.addEventListener("pointermove", onWinMove);
    window.addEventListener("pointerup", onWinUp, { once: true });
  }

  function onWinMove(e: PointerEvent) {
    if (dragging === null || !selected) return;
    const s = selected.steps[dragging];
    const rect = canvasEl?.getBoundingClientRect();
    const px = rect ? e.clientX - rect.left : e.clientX;
    const py = rect ? e.clientY - rect.top : e.clientY;
    s.x = Math.max(0, px - dragOff.x);
    s.y = Math.max(0, py - dragOff.y);
  }

  function onWinUp() {
    dragging = null;
    window.removeEventListener("pointermove", onWinMove);
  }

  // ---------- connecting ----------
  let connecting = $state<number | null>(null);
  let mouse = $state({ x: 0, y: 0 });

  function onConnectMove(e: PointerEvent) {
    if (connecting === null) return;
    const rect = canvasEl?.getBoundingClientRect();
    if (rect) mouse = { x: e.clientX - rect.left, y: e.clientY - rect.top };
  }

  function onConnectUp(e: PointerEvent) {
    window.removeEventListener("pointermove", onConnectMove);
    const from = connecting;
    connecting = null;
    if (from === null || !selected) return;
    const el = document.elementFromPoint(e.clientX, e.clientY) as HTMLElement | null;
    const nodeEl = el?.closest(".node") as HTMLElement | null;
    if (nodeEl?.dataset.index) addEdge(selected, from, Number(nodeEl.dataset.index));
  }

  function portDown(e: PointerEvent, i: number) {
    e.stopPropagation();
    connecting = i;
    const rect = canvasEl?.getBoundingClientRect();
    mouse = rect ? { x: e.clientX - rect.left, y: e.clientY - rect.top } : { x: 0, y: 0 };
    window.addEventListener("pointermove", onConnectMove);
    window.addEventListener("pointerup", onConnectUp, { once: true });
  }

  // 正交折线：上游底部中点 → 垂直 → 水平 → 垂直 → 下游顶部中点（固定端口）
  function edgePath(a: WorkflowStep, b: WorkflowStep): string {
    const x1 = a.x + nodeW(a) / 2, y1 = a.y + NODE_H, x2 = b.x + nodeW(b) / 2, y2 = b.y;
    if (Math.abs(x1 - x2) < 2) return `M ${x1} ${y1} L ${x2} ${y2}`;
    const my = Math.round((y1 + y2) / 2);
    return `M ${x1} ${y1} L ${x1} ${my} L ${x2} ${my} L ${x2} ${y2}`;
  }

  const edges = $derived.by(() => {
    if (!selected) return [];
    return selected.edges
      .filter((e) => e.from < selected.steps.length && e.to < selected.steps.length)
      .map((e) => ({ e, d: edgePath(selected.steps[e.from], selected.steps[e.to]) }));
  });

  // ---------- context menus ----------
  function nodeContextMenu(e: MouseEvent, i: number) {
    e.preventDefault();
    e.stopPropagation();
    selectedStep = i;
    selectedEdge = null;
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: "✏️ 编辑节点", run: () => { selectedStep = i; nodeDlg = i; } },
        { label: "⧉ 复制节点", run: () => copyStep(i) },
        ...(clipboard ? [{ label: "📋 粘贴节点", run: () => pasteStep() }] : []),
        { label: "⚙ 自动重排全部节点", run: () => { if (selected) { autoLayout(selected); toast("ok", "已按连线自动重排"); } } },
        "sep",
        { label: "🗑 删除节点", danger: true, run: () => removeStep(i) },
      ],
    };
  }

  function copyStep(i: number) {
    if (!selected) return;
    clipboard = JSON.parse(JSON.stringify(selected.steps[i]));
    toast("ok", "节点已复制，画布空白处右键粘贴或 Ctrl+V");
  }

  function pasteStep(atX?: number, atY?: number) {
    if (!selected || !clipboard) return;
    const step: WorkflowStep = JSON.parse(JSON.stringify(clipboard));
    const i = selected.steps.length;
    const pos = { x: (atX ?? 60) + 24, y: (atY ?? 60) + 24 };
    if (step.type === "shell" || step.type === "agent" || step.type === "delay" || step.type === "env") Object.assign(step, pos);
    const source = selected.steps.findIndex((_, k) => !hasOutgoing(selected, k));
    selected.steps.push(step);
    if (source >= 0 && source !== i) addEdge(selected, source, i);
    selectedStep = i;
    nodeDlg = i;
  }

  function edgeContextMenu(e: MouseEvent, i: number) {
    e.preventDefault();
    e.stopPropagation();
    selectedEdge = i;
    selectedStep = null;
    menu = { x: e.clientX, y: e.clientY, items: [{ label: "🗑 删除连线", danger: true, run: () => removeEdgeIdx(i) }] };
  }

  // 自动重排：按连线拓扑分层，从左到右、层内自上而下
  function autoLayout(w: Workflow) {
    const n = w.steps.length;
    const indeg = Array(n).fill(0);
    const outs: number[][] = Array.from({ length: n }, () => []);
    for (const e of w.edges) {
      if (e.from < n && e.to < n) {
        outs[e.from].push(e.to);
        indeg[e.to] += 1;
      }
    }
    const depth = Array(n).fill(-1);
    const roots = w.steps.map((_, i) => i).filter((i) => w.steps[i].type === "start" || indeg[i] === 0);
    const queue = roots.length ? [...new Set(roots)] : [0];
    queue.forEach((i) => (depth[i] = 0));
    const ordered: number[] = [];
    while (queue.length) {
      const i = queue.shift()!;
      ordered.push(i);
      for (const t of outs[i]) {
        const nd = depth[i] + 1;
        if (depth[t] < 0 || nd > depth[t]) depth[t] = nd;
        if (!ordered.includes(t) && !queue.includes(t)) queue.push(t);
      }
    }
    w.steps.forEach((_, i) => {
      if (!ordered.includes(i)) ordered.push(i);
    });
    const perLayer: Record<number, number[]> = {};
    for (const i of ordered) {
      const d = Math.max(0, depth[i] < 0 ? 0 : depth[i]);
      (perLayer[d] = perLayer[d] || []).push(i);
    }
    // 全局中心对齐规则：以最宽的一行决定内容水平中心，其余每行（单块或并行）
    // 累加块宽+间隙得到行宽，围绕同一中心对称摆放——单链的中线自然对齐，
    // 并行行的中心与上一行的中心重合，开始节点随全局中心定位。
    const GAP = 70;
    const layers = Object.keys(perLayer).map(Number).sort((a, b) => a - b);
    const rowWidth = (idxs: number[]) =>
      idxs.reduce((acc, i) => acc + nodeW(w.steps[i]), 0) + (idxs.length - 1) * GAP;
    const maxRow = Math.max(...layers.map((d) => rowWidth(perLayer[d])));
    layers.forEach((d) => {
      const idxs = perLayer[d];
      const rowW = rowWidth(idxs);
      let x = 60 + Math.round((maxRow - rowW) / 2);
      idxs.forEach((i) => {
        w.steps[i].x = x;
        w.steps[i].y = 40 + d * (NODE_H + 90);
        x += nodeW(w.steps[i]) + GAP;
      });
    });
  }

  function canvasContextMenu(e: MouseEvent) {
    e.preventDefault();
    const rect = canvasEl?.getBoundingClientRect();
    const px = rect ? e.clientX - rect.left : 60;
    const py = rect ? e.clientY - rect.top : 60;
    const items: MenuItem[] = [];
    if (selected) items.push({ label: "⚙ 自动重排全部节点", run: () => { autoLayout(selected); toast("ok", "已按连线自动重排"); } });
    if (clipboard) items.push({ label: "⧉ 粘贴节点", run: () => pasteStep(px, py) });
    items.push(
      { label: "🔗 顺序连线", run: () => { if (selected) autoChain(selected); } },
      { label: "🧹 清空连线", run: () => { if (selected) { selected.edges = []; selectedEdge = null; } } },
    );
    menu = { x: e.clientX, y: e.clientY, items };
  }

  // ---------- keyboard ----------
  function isFormTarget(e: KeyboardEvent): boolean {
    const t = e.target as HTMLElement | null;
    return t?.tagName === "INPUT" || t?.tagName === "TEXTAREA" || t?.tagName === "SELECT" || t?.isContentEditable === true;
  }

  function onCanvasKeydown(e: KeyboardEvent) {
    if (isFormTarget(e)) return;
    if (e.key === "Escape" && nodeDlg !== null) {
      nodeDlg = null;
      return;
    }
    if (!selected) return;
    if (e.key === "Delete") {
      e.preventDefault();
      if (selectedEdge !== null) removeEdgeIdx(selectedEdge);
      else if (selectedStep !== null) removeStep(selectedStep);
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "c") {
      if (selectedStep !== null) {
        e.preventDefault();
        copyStep(selectedStep);
      }
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") {
      if (clipboard) {
        e.preventDefault();
        pasteStep();
      }
    } else if (e.key === "Escape") {
      selectedEdge = null;
      propsOpen = false;
    }
  }

  function setScheduleKind(kind: string) {
    if (!selected) return;
    selected.trigger_type = kind as "manual" | "schedule";
    if (kind === "schedule" && !selected.schedule) {
      selected.schedule = { kind: "daily", time: "09:00" };
      selected.enabled = true;
    }
    if (kind === "manual") selected.enabled = true;
  }

  function setSchedKind(kind: string) {
    if (!selected) return;
    const base = (selected.schedule as any) ?? {};
    if (kind === "interval") selected.schedule = { kind: "interval", every_minutes: base.every_minutes ?? 30 };
    else if (kind === "daily") selected.schedule = { kind: "daily", time: base.time ?? "09:00" };
    else if (kind === "weekly") selected.schedule = { kind: "weekly", weekdays: base.weekdays ?? [1, 2, 3, 4, 5], time: base.time ?? "09:00" };
    else if (kind === "once") selected.schedule = { kind: "once", at: base.at ?? "" };
  }

  const WEEKDAYS = [
    { v: 1, n: "一" },
    { v: 2, n: "二" },
    { v: 3, n: "三" },
    { v: 4, n: "四" },
    { v: 5, n: "五" },
    { v: 6, n: "六" },
    { v: 7, n: "日" },
  ];

  // ---------- misc ----------
  const stepLabel: Record<string, string> = { start: "开始", note: "注释", balloon: "气泡", shell: "命令", agent: "Agent", delay: "等待", env: "变量" };
  const stepIcon: Record<string, string> = { start: "🚩", note: "📝", balloon: "🔔", shell: "▶", agent: "🤖", delay: "⏱", env: "🔧" };

  function envNewKey(step: { vars: Record<string, string> }): string {
    const base = "KEY";
    let i = 1;
    while (step.vars?.[base + (i > 1 ? i : "")] !== undefined) i++;
    return i > 1 ? base + i : base;
  }

  function envOutName(st: { name: string }, i: number): string {
    const key = st.name || "step" + (i + 1);
    return "{{env." + key + "_stdout}} / _stderr / _exit";
  }

  onMount(() => {
    window.addEventListener("keydown", onCanvasKeydown);
    return () => {
      window.removeEventListener("keydown", onCanvasKeydown);
    };
  });

  // wf://log 与 wf://status 的监听在 events.ts 全局注册（切页不丢状态）。
  // 这里只做视图联动：新运行展开日志；运行结束后刷新历史。
  let prevRunId: string | null = null;
  $effect(() => {
    const wfId = app.workflowSelected;
    const cur = wfId ? app.wfRunning[wfId] ?? null : null;
    if (cur) {
      openRun = cur;
    } else if (prevRunId && wfId) {
      void api
        .runsList(wfId)
        .then((rs) => {
          runs = rs;
          const r = rs.find((x) => x.id === prevRunId);
          if (r && !app.wfLiveLogs[r.id]) app.wfLiveLogs[r.id] = r.log;
        })
        .catch(() => {});
    }
    prevRunId = cur;
  });
</script>

<div class="wfview">
  {#if project}
    <div class="detail">
      {#if selected}
        <div class="head">
          <input class="name-input" bind:value={selected.name} placeholder="工作流名称" />
          {#if selected.trigger_type === "schedule" && selected.enabled}
            <span class="badge accent" title="下次触发">⏰ {scheduleText(selected.schedule)} · 下次 {selected.next_run_at ?? "—"}</span>
          {:else}
            <span class="badge">手动</span>
          {/if}
          <div class="trigger-wrap">
            <button class="btn" onclick={() => (triggerOpen = !triggerOpen)}>
              ⚡ 触发：{selected.trigger_type === "schedule" ? (selected.enabled ? scheduleText(selected.schedule) : "已停用") : "手动"}
            </button>
            {#if triggerOpen}
              <div class="popover">
                <div class="row">
                  <label><input type="radio" checked={selected.trigger_type === "manual"} onchange={() => setScheduleKind("manual")} /> 手动</label>
                  <label><input type="radio" checked={selected.trigger_type === "schedule"} onchange={() => setScheduleKind("schedule")} /> 定时</label>
                  <label class="toggle"><input type="checkbox" bind:checked={selected.enabled} disabled={selected.trigger_type !== "schedule"} /> 启用</label>
                </div>
                {#if selected.trigger_type === "schedule"}
                  <div class="row sched">
                    <select value={selected.schedule?.kind ?? "daily"} onchange={(e) => setSchedKind((e.target as HTMLSelectElement).value)}>
                      <option value="interval">固定间隔</option>
                      <option value="daily">每天</option>
                      <option value="weekly">每周</option>
                      <option value="once">单次</option>
                    </select>
                    {#if selected.schedule?.kind === "interval"}
                      <span>每</span>
                      <input class="num" type="number" min="1" bind:value={(selected.schedule as any).every_minutes} />
                      <span>分钟</span>
                    {:else if selected.schedule?.kind === "daily"}
                      <input class="time" type="time" bind:value={(selected.schedule as any).time} />
                    {:else if selected.schedule?.kind === "weekly"}
                      {#each WEEKDAYS as d (d.v)}
                        <label class="wd" class:on={((selected.schedule as any).weekdays ?? []).includes(d.v)}>
                          <input
                            type="checkbox"
                            checked={((selected.schedule as any).weekdays ?? []).includes(d.v)}
                            onchange={(e) => {
                              const arr: number[] = ((selected.schedule as any).weekdays ??= []);
                              const i = arr.indexOf(d.v);
                              if ((e.target as HTMLInputElement).checked && i < 0) arr.push(d.v);
                              if (!(e.target as HTMLInputElement).checked && i >= 0) arr.splice(i, 1);
                            }}
                          />{d.n}
                        </label>
                      {/each}
                      <input class="time" type="time" bind:value={(selected.schedule as any).time} />
                    {:else if selected.schedule?.kind === "once"}
                      <input class="time" type="datetime-local" bind:value={(selected.schedule as any).at} />
                    {/if}
                  </div>
                  <p class="next">下次运行：{selected.next_run_at ?? "—"}</p>
                {/if}
                <button class="btn sm primary" onclick={() => { triggerOpen = false; void save(); }}>应用</button>
              </div>
            {/if}
          </div>
          <span class="spacer"></span>
          {#if running}
            <button class="btn danger" onclick={stopRun}>■ 停止</button>
          {:else}
            <button class="btn primary" onclick={runNow}>▶ 运行</button>
          {/if}
          <button class="btn" onclick={save}>保存</button>
          <button class="btn danger" onclick={del}>删除</button>
        </div>

        <div class="canvas-row">
          <div class="canvas-col">
            <div class="canvas-toolbar">
              <span class="spacer"></span>
              <button class="btn sm" onclick={() => addStep("start")}>🚩 开始</button>
              <button class="btn sm" onclick={() => addStep("note")}>📝 注释</button>
              <button class="btn sm" onclick={() => addStep("shell")}>＋ 命令</button>
              <button class="btn sm" onclick={() => addStep("agent")}>＋ Agent</button>
              <button class="btn sm" onclick={() => addStep("delay")}>＋ 等待</button>
              <button class="btn sm" onclick={() => addStep("env")}>＋ 变量</button>
              <button class="btn sm" onclick={() => addStep("balloon")}>🔔 气泡</button>
            </div>
            <div
              class="canvas"
              role="application"
              tabindex="0"
              bind:this={canvasEl}
              oncontextmenu={canvasContextMenu}
            >
              {#if selected.steps.length === 0}
                <div class="empty">画布为空：从上方添加第一个节点</div>
              {/if}
              <svg class="edges">
                {#each edges as e, i (i)}
                  <path
                    class="edge-hit"
                    class:sel={selectedEdge === i}
                    d={e.d}
                    onclick={() => { selectedEdge = i; selectedStep = null; }}
                    ondblclick={() => removeEdgeIdx(i)}
                    oncontextmenu={(ev) => edgeContextMenu(ev, i)}
                  />
                  <path class="edge" class:danger={selectedEdge === i} d={e.d} />
                  <circle cx={selected.steps[e.e.to].x + nodeW(selected.steps[e.e.to]) / 2} cy={selected.steps[e.e.to].y} r="3.5" class="dotp" />
                  <polygon
                    class="arrow"
                    points="{selected.steps[e.e.to].x + nodeW(selected.steps[e.e.to]) / 2},{selected.steps[e.e.to].y - 1} {selected.steps[e.e.to].x + nodeW(selected.steps[e.e.to]) / 2 - 4.5},{selected.steps[e.e.to].y - 9} {selected.steps[e.e.to].x + nodeW(selected.steps[e.e.to]) / 2 + 4.5},{selected.steps[e.e.to].y - 9}"
                  />
                {/each}
                {#if connecting !== null && selected}
                  {@const a = selected.steps[connecting]}
                  <path
                    class="edge connecting"
                    d="M {a.x + nodeW(a) / 2} {a.y + NODE_H} L {mouse.x} {mouse.y}"
                  />
                {/if}
              </svg>
              {#each selected.steps as step, i (i)}
                <div
                  class="node"
                  class:sel={selectedStep === i}
                  data-index={i}
                  style="left:{step.x}px; top:{step.y}px; width:{step.type === "note" || step.type === "env" ? 290 : NODE_W}px"
                  onpointerdown={(e) => nodeDown(e, i)}
                  ondblclick={(e) => { e.stopPropagation(); selectedStep = i; selectedEdge = null; nodeDlg = i; }}
                  oncontextmenu={(e) => nodeContextMenu(e, i)}
                >
                  {#if step.type !== "start"}
                    <div class="port-in" title="输入"></div>
                  {/if}
                  <div class="nhead">
                    <span class="nnum">{i + 1}</span>
                    <span class="nicon">{stepIcon[step.type]}</span>
                    <span class="ntype">{stepLabel[step.type]}</span>
                    <span class="spacer"></span>
                    <button class="ghost-xs" title="删除" onclick={(e) => { e.stopPropagation(); removeStep(i); }}>✕</button>
                  </div>
                  <div class="nbody">
                    {#if step.type === "start"}
                      {step.name || "开始"}
                    {:else if step.type === "note"}
                      <textarea
                        class="note-edit"
                        rows="3"
                        placeholder="写点说明…"
                        bind:value={step.text}
                        onclick={(e) => e.stopPropagation()}
                        onpointerdown={(e) => e.stopPropagation()}
                        ondblclick={(e) => e.stopPropagation()}
                      ></textarea>
                    {:else if step.type === "balloon"}
                      <span class="balloon-line">🔔 {step.title || "提醒"}</span>
                      {#if step.message}<div class="env-line dim2">{step.message}</div>{/if}
                    {:else if step.type === "shell"}
                      {step.name || firstLineOf(step.command)}
                    {:else if step.type === "agent"}
                      {step.name || ((contexts.find((c) => c.id === step.context_id)?.name ?? "未知上下文") + " · " + (step.agent_type))}
                      {step.session_id ? "" : " · 临时"}
                    {:else if step.type === "delay"}
                      等待 {step.seconds}s
                    {:else if step.type === "env"}
                      <div class="env-lines">
                        {#each Object.entries(step.vars ?? {}) as [k, v] (k)}
                          <div class="env-row">
                            <span class="env-k" title={k}>{step.labels?.[k] || k}</span>=<input class="env-input" bind:value={step.vars[k]} onclick={(e) => e.stopPropagation()} onpointerdown={(e) => e.stopPropagation()} ondblclick={(e) => e.stopPropagation()} />
                            <button class="ghost-xs" title={"删除 " + (step.labels?.[k] || k)} onclick={(e) => { e.stopPropagation(); delete step.vars[k]; }}>✕</button>
                          </div>
                        {/each}
                        <button class="env-add" onclick={(e) => { e.stopPropagation(); const key = envNewKey(step); if (key) step.vars[key] = ""; }}>＋ 添加</button>
                      </div>
                    {/if}
                  </div>
                  <div
                    class="port-out"
                    title="拖到目标节点连线"
                    onpointerdown={(e) => portDown(e, i)}
                  ></div>
                </div>
              {/each}
            </div>
          </div>

          <aside class="runs-col">
            <section class="card runs">
              <h3>运行历史</h3>
              <div class="runlist">
                {#each runs as r (r.id)}
                  <div class="run">
                    <button class="rhead" onclick={() => (openRun = openRun === r.id ? null : r.id)}>
                      <span class="badge {r.status === 'success' ? 'ok' : r.status === 'running' ? 'accent' : r.status === 'stopped' ? 'warn' : 'danger'}">
                        {{ running: "运行中", success: "成功", failed: "失败", stopped: "已停止" }[r.status] ?? r.status}
                      </span>
                      <span class="rtime">{r.started_at.slice(5, 16)} · {r.trigger === "manual" ? "手动" : "定时"}</span>
                    </button>
                    {#if openRun === r.id}
                      <pre class="rlog">{(app.wfLiveLogs[r.id] ?? r.log) || "（暂无日志）"}</pre>
                    {/if}
                  </div>
                {:else}
                  <p class="none">暂无运行记录</p>
                {/each}
              </div>
            </section>
          </aside>
        </div>
      {:else}
        <div class="empty" style="flex:1">选择或创建一个工作流</div>
      {/if}
    </div>
  {:else}
    <div class="empty" style="flex:1">先选择项目</div>
  {/if}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

{#if nodeDlg !== null && selected && selected.steps[nodeDlg]}
  {@const st = selected.steps[nodeDlg]}
  {@const idx = nodeDlg}
  <div class="modal-backdrop">
    <div class="modal" style="min-width: 560px" role="dialog" aria-label="编辑节点">
      <header>
        编辑节点 — {stepLabel[st.type]} {idx + 1}
        <button class="btn ghost sm" onclick={() => (nodeDlg = null)}>✕</button>
      </header>
      <div class="body col">
        {#if st.type === "start"}
          <div class="field"><label>节点名（可留空）</label><input bind:value={st.name} placeholder="开始" /></div>
          <p class="hint">工作流从「开始」节点起跑；从它连线到后续节点。</p>
        {:else if st.type === "note"}
          <div class="field"><label>标题（可留空）</label><input bind:value={st.name} /></div>
          <div class="field"><label>注释内容（画布上可直接编辑）</label><textarea rows="4" bind:value={st.text}></textarea></div>
          <p class="hint">注释节点不执行，仅用于说明工作流的作用与用法。</p>
        {:else if st.type === "balloon"}
          <div class="field"><label>节点名（可留空）</label><input bind:value={st.name} /></div>
          <div class="field"><label>标题</label><input bind:value={st.title} placeholder="如：构建完成" /></div>
          <div class="field"><label>内容（支持插值）</label><textarea rows="2" bind:value={st.message}></textarea></div>
          <div class="srow">
            <div class="field"><label>点击行为</label>
              <select bind:value={st.click_action}>
                <option value="none">无动作</option>
                <option value="open">打开目录或文件位置</option>
                <option value="url">浏览器打开 URL</option>
              </select>
            </div>
            <div class="field grow"><label>点击目标（目录路径 或 http(s):// 链接，支持插值）</label><input class="grow" bind:value={st.click_target} placeholder={st.click_action === "url" ? "https://…" : "{{var.build_dir}} 或 D:\dist"} /></div>
          </div>
          <label class="toggle"><input type="checkbox" bind:checked={st.sound} /> 气泡伴随提示音</label>
          <p class="hint">运行到该节点时弹出 Windows 系统气泡提醒；提醒失败不会中断工作流。</p>
        {:else if st.type === "shell"}
          <div class="field"><label>节点名（可留空，显示在画布与日志中）</label><input bind:value={st.name} placeholder="如：构建 / 下载文件" /></div>
          <div class="field"><label>命令</label><textarea rows="3" class="mono" bind:value={st.command}></textarea></div>
          <div class="srow">
            <div class="field"><label>解释器</label>
              <select bind:value={st.shell}>
                <option value="cmd">cmd</option>
                <option value="powershell">powershell</option>
                <option value="python">python（设置里配置解释器）</option>
              </select>
            </div>
            <div class="field grow"><label>工作目录（留空=项目根）</label><input class="grow" bind:value={st.cwd} placeholder={"{{env.__root__}} 或绝对路径"} /></div>
          </div>
          <div class="srow">
            <div class="field"><label>超时秒（留空不限）</label><input class="num" type="number" min="0" value={st.timeout_sec ?? ""} oninput={(e) => (st.timeout_sec = (e.target as HTMLInputElement).value ? Number((e.target as HTMLInputElement).value) : null)} /></div>
            <label class="toggle"><input type="checkbox" bind:checked={st.continue_on_error} /> 失败继续</label>
          </div>
          <p class="hint">输出变量：{envOutName(st, idx)}</p>
        {:else if st.type === "agent"}
          <div class="field"><label>节点名</label><input bind:value={st.name} placeholder="可留空" /></div>
          <div class="srow">
            <div class="field"><label>上下文</label>
              <select bind:value={st.context_id}>
                {#each contexts as c (c.id)}
                  <option value={c.id}>{c.name}</option>
                {/each}
              </select>
            </div>
            <div class="field"><label>Agent</label>
              <select bind:value={st.agent_type}>
                <option value="codex">Codex</option>
                <option value="zcode">ZCode</option>
              </select>
            </div>
            <div class="field grow"><label>会话 ID（留空=新建临时会话）</label><input bind:value={st.session_id} placeholder="留空 = 临时会话" /></div>
          </div>
          <div class="field"><label>提示词</label><textarea rows="3" bind:value={st.prompt}></textarea></div>
          <div class="srow">
            <div class="field"><label>超时秒</label><input class="num" type="number" min="0" value={st.timeout_sec ?? ""} oninput={(e) => (st.timeout_sec = (e.target as HTMLInputElement).value ? Number((e.target as HTMLInputElement).value) : null)} /></div>
            <label class="toggle"><input type="checkbox" bind:checked={st.continue_on_error} /> 失败继续</label>
          </div>
        {:else if st.type === "delay"}
          <div class="srow">
            <span>等待</span>
            <input class="num" type="number" min="1" bind:value={(st as any).seconds} />
            <span>秒</span>
          </div>
        {:else if st.type === "env"}
          <p class="hint">在节点上直接配置变量：</p>
          {#each Object.entries(st.vars ?? {}) as [k, v] (k)}
            <div class="kv">
              <input class="kn" value={k} readonly title={k} />
              <input class="klabel" placeholder="备注（画布优先显示）" value={st.labels?.[k] ?? ""} oninput={(ev) => { if (!st.labels) st.labels = {}; st.labels[k] = (ev.target as HTMLInputElement).value; }} />
              <span>=</span>
              <input bind:value={st.vars[k]} />
              <button class="btn ghost sm" onclick={() => delete st.vars[k]}>✕</button>
            </div>
          {:else}
            <p class="none">无变量</p>
          {/each}
          <div class="kv add">
            <input class="kn" placeholder="变量名" bind:value={dlgEnvKey} />
            <span>=</span>
            <input placeholder="值，可用 {HINT_ENV} 或 {HINT_DATE}" bind:value={dlgEnvVal} />
            <button class="btn sm" onclick={() => { if (dlgEnvKey.trim()) { st.vars[dlgEnvKey.trim()] = dlgEnvVal; dlgEnvKey = ""; dlgEnvVal = ""; } }}>添加</button>
          </div>
        {/if}
      </div>
      <footer>
        <button class="btn" onclick={() => (nodeDlg = null)}>取消</button>
        <button class="btn primary" onclick={() => { nodeDlg = null; void save(); }}>完成并保存</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .wfview {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .detail {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1;
  }
  .head {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border-soft);
    flex-wrap: wrap;
  }
  .name-input {
    width: 220px;
    font-weight: 600;
    background: transparent;
    border-color: transparent;
  }
  .name-input:hover,
  .name-input:focus {
    background: var(--bg-elev);
    border-color: var(--border);
  }
  .spacer {
    flex: 1;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .trigger-wrap {
    position: relative;
  }
  .popover {
    position: absolute;
    top: calc(100% + 8px);
    left: 0;
    z-index: 60;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    padding: 14px;
    min-width: 420px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 14px;
    flex-wrap: wrap;
  }
  .row label {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .toggle {
    color: var(--text-dim);
    font-size: 0.9em;
  }
  .sched select,
  .sched input {
    padding: 4px 8px;
  }
  .num {
    width: 90px;
  }
  .time {
    width: 170px;
  }
  .wd {
    padding: 2px 7px;
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 0.86em;
    color: var(--text-dim);
    cursor: pointer;
  }
  .wd.on {
    border-color: var(--accent);
    color: var(--accent);
  }
  .wd input {
    display: none;
  }
  .next {
    font-size: 0.85em;
    color: var(--text-faint);
  }
  .canvas-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 14px;
    padding: 14px 16px;
    flex: 1;
    min-height: 0;
  }
  @media (max-width: 1360px) {
    .canvas-row {
      grid-template-columns: 1fr;
    }
    .runs-col {
      order: 2;
    }
  }
  .canvas-col {
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-width: 0;
    min-height: 420px;
  }
  .card {
    background: var(--bg-panel);
    border: 1px solid var(--border-soft);
    border-radius: var(--radius);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .runs-col {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow-y: auto;
  }
  .runs-col .card {
    min-height: 0;
  }
  .card h3 {
    font-size: 0.9em;
    color: var(--text-dim);
  }
  .canvas-toolbar {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .hint {
    color: var(--text-faint);
    font-size: 0.82em;
  }
  .canvas {
    position: relative;
    background:
      radial-gradient(circle, color-mix(in srgb, var(--border) 55%, transparent) 1px, transparent 1px) 0 0 / 22px 22px,
      var(--bg-panel);
    border: 1px solid var(--border-soft);
    border-radius: var(--radius);
    min-height: 380px;
    flex: 1;
    overflow: auto;
    touch-action: none;
  }
  .edges {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
    overflow: visible;
  }
  .edge {
    fill: none;
    stroke: var(--accent);
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
    opacity: 0.7;
    pointer-events: none;
  }
  .edge-hit {
    fill: none;
    stroke: transparent;
    stroke-width: 16;
    cursor: pointer;
    pointer-events: stroke;
  }
  .edge-hit:hover {
    stroke: color-mix(in srgb, var(--accent) 25%, transparent);
  }
  .edge-hit.sel {
    stroke: color-mix(in srgb, var(--warn) 30%, transparent);
  }
  .edge.sel {
    stroke: var(--warn);
    opacity: 1;
  }
  .edge.danger {
    stroke: var(--danger);
    opacity: 1;
  }
  .edge.connecting {
    stroke-dasharray: 6 4;
    opacity: 0.9;
  }
  .dotp {
    fill: var(--accent);
    opacity: 0.8;
  }
  .arrow {
    fill: var(--accent);
    opacity: 0.85;
  }
  .klabel {
    width: 180px;
    flex: none;
    font-size: 0.9em;
  }
  .node {
    position: absolute;
    touch-action: none;
    background: var(--bg-elev);
    border: 1.5px solid var(--border);
    border-radius: 10px;
    cursor: grab;
    user-select: none;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.18);
  }
  .node.sel {
    border-color: var(--accent);
  }
  .node:active {
    cursor: grabbing;
  }
  .nhead {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border-soft);
    font-size: 0.8em;
    color: var(--text-dim);
  }
  .nnum {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--accent);
    color: var(--accent-contrast);
    font-size: 0.72em;
    font-weight: 700;
    display: flex;
    align-items: center;
    justify-content: center;
    flex: none;
  }
  .nicon {
    font-size: 0.9em;
  }
  .ntype {
    font-weight: 600;
  }
  .node .spacer {
    flex: 1;
  }
  .ghost-xs {
    color: var(--text-faint);
    font-size: 0.9em;
    padding: 0 2px;
  }
  .ghost-xs:hover {
    color: var(--danger);
  }
  .nbody {
    padding: 7px 10px;
    font-size: 0.84em;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-height: 28px;
  }
  .note-edit {
    width: 100%;
    background: transparent;
    border: none;
    color: var(--text-dim);
    font-size: 0.9em;
    line-height: 1.45;
    resize: vertical;
    min-height: 54px;
    padding: 0;
    white-space: pre-wrap;
  }
  .note-edit:focus {
    outline: none;
    color: var(--text);
  }
  .balloon-line {
    color: var(--text);
    font-weight: 600;
  }
  .env-row {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .env-k {
    color: var(--accent);
    font-family: var(--mono);
    font-size: 0.9em;
    flex: none;
    max-width: 90px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .env-input {
    flex: 1;
    min-width: 0;
    padding: 1px 6px;
    font-size: 0.9em;
    font-family: var(--mono);
    height: 22px;
  }
  .env-add {
    color: var(--text-faint);
    font-size: 0.82em;
    padding: 1px 0;
  }
  .env-add:hover {
    color: var(--accent);
  }
  .env-lines {
    white-space: normal;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-height: 0;
  }
  .env-line {
    font-family: var(--mono);
    font-size: 0.9em;
    color: var(--text-dim);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .env-line.dim2 {
    color: var(--text-faint);
  }
  .port-in,
  .port-out {
    position: absolute;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--bg-panel);
    border: 2px solid var(--accent);
    left: calc(50% - 6px);
    z-index: 2;
  }
  .port-in {
    top: -7px; /* 上方输入口：连线终点落在节点顶部中点 */
  }
  .port-out {
    bottom: -7px; /* 下方输出口：连线起点在节点底部中点 */
    cursor: crosshair;
  }
  .port-out:hover {
    background: var(--accent);
  }
  .runlist {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .run {
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    overflow: hidden;
  }
  .rhead {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 7px 12px;
  }
  .rhead:hover {
    background: var(--bg-elev);
  }
  .rtime {
    color: var(--text-faint);
    font-size: 0.85em;
  }
  .rlog {
    font-family: var(--mono);
    font-size: 0.78em;
    background: var(--code-bg);
    border-top: 1px solid var(--border-soft);
    padding: 10px 12px;
    max-height: 240px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-all;
    user-select: text;
    margin: 0;
  }
  .none {
    color: var(--text-faint);
    font-size: 0.88em;
  }
  .col,
  .body {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 0.88em;
  }
  .field label {
    color: var(--text-dim);
  }
  .field.grow {
    flex: 1;
    min-width: 160px;
  }
  .srow {
    display: flex;
    gap: 10px;
    align-items: end;
    flex-wrap: wrap;
  }
  .mono {
    font-family: var(--mono);
    font-size: 0.9em;
  }
  .kv {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: 0.88em;
    min-width: 0;
  }
  .kv input,
  .kv code,
  .kv span {
    min-width: 0;
  }
  .kv.add input.kn {
    width: 130px;
    flex: none;
  }
  .kv.add input {
    flex: 1;
    min-width: 0;
    width: auto;
  }
  .kv.add .btn {
    flex: none;
  }
  .empty {
    color: var(--text-faint);
    display: flex;
    align-items: center;
    justify-content: center;
  }
</style>
