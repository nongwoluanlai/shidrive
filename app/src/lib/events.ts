// Wire backend events to global state. Called once from App.svelte on mount.
import { listen } from "@tauri-apps/api/event";
import { app, applySessionUpdate, applyBindingStatus, adoptBinding, capabilityLists, toast, chatKey } from "./state.svelte";
import type { AgentType, ElicitationRequest, PermissionRequest, SessionReadyInfo } from "./types";
import { api } from "./ipc";

// ---------- 执行后动作（回合结束后触发；配置存 localStorage，聊天页配置栏可改） ----------
export interface AfterActionCfg {
  kind: "sound" | "shutdown" | "command";
  cmd: string;
}

export function loadAfterAction(): AfterActionCfg {
  try {
    const raw = localStorage.getItem("shidrive.afterAction");
    if (raw) {
      const v = JSON.parse(raw) as AfterActionCfg;
      if (v && (v.kind === "sound" || v.kind === "shutdown" || v.kind === "command")) {
        return { kind: v.kind, cmd: typeof v.cmd === "string" ? v.cmd : "" };
      }
    }
  } catch {}
  return { kind: "sound", cmd: "" };
}

function fireAfterAction() {
  const cfg = loadAfterAction();
  if (cfg.kind === "sound") {
    void api.systemAfterAction("sound", "").catch(() => {});
  } else if (cfg.kind === "shutdown") {
    void api.systemAfterAction("shutdown", "").catch(() => {});
    import("./state.svelte").then(({ toast }) => toast("warn", "回合已结束：已安排 30 秒后关机（cmd 执行 shutdown /a 可取消）"));
  } else if (cfg.kind === "command" && cfg.cmd.trim()) {
    void api.systemAfterAction("command", cfg.cmd).catch(() => {});
  }
}

export async function wireEvents() {
  await listen<{
    agentType: AgentType;
    sessionId?: string;
    contextId?: string | null;
    update?: { sessionUpdate?: string } & Record<string, unknown>;
    turnId?: string;
    connectionId?: string;
    source?: string;
    sessionExpired?: boolean;
    detail?: string;
  }>("acp://update", (e) => {
    const p = e.payload;
    if (p.sessionExpired) {
      toast("warn", `会话已失效（${p.agentType}）${p.detail ? "：" + p.detail : ""}，请新建会话`);
      return;
    }
    if (p.update && p.source !== "workflow") {
      const ctxId = p.contextId ?? findContextBySession(p.agentType, p.sessionId);
      // 找不到所属上下文的更新以前会写进 "-:agent" 这个没有任何界面展示、也永远
      // 不会被清理的幽灵聊天里（只增不减）；现在直接丢弃
      if (!ctxId) return;
      applySessionUpdate(p.agentType, ctxId, p.sessionId ?? "", p.update, p);
    }
  });

  await listen<{ agentType: AgentType; state: string }>("acp://status", (e) => {
    app.agentStatus[e.payload.agentType] = e.payload.state;
  });

  await listen<PermissionRequest>("acp://permission", (e) => {
    app.permissions.push(e.payload);
  });

  await listen<ElicitationRequest>("acp://elicitation", (e) => {
    app.elicitations.push(e.payload);
  });

  await listen<{ requestIds: string[] }>("acp://requests-cancelled", (e) => {
    const ids = new Set(e.payload.requestIds);
    app.permissions = app.permissions.filter((r) => !ids.has(r.requestId));
    app.elicitations = app.elicitations.filter((r) => !ids.has(r.requestId));
  });

  await listen<SessionReadyInfo>("acp://session-ready", (e) => {
    const p = e.payload;
    const key = chatKey(p.contextId, p.agentType);
    const old = app.sessionInfo[key];
    const prev = old?.sessionId === p.sessionId && old.connectionId === p.connectionId ? old : undefined;
    adoptBinding(key, p.sessionId);
    const merged: SessionReadyInfo = { ...p, response: { ...prev?.response, ...p.response } };
    const cached = capabilityLists(app.agentCaps[p.agentType] ?? {});
    if (!merged.response.models && cached.models) merged.response.models = cached.models;
    if (!merged.response.configOptions?.length && cached.configOptions) merged.response.configOptions = cached.configOptions;
    app.sessionInfo[key] = merged;
    if (p.title != null) app.bindingTitleMap[key] = p.title;
    const caps = merged.response;
    if (caps.models || caps.configOptions) {
      app.agentCaps[p.agentType] = capabilityLists(caps);
      void api.settingsSet("caps." + p.agentType, JSON.stringify(app.agentCaps[p.agentType])).catch(() => {});
    }
    // Pre-session choices belong to a context+agent, not to whichever session
    // happens to become ready first on this adapter.
    const pending = app.pendingCfg[key];
    if (pending) {
      delete app.pendingCfg[key];
      for (const [id, value] of Object.entries(pending)) {
        void api.acpSetConfigOption(p.contextId, p.agentType, id, value, p.sessionId).catch(() => {});
      }
    }
  });

  await listen<{
    contextId: string; agentType: string; status: string; sessionId: string;
    turnId: string; connectionId?: string; source?: string;
  }>("acp://binding-status", (e) => {
    if (applyBindingStatus(e.payload)) {
      app.treeRev++;
      fireAfterAction();
    }
  });

  // Workflow output is displayed in its run log; only refresh the file tree here.
  await listen<{ status: string }>("acp://workflow-status", (e) => {
    if (e.payload.status === "completed" || e.payload.status === "interrupted") app.treeRev++;
  });

  // workflow run log/status: global so the 运行中 view keeps state across tab switches
  await listen<{ runId: string; line: string }>("wf://log", (e) => {
    const { runId, line } = e.payload;
    const logs = app.wfLiveLogs;
    logs[runId] = (logs[runId] ?? "") + line + "\n";
    // keep memory bounded: keep only the 20 most recent run logs
    const keys = Object.keys(logs);
    if (keys.length > 20) {
      for (const k of keys.slice(0, keys.length - 20)) {
        if (!Object.values(app.wfRunning).includes(k)) delete logs[k];
      }
    }
  });

  await listen<{ runId: string; workflowId: string; status: string }>("wf://status", (e) => {
    const { runId, workflowId, status } = e.payload;
    if (status === "running") {
      app.wfRunning[workflowId] = runId;
      if (!app.wfLiveLogs[runId]) app.wfLiveLogs[runId] = "";
    } else {
      delete app.wfRunning[workflowId];
    }
  });
}

function findContextBySession(agentType: AgentType, sessionId?: string): string | null {
  if (!sessionId) return null;
  for (const [key, bound] of Object.entries(app.bindingSession)) {
    if (key.endsWith(`:${agentType}`) && bound === sessionId) {
      return key.slice(0, key.length - agentType.length - 1);
    }
  }
  return null;
}
