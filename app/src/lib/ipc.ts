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
  ScCommit,
  ScheduleConfig,
  SessionInfo,
  SetupStatus,
  NodeStat,
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
  projectsDelete: (id: string) => invoke<void>("projects_delete", { id }),

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
  acpPrompt: (context: Context, agent_type: string, text: string, images?: { data: string; mime: string }[]) =>
    invoke<{ stopReason?: string }>("acp_prompt", { context, agentType: agent_type, text, images: images ?? [] }),
  acpCancel: (context_id: string, agent_type: string) => invoke<void>("acp_cancel", { contextId: context_id, agentType: agent_type }),
  acpSetMode: (context_id: string, agent_type: string, mode_id: string) =>
    invoke<void>("acp_set_mode", { contextId: context_id, agentType: agent_type, modeId: mode_id }),
  acpSetConfigOption: (context_id: string, agent_type: string, option_id: string, value: unknown) =>
    invoke<void>("acp_set_config_option", { contextId: context_id, agentType: agent_type, optionId: option_id, value }),
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
  workflowUpdate: (workflow: Workflow) => invoke<void>("workflow_update", { workflow }),
  workflowDelete: (id: string) => invoke<void>("workflow_delete", { id }),
  workflowRun: (id: string) => invoke<void>("workflow_run", { id }),
  workflowStop: (run_id: string) => invoke<void>("workflow_stop", { runId: run_id }),
  runsList: (workflow_id: string, limit = 30) => invoke<WorkflowRun[]>("runs_list", { workflowId: workflow_id, limit }),

  // settings
  settingsGet: (key: string) => invoke<string | null>("settings_get", { key }),
  settingsSet: (key: string, value: string) => invoke<void>("settings_set", { key, value }),
  setupStatus: () => invoke<SetupStatus>("setup_status"),
  nodeStatus: () => invoke<NodeStat>("node_status"),
  uiLog: (kind: string, text: string) => invoke<void>("ui_log", { kind, text }).catch(() => {}),
  nodeDownload: () => invoke<string>("node_download"),
  agentConfigGet: (agent_type: string) => invoke<AgentLaunch>("agent_config_get", { agentType: agent_type }),
  agentConfigSet: (agent_type: string, launch: AgentLaunch | null) =>
    invoke<void>("agent_config_set", { agentType: agent_type, launch }),
  agentsRegistry: () => invoke<AgentEnvStatusItem[]>("agents_registry"),
  agentsEnabledGet: () => invoke<string[]>("agents_enabled_get"),
  agentsEnabledSet: (ids: string[]) => invoke<void>("agents_enabled_set", { ids }),
  agentsBootstrap: (id: string) => invoke<string>("agents_bootstrap", { id }),
};
