// Typed wrappers over the Tauri IPC commands.
import { invoke } from "@tauri-apps/api/core";
import type {
  AgentBinding,
  AgentEnvStatusItem,
  AgentLaunch,
  AgentType,
  BindingInfo,
  TranscriptRow,
  Context,
  ContextEntry,
  DirEntryInfo,
  Edge,
  Project,
  ProjectCleanup,
  ScCommit,
  ScheduleConfig,
  SessionInfo,
  SetupStatus,
  NodeStat,
  OAuthPending,
  OAuthToken,
  Workflow,
  WorkflowRun,
  WorkflowStep,
} from "./types";

export const api = {
  // projects
  projectsList: () => invoke<Project[]>("projects_list"),
  projectsCreate: (name: string, root_path: string, description: string) =>
    invoke<Project>("projects_create", { name, rootPath: root_path, description }),
  projectsUpdate: (id: string, name: string, root_path: string, description: string) =>
    invoke<void>("projects_update", { id, name, rootPath: root_path, description }),
  /** Deletes the project and everything under it (contexts, bindings, chat, workflows, runs, remote grants). */
  projectsDelete: (id: string) => invoke<ProjectCleanup>("projects_delete", { id }),

  // contexts
  contextsList: (project_id: string) => invoke<Context[]>("contexts_list", { projectId: project_id }),
  contextsCreate: (project_id: string, name: string) => invoke<Context>("contexts_create", { projectId: project_id, name }),
  contextsUpdate: (id: string, name: string, overview: string, constraints: string) =>
    invoke<void>("contexts_update", { id, name, overview, constraints }),
  contextsDelete: (id: string) => invoke<void>("contexts_delete", { id }),

  // entries
  entriesList: (context_id: string) => invoke<ContextEntry[]>("context_entries_list", { contextId: context_id }),
  entryAdd: (context_id: string, kind: string, content: string) =>
    invoke<ContextEntry>("context_entry_add", { contextId: context_id, kind, content }),
  entryUpdate: (id: string, content: string, status: string) => invoke<void>("context_entry_update", { id, content, status }),
  entryDelete: (id: string) => invoke<void>("context_entry_delete", { id }),
  contextCommits: (context_id: string, limit = 50) =>
    invoke<ScCommit[]>("context_commits_list", { contextId: context_id, limit }),

  // bindings / chat
  bindingGet: (context_id: string, agent_type: string) =>
    invoke<AgentBinding | null>("binding_get", { contextId: context_id, agentType: agent_type }),
  bindingUnbind: (context_id: string, agent_type: string) =>
    invoke<void>("binding_unbind", { contextId: context_id, agentType: agent_type }),
  // acp
  acpStatus: (agent_type: string) => invoke<string>("acp_status", { agentType: agent_type }),
  acpConnect: (agent_type: string) => invoke<unknown>("acp_connect", { agentType: agent_type }),
  acpDisconnect: (agent_type: string) => invoke<void>("acp_disconnect", { agentType: agent_type }),
  acpSessionNew: (context: Context, agent_type: string) => invoke<string>("acp_session_new", { context, agentType: agent_type }),
  acpDeepseekNew: (context: Context) => invoke<string>("acp_deepseek_new", { context }),
  acpPrompt: (context: Context, agent_type: string, text: string, images?: { data: string; mime: string }[]) =>
    invoke<{ stopReason?: string }>("acp_prompt", { context, agentType: agent_type, text, images: images ?? [] }),
  acpCancel: (context_id: string, agent_type: string) => invoke<void>("acp_cancel", { contextId: context_id, agentType: agent_type }),
  acpSetMode: (context_id: string, agent_type: string, mode_id: string) =>
    invoke<void>("acp_set_mode", { contextId: context_id, agentType: agent_type, modeId: mode_id }),
  acpSetConfigOption: (context_id: string, agent_type: string, option_id: string, value: unknown) =>
    invoke<void>("acp_set_config_option", { contextId: context_id, agentType: agent_type, optionId: option_id, value }),
  acpRespondElicitation: (request_id: string, response: unknown) =>
    invoke<void>("acp_respond_elicitation", { requestId: request_id, response }),
  acpRespondPermission: (request_id: string, option_id: string) =>
    invoke<void>("acp_respond_permission", { requestId: request_id, optionId: option_id }),
  acpSessionsList: (agent_type: string) => invoke<SessionInfo[]>("acp_sessions_list", { agentType: agent_type }),
  acpSessionBind: (context: Context, agent_type: string, session_id: string, title?: string, silent?: boolean) =>
    invoke<TranscriptRow[]>("acp_session_bind", { context, agentType: agent_type, sessionId: session_id, title, silent: silent ?? false }),
  bindingsAll: () => invoke<BindingInfo[]>("bindings_all"),
  contextsAll: () => invoke<[Context, string][]>("contexts_all"),
  activeRuns: () => invoke<[string, string, string, string][]>("active_runs"),

  // fs
  fsList: (path: string) => invoke<DirEntryInfo[]>("fs_list", { path }),
  fsRead: (path: string) => invoke<[string, boolean]>("fs_read", { path }),
  fsWrite: (path: string, content: string) => invoke<void>("fs_write", { path, content }),
  fsCreateFile: (path: string) => invoke<void>("fs_create_file", { path }),
  fsCreateDir: (path: string) => invoke<void>("fs_create_dir", { path }),
  fsRename: (from: string, to: string) => invoke<void>("fs_rename", { from, to }),
  fsDelete: (path: string) => invoke<void>("fs_delete", { path }),
  fsOpenExplorer: (path: string) => invoke<void>("fs_open_explorer", { path }),
  fsOpenTerminal: (path: string) => invoke<void>("fs_open_terminal", { path }),
  fsOpenDefault: (path: string) => invoke<void>("fs_open_default", { path }),
  fsOpenCmd: (path: string) => invoke<void>("fs_open_cmd", { path }),
  fsOpenVscode: (path: string) => invoke<void>("fs_open_vscode", { path }),
  fsCopyToClipboard: (paths: string[]) => invoke<void>("fs_copy_to_clipboard", { paths }),
  fsPasteFromClipboard: (dest_dir: string) => invoke<void>("fs_paste_from_clipboard", { destDir: dest_dir }),
  fsDesktopDir: () => invoke<string>("fs_desktop_dir"),
  chatStoreGet: (key: string) => invoke<string>("chat_store_get", { key }),
  chatStoreSet: (key: string, itemsJson: string) => invoke<void>("chat_store_set", { key, itemsJson }),
  chatStoreDelete: (key: string) => invoke<void>("chat_store_delete", { key }),
  bindingSetTitle: (context_id: string, agent_type: string, title: string) =>
    invoke<void>("binding_set_title", { contextId: context_id, agentType: agent_type, title }),
  workflowMove: (id: string, dir: number) => invoke<void>("workflow_move", { id, dir }),

  // workflows
  workflowsList: (project_id: string) => invoke<Workflow[]>("workflows_list", { projectId: project_id }),
  workflowCreate: (w: {
    project_id: string;
    name: string;
    description: string;
    enabled: boolean;
    trigger_type: string;
    schedule: ScheduleConfig | null;
    steps: WorkflowStep[];
    env?: Record<string, string>;
    edges?: Edge[];
  }) =>
    invoke<Workflow>("workflow_create", {
      projectId: w.project_id,
      name: w.name,
      description: w.description,
      enabled: w.enabled,
      triggerType: w.trigger_type,
      schedule: w.schedule,
      steps: w.steps,
      env: w.env ?? {},
      edges: w.edges ?? [],
    }),
  /** Saves editable fields only; the returned row carries the server-owned last_run_at/next_run_at. */
  workflowUpdate: (workflow: Workflow) => invoke<Workflow>("workflow_update", { workflow }),
  workflowDelete: (id: string) => invoke<void>("workflow_delete", { id }),
  workflowRun: (id: string) => invoke<void>("workflow_run", { id }),
  workflowStop: (run_id: string) => invoke<void>("workflow_stop", { runId: run_id }),
  runsList: (workflow_id: string, limit = 30) => invoke<WorkflowRun[]>("runs_list", { workflowId: workflow_id, limit }),

  // settings
  settingsGet: (key: string) => invoke<string | null>("settings_get", { key }),
  settingsSet: (key: string, value: string) => invoke<void>("settings_set", { key, value }),
  setupStatus: () => invoke<SetupStatus>("setup_status"),
  nodeStatus: () => invoke<NodeStat>("node_status"),
  remoteMcpStatus: () => invoke<any>("remote_mcp_status"),
  remoteMcpStart: (opts: { listenHost?: string; port?: number; quickTunnel?: boolean }) =>
    invoke<void>("remote_mcp_start", { opts: { listenHost: opts.listenHost ?? "0.0.0.0", port: opts.port ?? 51688, quickTunnel: !!opts.quickTunnel } }),
  remoteMcpStop: () => invoke<void>("remote_mcp_stop"),
  remoteGrantsList: () => invoke<any[]>("remote_grants_list"),
  remoteGrantCreate: (input: Record<string, unknown>) => invoke<any>("remote_grant_create", { input }),
  remoteGrantRevoke: (id: string) => invoke<void>("remote_grant_revoke", { id }),
  remoteGrantDelete: (id: string) => invoke<void>("remote_grant_delete", { id }),
  remoteGrantRotateToken: (id: string) => invoke<{ token: string }>("remote_grant_rotate_token", { id }),
  remoteGrantToken: (id: string) => invoke<{ token: string }>("remote_grant_token", { id }),
  remoteGrantPause: (id: string) => invoke<void>("remote_grant_pause", { id }),
  remoteGrantResume: (id: string) => invoke<void>("remote_grant_resume", { id }),
  remoteOauthPendingList: () => invoke<OAuthPending[]>("remote_oauth_pending_list"),
  remoteOauthDecide: (txnId: string, approve: boolean) => invoke<void>("remote_oauth_decide", { txnId, approve }),
  remoteOauthTokensList: () => invoke<OAuthToken[]>("remote_oauth_tokens_list"),
  remoteOauthTokenRevoke: (id: string) => invoke<void>("remote_oauth_token_revoke", { id }),
  remoteCloudflaredInstall: () => invoke<string>("remote_cloudflared_install"),
  remoteCloudflaredStatus: () => invoke<{ installed: boolean; path: string; version: string }>("remote_cloudflared_status"),
  remoteCloudflaredSetPath: (path: string) => invoke<void>("remote_cloudflared_set_path", { path }),
  remoteTunnelStart: () => invoke<void>("remote_tunnel_start"),
  remoteTimerSet: (hours: number) => invoke<void>("remote_timer_set", { hours }),
  systemAfterAction: (kind: string, command: string) => invoke<void>("system_after_action", { kind, command }),
  remoteTunnelStop: () => invoke<void>("remote_tunnel_stop"),
  dataExport: () => invoke<string>("data_export"),
  dataImport: (path: string) => invoke<string>("data_import", { path }),
  skinImport: (path: string) => invoke<Record<string, unknown>>("skin_import", { path }),
  skinsList: () => invoke<Record<string, unknown>[]>("skins_list"),
  skinAssetData: (skinDir: string, file: string) => invoke<string>("skin_asset_data", { skinDir, file }),
  clipboardWriteText: (text: string) => invoke<void>("clipboard_write_text", { text }),
  clipboardReadText: () => invoke<string>("clipboard_read_text"),
  uiLog: (kind: string, text: string) => invoke<void>("ui_log", { kind, text }).catch(() => {}),
  nodeDownload: () => invoke<string>("node_download"),
  agentConfigGet: (agent_type: string) => invoke<AgentLaunch>("agent_config_get", { agentType: agent_type }),
  agentConfigSet: (agent_type: string, launch: AgentLaunch | null) =>
    invoke<void>("agent_config_set", { agentType: agent_type, launch }),
  agentsRegistry: () => invoke<AgentEnvStatusItem[]>("agents_registry"),
  agentsEnabledGet: () => invoke<string[]>("agents_enabled_get"),
  agentsEnabledSet: (ids: string[]) => invoke<void>("agents_enabled_set", { ids }),
  agentsBootstrap: (id: string) => invoke<string>("agents_bootstrap", { id }),
  agentsUninstall: (id: string) => invoke<string>("agents_uninstall", { id }),
};
