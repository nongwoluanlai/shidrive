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
  GlobalLogEntry,
  SearchOpts,
  SearchResult,
  AssistantConfig,
  AssistantConfigView,
  AssistantMemory,
  AssistantMessage,
  AssistantPersona,
  AssistantTurnResult,
  SpeechHint,
  VoiceConfig,
  VoicePack,
  VoiceStatus,
  VoiceView,
} from "./types";

/** 每个工作流保留的运行历史条数，与后端 db::MAX_RUNS_PER_WORKFLOW 一致 */
export interface WebSessionInfo { ip: string; ua: string; loginSecsAgo: number; activeSecsAgo: number }
export interface WebStatus {
  running: boolean; enabled: boolean; port: number; listen: string; lan: boolean;
  public_url: string; guest_code: string; started_at: string | null;
  sessions: WebSessionInfo[]; clients: number; local_urls: string[]; logs: string[];
  assets_ready: boolean; last_error: string;
}

export const RUN_HISTORY_LIMIT = 50;

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
  acpSessionNew: (context: Context, agent_type: string, fresh = false) => invoke<string>("acp_session_new", { context, agentType: agent_type, fresh }),
  deepseekSetSource: (mode: string) => invoke<void>("deepseek_set_source", { mode }),
  deepseekRedetect: () => invoke<string | null>("deepseek_redetect"),
  deepseekResolveDsh: (path: string) => invoke<string>("deepseek_resolve_dsh", { path }),
  pickFolder: (title?: string, initial?: string) => invoke<string | null>("pick_folder", { title: title ?? null, initial: initial ?? null }),
  acpPiLogin: (cwd?: string) => invoke<void>("acp_pi_login", { cwd: cwd ?? null }),
  acpDeepseekNew: (context: Context) => invoke<string>("acp_deepseek_new", { context }),
  acpPrompt: (context: Context, agent_type: string, text: string, images?: { data: string; mime: string }[], requestId?: string) =>
    invoke<{ stopReason?: string }>("acp_prompt", { context, agentType: agent_type, text, images: images ?? [], requestId }),
  acpCancel: (context_id: string, agent_type: string) => invoke<void>("acp_cancel", { contextId: context_id, agentType: agent_type }),
  acpSetMode: (context_id: string, agent_type: string, mode_id: string) =>
    invoke<void>("acp_set_mode", { contextId: context_id, agentType: agent_type, modeId: mode_id }),
  acpSetConfigOption: (context_id: string, agent_type: string, option_id: string, value: unknown, expectedSessionId?: string) =>
    invoke<void>("acp_set_config_option", { contextId: context_id, agentType: agent_type, optionId: option_id, value, expectedSessionId }),
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
  fsSearch: (opts: SearchOpts) => invoke<SearchResult>("fs_search", { opts }),
  fsSearchCancel: () => invoke<void>("fs_search_cancel"),
  /** 目录签名（子项名+类型哈希），null = 目录不可读/已删除 */
  fsDirSigs: (paths: string[]) => invoke<(string | null)[]>("fs_dir_sigs", { paths }),
  globalLogList: () => invoke<GlobalLogEntry[]>("global_log_list"),
  globalLogClear: () => invoke<void>("global_log_clear"),
  globalLogPush: (level: string, category: string, message: string) => invoke<void>("global_log_push", { level, category, message }),
  /** 弹窗提醒（text=「标题:内容」）；点击后后端唤出主窗口并发 notify://clicked */
  systemNotify: (text: string, contextId: string, agentType: string) => invoke<void>("system_notify", { text, contextId, agentType }),
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
  runsList: (workflow_id: string, limit = RUN_HISTORY_LIMIT) => invoke<WorkflowRun[]>("runs_list", { workflowId: workflow_id, limit }),

  // settings
  settingsGet: (key: string) => invoke<string | null>("settings_get", { key }),
  settingsSet: (key: string, value: string) => invoke<void>("settings_set", { key, value }),
  setupStatus: () => invoke<SetupStatus>("setup_status"),
  nodeStatus: () => invoke<NodeStat>("node_status"),
  remoteMcpStatus: () => invoke<any>("remote_mcp_status"),
  remoteMcpStart: (opts: { listenHost?: string; port?: number; quickTunnel?: boolean }) =>
    invoke<void>("remote_mcp_start", { opts: { listenHost: opts.listenHost ?? "0.0.0.0", port: opts.port ?? 51688, quickTunnel: !!opts.quickTunnel } }),
  remoteMcpStop: () => invoke<void>("remote_mcp_stop"),
  // 网站接入（网页版服务）
  webStatus: () => invoke<WebStatus>("web_status"),
  webStart: () => invoke<WebStatus>("web_start"),
  webStop: () => invoke<WebStatus>("web_stop"),
  webConfigSet: (c: { port?: number; publicUrl?: string; guestCode?: string; lan?: boolean }) => invoke<WebStatus>("web_config_set", c),
  webRevokeAll: () => invoke<number>("web_revoke_all"),
  webGenerateCode: () => invoke<string>("web_generate_code"),
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
  /** zip 内容以二进制请求体发送（文件选择器 / HTML5 拖放拿不到路径，只有字节） */
  skinImportBytes: (bytes: Uint8Array) => invoke<Record<string, unknown>>("skin_import_bytes", bytes),
  skinDelete: (id: string) => invoke<void>("skin_delete", { id }),
  skinsList: () => invoke<Record<string, unknown>[]>("skins_list"),
  /** 图片原始字节（前端转 blob: URL；data: URL 超过 2 MiB 会被 Chromium 视为非法而静默不显示） */
  skinAssetRead: (skinDir: string, file: string) => invoke<ArrayBuffer>("skin_asset_read", { skinDir, file }),
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
  // ---- 全局助手 ----
  assistantConfigGet: () => invoke<AssistantConfigView>("assistant_config_get"),
  /** apiKey: undefined/null = 保留已保存的密钥；"" = 清除 */
  assistantConfigSet: (config: AssistantConfig, apiKey?: string | null) => invoke<AssistantConfigView>("assistant_config_set", { config, apiKey: apiKey ?? null }),
  assistantSend: (text: string, persona: AssistantPersona, appState: string | null, turnId: string, speech: SpeechHint | null = null) =>
    invoke<AssistantTurnResult>("assistant_send", { text, persona, appState, turnId, speech }),
  assistantGreet: (persona: AssistantPersona, appState: string | null, turnId: string, reason: "new" | "resume", speech: SpeechHint | null = null) =>
    invoke<AssistantTurnResult>("assistant_greet", { persona, appState, turnId, reason, speech }),
  voiceConfigGet: () => invoke<VoiceView>("voice_config_get"),
  voiceConfigSet: (config: VoiceConfig, baiduSecret?: string | null) => invoke<VoiceView>("voice_config_set", { config, baiduSecret: baiduSecret ?? null }),
  voiceStatus: () => invoke<VoiceStatus>("voice_status"),
  voiceInstall: (id: string) => invoke<void>("voice_install", { id }),
  voiceCancelInstall: (id: string) => invoke<void>("voice_cancel_install", { id }),
  voiceRemove: (id: string) => invoke<VoiceStatus>("voice_remove", { id }),
  voicePickPack: () => invoke<string | null>("voice_pick_pack"),
  voiceImportPack: (path: string, overwrite = false) => invoke<VoicePack>("voice_import_pack", { path, overwrite }),
  voiceOpenDir: () => invoke<void>("voice_open_dir"),
  voiceStt: (pcm: string, rate = 16000, engine?: "local" | "baidu") => invoke<{ text: string; engine: string }>("voice_stt", { pcm, rate, engine: engine ?? null }),
  voiceTts: (text: string, voice: string, speaker?: number) => invoke<{ rate: number; pcm: string; seconds: number }>("voice_tts", { text, voice, speaker: speaker ?? null }),
  voiceStopServer: () => invoke<void>("voice_stop_server"),
  voiceKwsFeed: (pcm: string, rate = 16000) => invoke<{ hits: string[]; tokens: string[] }>("voice_kws_feed", { pcm, rate }),
  voiceKwsStop: () => invoke<void>("voice_kws_stop"),
  voiceWakeRaise: () => invoke<void>("voice_wake_raise"),
  assistantCancel: (turnId: string) => invoke<void>("assistant_cancel", { turnId }),
  assistantSessionReset: () => invoke<AssistantConfigView>("assistant_session_reset"),
  assistantMessages: (limit?: number) => invoke<AssistantMessage[]>("assistant_messages", { limit: limit ?? null }),
  assistantMessagesClear: () => invoke<void>("assistant_messages_clear"),
  assistantMemories: () => invoke<AssistantMemory[]>("assistant_memories"),
  assistantMemoryAdd: (content: string, pinned = false) => invoke<boolean>("assistant_memory_add", { content, pinned }),
  assistantMemoryUpdate: (id: string, content: string, pinned: boolean) => invoke<void>("assistant_memory_update", { id, content, pinned }),
  assistantMemoryDelete: (id: string) => invoke<void>("assistant_memory_delete", { id }),
  assistantMemorySummarize: (mode: "consolidate" | "extract", persona: AssistantPersona, turnId: string) =>
    invoke<number>("assistant_memory_summarize", { mode, persona, turnId }),
  assistantModels: (provider: "acp" | "api", opts: { agentType?: string; baseUrl?: string; apiKey?: string } = {}) =>
    invoke<{ models?: unknown; configOptions?: unknown }>("assistant_models", { provider, agentType: opts.agentType ?? null, baseUrl: opts.baseUrl ?? null, apiKey: opts.apiKey ?? null }),
};
