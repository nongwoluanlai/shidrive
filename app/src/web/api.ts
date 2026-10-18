// 网站接入：HTTP / SSE 客户端（不依赖任何 Tauri 模块）。
// 鉴权走 HttpOnly Cookie；写操作附带 X-SD-Web 头（服务端据此拒绝跨站请求）。

export interface Project { id: string; name: string; root_path: string; description: string }
export interface Context { id: string; project_id: string; name: string; overview: string; constraints: string; updated_at: string }
export interface ContextEntry { id: string; kind: string; content: string; status: string; updated_at: string }
export interface Commit { seq: number; agent: string; summary: string; files: unknown; time: string }
export interface Agent { id: string; name: string; status: string }
export interface ToolCall {
  sessionUpdate?: string;
  toolCallId: string;
  title?: string;
  kind?: string;
  status?: string;
  content?: unknown;
  rawInput?: unknown;
  rawOutput?: unknown;
  locations?: { path: string; line?: number }[];
}
export interface Item {
  id: string;
  kind: "user" | "assistant" | "thought" | "tools" | "error";
  text: string;
  tools?: ToolCall[];
  streaming?: boolean;
  time?: string;
  requestId?: string;
}
export interface Binding { session_id: string | null; title: string | null; status: string; model: string }
export interface ReplayRow { role: string; content: string }
export interface PermissionOption { optionId: string; name: string; kind: string }
export interface PermissionReq {
  requestId: string;
  agentType: string;
  sessionId: string;
  params: { toolCall?: { title?: string; kind?: string; rawInput?: unknown }; options: PermissionOption[] };
}
export interface WorkflowSummary {
  id: string; name: string; description: string; enabled: boolean; trigger_type: string;
  schedule: { kind: string; every_minutes?: number; time?: string; weekdays?: number[]; at?: string } | null;
  steps: { kind: string; name: string }[];
  last_run_at: string | null; next_run_at: string | null; running: string | null;
}
export interface RunSummary { id: string; workflow_id: string; trigger: string; status: string; started_at: string; finished_at: string | null; log_len: number }
export interface RunDetail extends Omit<RunSummary, "log_len"> { log: string }
export interface DirEntry { name: string; path: string; is_dir: boolean; size: number; mtime: string }
export interface FileHit { path: string; rel: string; score: number; indices: number[] }
export interface LineMatch { line: number; col: number; len: number; preview: string; pstart: number; plen: number }
export interface ContentHit { path: string; rel: string; matches: LineMatch[]; more: boolean }

export class ApiError extends Error {
  constructor(public status: number, message: string, public data?: any) { super(message); }
}

let onUnauthorized: (() => void) | null = null;
export function setUnauthorizedHandler(fn: () => void) { onUnauthorized = fn; }

async function request<T>(method: "GET" | "POST", url: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (method === "POST") { headers["Content-Type"] = "application/json"; headers["X-SD-Web"] = "1"; }
  let res: Response;
  try {
    res = await fetch(url, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), credentials: "same-origin", cache: "no-store" });
  } catch {
    throw new ApiError(0, "网络连接失败，请检查电脑端服务与隧道");
  }
  let data: any = null;
  const text = await res.text();
  try { data = text ? JSON.parse(text) : null; } catch { data = null; }
  if (!res.ok) {
    if (res.status === 401 && !url.endsWith("/api/login")) onUnauthorized?.();
    throw new ApiError(res.status, data?.error ?? `请求失败（${res.status}）`, data);
  }
  return data as T;
}

