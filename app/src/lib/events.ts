// Wire backend events to global state. Called once from App.svelte on mount.
import { listen } from "@tauri-apps/api/event";
import { app, applySessionUpdate, toast, chatKey } from "./state.svelte";
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
    sessionExpired?: boolean;
    detail?: string;
  }>("acp://update", (e) => {
    const p = e.payload;
    if (p.sessionExpired) {
      toast("warn", `会话已失效（${p.agentType}）${p.detail ? "：" + p.detail : ""}，请新建会话`);
      return;
    }
    if (p.update) {
      const ctxId = p.contextId ?? findContextBySession(p.agentType, p.sessionId);
      applySessionUpdate(p.agentType, ctxId, p.sessionId ?? "", p.update);
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

  await listen<SessionReadyInfo>("acp://session-ready", (e) => {
    const p = e.payload;
    const key = chatKey(p.contextId, p.agentType);
    // resume 场景的 payload 不含 models/modes —— 保留已有能力信息
    const prev = app.sessionInfo[key];
    const merged: SessionReadyInfo = {
      ...p,
      response: {
        ...prev?.response,
        ...p.response,
      },
    };
    app.sessionInfo[key] = merged;
    app.bindingSession[key] = p.sessionId;
    // 缓存 agent 能力（模型/配置项），供会话创建前展示
    const caps = merged.response;
    if (caps.models || caps.configOptions) {
      app.agentCaps[p.agentType] = { models: caps.models, configOptions: caps.configOptions };
      void api.settingsSet("caps." + p.agentType, JSON.stringify(app.agentCaps[p.agentType])).catch(() => {});
    }
    // 应用会话创建前暂存的配置
    const pending = app.pendingCfg[p.agentType];
    if (pending) {
      delete app.pendingCfg[p.agentType];
      void api
        .acpSetConfigOption(p.contextId, p.agentType, pending.id, pending.value)
        .catch(() => {});
    }
  });

  // 回合结束（完成/中断）后自增目录树版本号 → FileTree 自动刷新，及时看到 Agent 产出的文件
  await listen<{ contextId: string; agentType: string; status: string }>("acp://binding-status", (e) => {
    if (e.payload.status === "completed" || e.payload.status === "interrupted") {
      app.treeRev++;
      fireAfterAction();
    }
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
  for (const [key, info] of Object.entries(app.sessionInfo)) {
    if (key.endsWith(`:${agentType}`) && info.sessionId === sessionId) {
      return key.slice(0, key.length - agentType.length - 1);
    }
  }
  return null;
}
