// Debounced, serialized autosave for workflow edits.
//
// Invariants (each one closes a way edits used to be lost or mis-reported):
// - a workflow is "saved" only after the IPC call succeeded — never when a timer is armed;
// - dirty ids and baselines live at module level, so unmounting WorkflowsView (tab switch,
//   project switch) can never drop a pending save; the view flushes on destroy and the
//   pending timer keeps working even if it fires after unmount;
// - saves are serialized, so two edits of the same workflow can never reach the backend
//   out of order;
// - the backend owns last_run_at / next_run_at: they are merged back from the returned row
//   instead of being echoed from the client;
// - edits that the backend would reject (empty name, empty/invalid schedule) are held back
//   and reported instead of being sent again every 800 ms.
import { untrack } from "svelte";
import { app, onBeforeWorkflowsReload, toast } from "../state.svelte";
import { api } from "../ipc";
import { t } from "../i18n";
import type { ScheduleConfig, Workflow } from "../types";
import { onceFromInput, workflowProblem } from "./wf-shared";

const DEBOUNCE_MS = 800;
const baseline = new Map<string, string>(); // id -> snapshot known to be stored (or last sent successfully)
const dirty = new Set<string>();
// Reactive revision so views can show "unsaved" state without polling.
const status = $state({ rev: 0 });
let timer: ReturnType<typeof setTimeout> | null = null;
let chain: Promise<boolean> = Promise.resolve(true);

// Callers run inside $effect; untrack the bump so the effect never depends on `rev`.
function markDirty(id: string) {
  if (!dirty.has(id)) {
    dirty.add(id);
    untrack(() => status.rev++);
  }
}

function markClean(id: string) {
  if (dirty.delete(id)) untrack(() => status.rev++);
}

function canonicalSchedule(s: ScheduleConfig | null): ScheduleConfig | null {
  if (s && s.kind === "once") return { kind: "once", at: onceFromInput(s.at) };
  return s;
}

/** Content-only fingerprint of the editable fields (runtime fields are deliberately excluded). */
export function snapOf(w: Workflow | null): string {
  if (!w) return "";
  return JSON.stringify({ n: w.name, d: w.description, en: w.enabled, t: w.trigger_type, s: canonicalSchedule(w.schedule), st: w.steps, ed: w.edges, env: w.env });
}

/** True while `id` has edits that are not yet confirmed by the backend (reactive). */
export function isDirty(id: string): boolean {
  void status.rev;
  return dirty.has(id);
}

/** Drop bookkeeping for a workflow that no longer exists. */
export function forgetWorkflow(id: string) {
  baseline.delete(id);
  markClean(id);
}

/**
 * Compare a live row with its baseline and arm the debounced save when it differs.
 * Returns the reason the edit cannot be saved yet (null when clean or scheduled).
 */
export function trackWorkflowEdit(w: Workflow): string | null {
  const snap = snapOf(w);
  const base = baseline.get(w.id);
  if (base === undefined) {
    // First time we see this row: it came from the backend, so it is the baseline.
    baseline.set(w.id, snap);
    return null;
  }
  if (snap === base) {
    markClean(w.id);
    return null;
  }
  markDirty(w.id);
  const problem = workflowProblem(w);
  if (timer) clearTimeout(timer);
  // The debounced flush is "quiet": an edit the backend would reject is shown inline by the
  // editor and simply waits; explicit saves / leaving the editor report it with a toast.
  timer = setTimeout(() => {
    timer = null;
    void flushWorkflowSaves({ quiet: true });
  }, DEBOUNCE_MS);
  return problem;
}

/** Save every dirty workflow now. Resolves to true when nothing failed or was held back. */
export function flushWorkflowSaves(opts: { quiet?: boolean } = {}): Promise<boolean> {
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  const run = () => flushNow(opts.quiet === true);
  chain = chain.then(run, run);
  return chain;
}

async function flushNow(quiet: boolean): Promise<boolean> {
  let ok = true;
  for (const id of [...dirty]) {
    const live = app.workflows.find((x) => x.id === id);
    if (!live) {
      forgetWorkflow(id);
      continue;
    }
    const payload = $state.snapshot(live) as Workflow;
    if (payload.schedule?.kind === "once") payload.schedule = { kind: "once", at: onceFromInput(payload.schedule.at) };
    const problem = workflowProblem(payload);
    if (problem) {
      ok = false;
      if (!quiet) toast("warn", t("「{name}」未保存：{reason}", { name: payload.name || t("未命名工作流"), reason: problem }));
      continue;
    }
    const sent = snapOf(payload);
    try {
      const saved = await api.workflowUpdate(payload);
      baseline.set(id, sent);
      const row = app.workflows.find((x) => x.id === id);
      if (row) {
        // Server-owned fields only; editable fields may have changed again while the call was in flight.
        row.last_run_at = saved.last_run_at;
        row.next_run_at = saved.next_run_at;
        row.updated_at = saved.updated_at;
        if (snapOf(row) === sent) markClean(id);
      } else {
        forgetWorkflow(id);
      }
    } catch (e) {
      ok = false;
      const msg = String(e);
      if (msg.includes("工作流不存在")) {
        forgetWorkflow(id);
        continue;
      }
      toast("error", t("自动保存失败：{error}", { error: msg }));
    }
  }
  return ok;
}

// Any reload of app.workflows (project switch, move up/down, …) first lands pending edits.
onBeforeWorkflowsReload(() => flushWorkflowSaves());

// Last-resort flush when the webview is being torn down (window close / reload).
if (typeof window !== "undefined") {
  window.addEventListener("beforeunload", () => {
    if (dirty.size) void flushWorkflowSaves();
  });
}