const qs = (o: Record<string, string | number | undefined>) =>
  Object.entries(o).filter(([, v]) => v !== undefined && v !== "").map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`).join("&");

export const api = {
  me: () => request<{ authed: boolean; version: string }>("GET", "/api/me"),
  login: (code: string) => request<{ ok: boolean }>("POST", "/api/login", { code }),
  logout: () => request<{ ok: boolean }>("POST", "/api/logout", {}),
  projects: () => request<Project[]>("GET", "/api/projects"),
  contexts: (project: string) => request<Context[]>("GET", `/api/contexts?${qs({ project })}`),
  context: (id: string) => request<{ context: Context; entries: ContextEntry[]; commits: Commit[] }>("GET", `/api/context?${qs({ id })}`),
  agents: () => request<Agent[]>("GET", "/api/agents"),
  /** limit：只取最近 N 条（默认服务端 150；0 = 全部） */
  chat: (context: string, agent: string, limit?: number) =>
    request<{ binding: Binding | null; sessionId: string | null; items: Item[] | null; omitted?: number }>("GET", `/api/chat?${qs({ context, agent, limit })}`),
  history: (contextId: string, agent: string, limit?: number) =>
    request<{ sessionId: string | null; rows: ReplayRow[]; omitted?: number }>("POST", "/api/chat/history", limit === undefined ? { contextId, agent } : { contextId, agent, limit }),
  send: (contextId: string, agent: string, text: string, requestId: string) =>
    request<{ requestId: string }>("POST", "/api/chat/send", { contextId, agent, text, requestId }),
  cancel: (contextId: string, agent: string) => request<{ ok: boolean }>("POST", "/api/chat/cancel", { contextId, agent }),
  newSession: (contextId: string, agent: string) => request<{ sessionId: string }>("POST", "/api/chat/new", { contextId, agent }),
  permissions: () => request<PermissionReq[]>("GET", "/api/permissions"),
  respondPermission: (requestId: string, optionId: string) => request<{ ok: boolean }>("POST", "/api/permission", { requestId, optionId }),
  workflows: (project: string) => request<WorkflowSummary[]>("GET", `/api/workflows?${qs({ project })}`),
  runs: (workflow: string, limit = 20) => request<RunSummary[]>("GET", `/api/runs?${qs({ workflow, limit })}`),
  run: (id: string) => request<RunDetail>("GET", `/api/run?${qs({ id })}`),
  runWorkflow: (id: string) => request<{ runId: string }>("POST", "/api/workflow/run", { id }),
  stopRun: (runId: string) => request<{ ok: boolean }>("POST", "/api/workflow/stop", { runId }),
  list: (project: string, path = "") => request<{ root: string; path: string; entries: DirEntry[] }>("GET", `/api/fs/list?${qs({ project, path })}`),
  read: (project: string, path: string) =>
    request<{ content?: string; binary?: boolean; size: number; tooLarge?: boolean }>("GET", `/api/fs/read?${qs({ project, path })}`),
  rawUrl: (project: string, path: string) => `/api/fs/raw?${qs({ project, path })}`,
  search: (project: string, q: string, mode: "files" | "content") =>
    request<{ files: FileHit[]; content: ContentHit[]; truncated?: boolean }>("GET", `/api/fs/search?${qs({ project, q, mode })}`),
};

/** 不依赖安全上下文（http 局域网访问时 crypto.randomUUID 不可用） */
export function rid(): string {
  const c = (globalThis as any).crypto;
  if (c?.randomUUID) return c.randomUUID();
  const b = new Uint8Array(16);
  if (c?.getRandomValues) c.getRandomValues(b);
  else for (let i = 0; i < 16; i++) b[i] = Math.floor(Math.random() * 256);
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("").replace(/^(.{8})(.{4})(.{4})(.{4})/, "$1-$2-$3-$4-");
}

export type EventHandler = (name: string, payload: any) => void;

/** SSE 订阅：断线由浏览器自动重连（retry 3s），重连成功会再收到 hello 事件 */
export function subscribe(onEvent: EventHandler, onState: (connected: boolean) => void): () => void {
  const names = [
    "hello", "acp://update", "acp://binding-status", "acp://session-ready", "acp://permission",
    "acp://requests-cancelled", "acp://status", "wf://status", "wf://log", "wf://deleted",
    "chat://prompt", "web://prompt-done", "web://logout",
  ];
  let es: EventSource | null = null;
  let closed = false;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  const open = () => {
    if (closed) return;
    es = new EventSource("/api/events", { withCredentials: true });
    es.onopen = () => onState(true);
    es.onerror = () => {
      onState(false);
      // 服务端返回非 200（例如 401）时浏览器不会自动重连：手动退避重连
      if (es && es.readyState === EventSource.CLOSED) {
        es.close();
        es = null;
        retryTimer = setTimeout(open, 4000);
      }
    };
    for (const n of names) {
      es.addEventListener(n, (e) => {
        let data: any = null;
        try { data = JSON.parse((e as MessageEvent).data); } catch { /* ignore */ }
        onEvent(n, data);
      });
    }
  };
  open();
  return () => {
    closed = true;
    if (retryTimer) clearTimeout(retryTimer);
    es?.close();
  };
}
