//! Workflow engine: graph execution (parallel branches, free-form edges), manual/scheduled.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Local;
use futures::stream::{FuturesUnordered, StreamExt};
use std::future::Future;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::db::Db;
use crate::manager::{AgentManager, SessionTarget};
use crate::models::*;

pub const EVT_WF_LOG: &str = "wf://log";
pub const EVT_WF_STATUS: &str = "wf://status";

pub use crate::schedule::{compute_next, fmt_time, parse_time};

struct RunControl {
    stop: Arc<AtomicBool>,
    /// pids of running shell children (killed via taskkill /T)
    shell_pids: Mutex<Vec<u32>>,
}

pub struct Engine {
    pub app: AppHandle,
    pub db: Arc<Db>,
    pub agents: Arc<AgentManager>,
    controls: Mutex<HashMap<String, Arc<RunControl>>>,
    /// workflow_id -> run_id currently executing
    active: Mutex<HashMap<String, String>>,
}

/// Validated execution graph.
struct Graph {
    outs: Vec<Vec<usize>>,
    indeg0: Vec<usize>,
}

fn build_graph(n: usize, edges: &[(usize, usize)]) -> Graph {
    let mut outs = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for (f, t) in edges {
        outs[*f].push(*t);
        indeg[*t] += 1;
    }
    let indeg0 = (0..n).filter(|&i| indeg[i] == 0).collect();
    Graph { outs, indeg0 }
}

/// Resolve the edge list: user edges when valid & acyclic, else a sequential chain.
fn effective_edges(w: &Workflow) -> (Vec<(usize, usize)>, bool) {
    let n = w.steps.len();
    if n == 0 {
        return (vec![], true);
    }
    let seq: Vec<(usize, usize)> = (0..n - 1).map(|i| (i, i + 1)).collect();
    if w.edges.is_empty() {
        return (seq, true);
    }
    let mut raw: Vec<(usize, usize)> = w.edges.iter().map(|e| (e.from as usize, e.to as usize)).collect();
    raw.sort_unstable();
    raw.dedup();
    let bounds_ok = raw.iter().all(|(f, t)| f < &n && t < &n && f != t);
    if !bounds_ok {
        return (seq, false);
    }
    // cycle check (Kahn)
    let g = build_graph(n, &raw);
    let mut indeg: Vec<usize> = (0..n).map(|i| g.outs.iter().filter(|o| o.contains(&i)).count()).collect();
    let mut q: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut seen = 0;
    while let Some(i) = q.pop_front() {
        seen += 1;
        for &t in &g.outs[i] {
            indeg[t] -= 1;
            if indeg[t] == 0 {
                q.push_back(t);
            }
        }
    }
    if seen == n {
        (raw, true)
    } else {
        (seq, false)
    }
}

impl Engine {
    pub fn new(app: AppHandle, db: Arc<Db>, agents: Arc<AgentManager>) -> Self {
        let _ = db.fail_stale_runs();
        Self { app, db, agents, controls: Mutex::new(HashMap::new()), active: Mutex::new(HashMap::new()) }
    }

    pub fn start(self: &Arc<Self>) {
        let eng = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(e) = eng.tick_once().await {
                    log::error!("scheduler tick error: {e}");
                }
            }
        });
    }

    async fn tick_once(self: &Arc<Self>) -> Result<(), String> {
        // `scheduled_workflows` joins on `projects`, so a workflow whose project was
        // removed can never be picked up here even if a stale row survived.
        let workflows = self.db.scheduled_workflows()?;
        let now = Local::now();
        for w in workflows {
            let Some(sched) = w.schedule.clone() else { continue };
            match w.next_run_at.as_deref().and_then(parse_time) {
                None => {
                    // No (valid) next time yet: arm it — or retire the workflow when the
                    // schedule has no future occurrence (e.g. a one-shot in the past).
                    match compute_next(&sched, now) {
                        Some(next) => self.db.update_workflow_times(&w.id, w.last_run_at.as_deref(), Some(&fmt_time(next)))?,
                        None => {
                            log::warn!("workflow {} ({}) has no future occurrence; disabling", w.name, w.id);
                            self.db.set_workflow_enabled(&w.id, false)?;
                        }
                    }
                }
                Some(due) if due <= now => {
                    if self.active.lock().unwrap().contains_key(&w.id) {
                        continue;
                    }
                    // Consume the slot *before* enqueueing: a one-shot is retired here no
                    // matter how the run ends (success / failure / stopped), so it can
                    // never be re-run every tick. The claim is conditional on the row still
                    // carrying the `next_run_at` we read, so an edit or delete that raced
                    // with this tick wins and the stale copy is not executed.
                    let next = compute_next(&sched, now);
                    let retire = matches!(sched, ScheduleConfig::Once { .. }) || next.is_none();
                    let claimed = self.db.claim_scheduled_run(
                        &w.id,
                        w.next_run_at.as_deref(),
                        &fmt_time(now),
                        next.map(fmt_time).as_deref(),
                        retire,
                    )?;
                    if claimed {
                        self.spawn_run(&w, "schedule");
                    }
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Stop every in-flight run of the given workflows (used when their project is
    /// deleted). Returns how many runs received a stop request.
    pub fn stop_workflows(&self, workflow_ids: &[String]) -> usize {
        let run_ids: Vec<String> = {
            let active = self.active.lock().unwrap();
            workflow_ids.iter().filter_map(|id| active.get(id).cloned()).collect()
        };
        for run_id in &run_ids {
            self.stop_run(run_id);
        }
        run_ids.len()
    }

    /// Currently executing runs (for the 运行中任务 panel).
    pub fn active_runs(&self) -> Vec<(String, String, String, String)> {
        let active = self.active.lock().unwrap();
        active
            .iter()
            .filter_map(|(wf_id, run_id)| {
                let name = self.db.get_workflow(wf_id).ok().flatten().map(|w| w.name).unwrap_or_else(|| wf_id.clone());
                self.db.get_run(run_id).ok().flatten().map(|r| (run_id.clone(), wf_id.clone(), name, r.started_at))
            })
            .collect()
    }

    pub fn run_now(self: &Arc<Self>, workflow_id: &str) -> Result<(), String> {
        let w = self.db.get_workflow(workflow_id)?.ok_or("工作流不存在")?;
        if self.active.lock().unwrap().contains_key(workflow_id) {
            return Err("该工作流正在运行".into());
        }
        self.spawn_run(&w, "manual");
        Ok(())
    }

    pub fn stop_run(&self, run_id: &str) {
        if let Some(ctrl) = self.controls.lock().unwrap().get(run_id) {
            ctrl.stop.store(true, Ordering::SeqCst);
            for pid in ctrl.shell_pids.lock().unwrap().drain(..) {
                kill_tree(pid);
            }
            // Agent nodes observe stop and drop their prompt future. Its session
            // guard cancels the actual Temp/Explicit session, not the UI binding.
        }
    }

    fn spawn_run(self: &Arc<Self>, w: &Workflow, trigger: &str) {
        let run_id = match self.db.create_run(&w.id, trigger) {
            Ok(id) => id,
            Err(e) => {
                log::error!("create_run failed: {e}");
                return;
            }
        };
        self.active.lock().unwrap().insert(w.id.clone(), run_id.clone());

        let ctrl = Arc::new(RunControl {
            stop: Arc::new(AtomicBool::new(false)),
            shell_pids: Mutex::new(Vec::new()),
        });
        self.controls.lock().unwrap().insert(run_id.clone(), ctrl.clone());

        let eng = self.clone();
        let wf = w.clone();
        let run_id2 = run_id.clone();
        tauri::async_runtime::spawn(async move {
            let _ = eng.app.emit(EVT_WF_STATUS, json!({ "runId": run_id2, "workflowId": wf.id, "status": "running" }));
            let status = eng.execute(&wf, &run_id2, ctrl).await;
            let _ = eng.db.finish_run(&run_id2, &status);
            let _ = eng.app.emit(EVT_WF_STATUS, json!({ "runId": run_id2, "workflowId": wf.id, "status": status }));
            eng.controls.lock().unwrap().remove(&run_id2);
            eng.active.lock().unwrap().remove(&wf.id);
            // One-shot schedules are retired when the run is *claimed* (tick_once), not
            // here: doing it on success only made failed/stopped runs repeat forever, and
            // writing back the stale `wf` copy clobbered edits made during the run.
        });
    }

    /// Execute the workflow graph. Nodes whose predecessors finished run in parallel.
    async fn execute(&self, w: &Workflow, run_id: &str, ctrl: Arc<RunControl>) -> String {
        let n = w.steps.len();
        let project_root = self
            .db
            .list_projects()
            .ok()
            .and_then(|ps| ps.into_iter().find(|p| p.id == w.project_id))
            .map(|p| p.root_path)
            .unwrap_or_default();

        if n == 0 {
            self.log(run_id, "工作流没有节点");
            return "success".into();
        }

        let (edges, valid) = effective_edges(w);
        if !valid {
            self.log(run_id, "⚠ 连线无效或存在循环，已回退为顺序执行");
        }
        let graph = build_graph(n, &edges);
        let env: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new({
            let mut m = w.env.clone();
            m.entry("__root__".to_string()).or_insert_with(|| project_root.clone());
            m.entry("__app__".to_string()).or_insert_with(|| {
                std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
            });
            m
        }));

        self.log(run_id, &format!("▶ 工作流「{}」开始（{} 个节点，{} 条连线）", w.name, n, edges.len()));

        let mut indeg: Vec<usize> = (0..n).map(|i| graph.outs.iter().filter(|o| o.contains(&i)).count()).collect();
        // "开始"节点存在时，从开始节点起跑（忽略其他无入度节点）
        let starts: Vec<usize> = (0..n).filter(|&i| w.steps[i].is_start()).collect();
        let roots: Vec<usize> = if starts.is_empty() {
            graph.indeg0.clone()
        } else {
            for &i in &starts {
                indeg[i] = 0;
            }
            starts
        };
        let mut futures: FuturesUnordered<std::pin::Pin<Box<dyn Future<Output = (usize, bool)> + Send>>> = FuturesUnordered::new();
        for &i in &roots {
            futures.push(Box::pin(self.run_node(run_id, ctrl.clone(), env.clone(), w, i)));
        }
        let mut failed_at: Option<usize> = None;
        let mut stopped = false;

        // drains naturally: successors are pushed as predecessors complete
        while let Some((idx, ok)) = futures.next().await {
            if ctrl.stop.load(Ordering::SeqCst) && failed_at.is_none() {
                stopped = true;
            }
            let step_failed = !ok && !w.steps[idx].continue_on_error();
            if step_failed && failed_at.is_none() && !stopped {
                failed_at = Some(idx);
                ctrl.stop.store(true, Ordering::SeqCst);
                for pid in ctrl.shell_pids.lock().unwrap().drain(..) {
                    kill_tree(pid);
                }
                self.log(run_id, &format!("✕ 节点「{}」失败，已停止其余节点", step_name(&w.steps[idx], idx)));
            }
            if !stopped && failed_at.is_none() {
                for &t in &graph.outs[idx] {
                    indeg[t] -= 1;
                    if indeg[t] == 0 {
                        futures.push(Box::pin(self.run_node(run_id, ctrl.clone(), env.clone(), w, t)));
                    }
                }
            }
        }

        if stopped {
            self.log(run_id, "■ 已手动停止");
            return "stopped".into();
        }
        if let Some(i) = failed_at {
            self.log(run_id, &format!("✕ 工作流在节点「{}」失败后中止", step_name(&w.steps[i], i)));
            return "failed".into();
        }
        self.log(run_id, "✔ 全部节点完成");
        "success".into()
    }

    /// Run a single node (with template substitution against current env).
    async fn run_node(
        &self,
        run_id: &str,
        ctrl: Arc<RunControl>,
        env: Arc<Mutex<BTreeMap<String, String>>>,
        w: &Workflow,
        i: usize,
    ) -> (usize, bool) {
        if ctrl.stop.load(Ordering::SeqCst) {
            self.log(run_id, &format!("— 跳过节点「{}」（工作流已停止）", step_name(&w.steps[i], i)));
            return (i, false);
        }
        let label = step_name(&w.steps[i], i);
        self.log(run_id, &format!("— 节点 {}：「{}」", i + 1, label));
        let step = &w.steps[i];
        let snapshot = |env: &Arc<Mutex<BTreeMap<String, String>>>| env.lock().unwrap().clone();
        let snap = snapshot(&env);

        let ok = match step {
            WorkflowStep::Shell { command, cwd, shell, .. } => {
                let command = substitute(command, &snap);
                let cwd = substitute(cwd, &snap);
                let python_path = if shell == "python" {
                    Some(
                        self.db
                            .get_setting("tools.python")
                            .ok()
                            .flatten()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| self.agents.setup_status().python_path),
                    )
                } else {
                    None
                };
                let shell_kind = if shell == "python" {
                    "python"
                } else if shell == "powershell" {
                    "powershell"
                } else {
                    "cmd"
                };
                let (ok, exit_code, stdout_txt, stderr_txt) =
                    self.run_shell(run_id, ctrl.clone(), &command, &cwd, &snap.get("__root__").cloned().unwrap_or_default(), step.timeout(), shell_kind, python_path.as_deref(), &snap).await;
                let mut e = env.lock().unwrap();
                let key = step_output_key(step, i);
                e.insert(format!("{key}_exit"), exit_code.to_string());
                e.insert(format!("{key}_stdout"), truncate_env(&stdout_txt));
                e.insert(format!("{key}_stderr"), truncate_env(&stderr_txt));
                ok
            }
            WorkflowStep::Agent { context_id, agent_type, prompt, session_id, .. } => {
                let prompt = substitute(prompt, &snap);
                let target = match session_id {
                    Some(sid) if !sid.trim().is_empty() => SessionTarget::Explicit(sid.trim().to_string()),
                    _ => SessionTarget::Temp,
                };
                self.run_agent(run_id, ctrl.clone(), context_id, agent_type, target, &prompt, step.timeout()).await
            }
            WorkflowStep::Start { .. } => true,
            WorkflowStep::Note { .. } => {
                // 注释节点：不执行任何操作
                true
            }
            WorkflowStep::Balloon { title, message, click_action, click_target, sound, .. } => {
                let title = substitute(title, &snap);
                let message = substitute(message, &snap);
                let target = substitute(click_target, &snap);
                self.show_balloon(run_id, &title, &message, click_action, &target, *sound)
            }
            WorkflowStep::Delay { seconds, .. } => {
                for _ in 0..*seconds {
                    if ctrl.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                true
            }
            WorkflowStep::EnvSet { vars, .. } => {
                let mut e = env.lock().unwrap();
                for (k, v) in vars {
                    let val = substitute(v, &snap);
                    e.insert(k.clone(), val.clone());
                    drop(e);
                    self.log(run_id, &format!("env {k} = {val}"));
                    e = env.lock().unwrap();
                }
                true
            }
        };
        (i, ok)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_shell(
        &self,
        run_id: &str,
        ctrl: Arc<RunControl>,
        command: &str,
        cwd: &str,
        project_root: &str,
        timeout_sec: Option<u64>,
        shell_kind: &str,
        python_path: Option<&str>,
        env: &BTreeMap<String, String>,
    ) -> (bool, i32, String, String) {
        let dir = if !cwd.is_empty() {
            cwd.to_string()
        } else if !project_root.is_empty() {
            project_root.to_string()
        } else {
            std::env::temp_dir().to_string_lossy().to_string()
        };

        self.log(run_id, &format!("$ [{shell_kind}] {command}   （目录：{dir}）"));
        let mut cmd = shell_command(shell_kind, command, python_path);
        cmd.current_dir(&dir).envs(env);
        execute_shell(cmd, ctrl, timeout_sec, |line| self.log(run_id, line)).await
    }

    async fn run_agent(
        &self,
        run_id: &str,
        ctrl: Arc<RunControl>,
        context_id: &str,
        agent_type: &str,
        target: SessionTarget,
        prompt: &str,
        timeout_sec: Option<u64>,
    ) -> bool {
        let Some(ctx) = self.db.get_context_row(context_id) else {
            self.log(run_id, &format!("上下文 {context_id} 不存在"));
            return false;
        };
        let target_note = match &target {
            SessionTarget::Explicit(sid) => format!("（复用会话 {sid}）"),
            SessionTarget::Temp => "（临时会话）".to_string(),
            SessionTarget::Binding => "（绑定会话）".to_string(),
        };
        self.log(run_id, &format!("→ [{agent_type}]{target_note} {}", first_line(prompt)));

        let agents = self.agents.clone();
        let result = tokio::select! {
            result = wait_for_agent(agents.prompt_with(&ctx, agent_type, target, prompt, &[]), timeout_sec) => result,
            _ = wait_for_stop(ctrl.stop.clone()) => Err("Agent 节点已停止".to_string()),
        };

        match result {
            Ok(res) => {
                let stop = res.get("stopReason").and_then(|s| s.as_str()).unwrap_or("?").to_string();
                self.log(run_id, &format!("← [{agent_type}] 回合结束（stop={stop}）"));
                true
            }
            Err(e) => {
                self.log(run_id, &format!("Agent 节点失败: {e}"));
                false
            }
        }
    }

    /// Windows 气泡提醒（系统托盘 balloon，与旧使驾同体验）。
    /// 支持点击行为：open=打开目录/文件位置，url=浏览器打开；sound=伴随提示音。
    fn show_balloon(&self, run_id: &str, title: &str, message: &str, click_action: &str, click_target: &str, sound: bool) -> bool {
        let ps_escape = |v: &str| v.replace("`", "``").replace("\"", "`\"");
        let action = if click_target.trim().is_empty() { "none" } else { click_action };
        let target = ps_escape(click_target.trim());
        let click_block = match action {
            "open" => r#"$onClick = { try { Start-Process -FilePath explorer.exe -ArgumentList "`"$tgt`"" } catch {}; $n.Visible=$false; $n.Dispose() }; $n.add_BalloonTipClicked($onClick); $n.add_Click($onClick);"#,
            "url" => r#"$onClick = { try { Start-Process "`"$tgt`"" } catch {}; $n.Visible=$false; $n.Dispose() }; $n.add_BalloonTipClicked($onClick); $n.add_Click($onClick);"#,
            _ => "",
        };
        let sound_line = if sound { "Add-Type -AssemblyName System; [System.Media.SystemSounds]::Asterisk.Play();" } else { "" };
        let script = format!(
            r#"$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $n=New-Object System.Windows.Forms.NotifyIcon; $n.Icon=[System.Drawing.SystemIcons]::Information; $n.Visible=$true; $tgt="{target}"; {click_block} $n.ShowBalloonTip(8000,'{t}','{m}','Info'); {sound_line} Start-Sleep -Seconds 8; $n.Visible=$false; $n.Dispose()"#,
            target = target,
            click_block = click_block,
            t = title.replace('\'', "''"),
            m = message.replace('\'', "''"),
            sound_line = sound_line,
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .creation_flags_cfg()
            .output();
        match out {
            Ok(o) if o.status.success() => {

                self.log(run_id, &format!("🔔 已发送气泡提醒：{title}"));
                true
            }
            Ok(o) => {
                let err_txt = String::from_utf8_lossy(&o.stderr).trim().to_string();
                self.log(run_id, &format!("⚠ 气泡提醒失败: {}", err_txt.chars().take(180).collect::<String>()));
                true // 提醒失败不阻塞工作流
            }
            Err(e) => {
                self.log(run_id, &format!("⚠ 气泡提醒启动失败: {e}"));
                true
            }
        }
    }

    fn log(&self, run_id: &str, line: &str) {
        let ts = Local::now().format("%H:%M:%S");
        let full = format!("[{ts}] {line}");
        let _ = self.db.append_run_log(run_id, &full);
        let _ = self.app.emit(EVT_WF_LOG, json!({ "runId": run_id, "line": full }));
    }
}

async fn wait_for_stop(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_agent<T>(future: impl Future<Output = Result<T, String>>, timeout_sec: Option<u64>) -> Result<T, String> {
    match timeout_sec {
        Some(t) => tokio::time::timeout(Duration::from_secs(t), future).await
            .map_err(|_| format!("Agent 节点超时（{t}s）"))?,
        None => future.await,
    }
}

/// cmd.exe parses its own command tail; CRT argument escaping changes embedded
/// quotes into literal backslashes. /S strips only our extra outer quote pair.
fn shell_command(kind: &str, command: &str, python_path: Option<&str>) -> tokio::process::Command {
    match kind {
        "python" => {
            let mut cmd = tokio::process::Command::new(python_path.filter(|p| !p.is_empty()).unwrap_or("python"));
            cmd.arg("-c").arg(command);
            cmd
        }
        "powershell" => {
            let mut cmd = tokio::process::Command::new("powershell");
            cmd.args(["-NoProfile", "-NonInteractive", "-Command", command]);
            cmd
        }
        _ => {
            let mut cmd = tokio::process::Command::new("cmd");
            let command = command.replace("\r\n", " & ").replace('\n', " & ");
            cmd.args(["/d", "/s", "/c"]);
            #[cfg(windows)]
            cmd.raw_arg(format!("\"{command}\""));
            #[cfg(not(windows))]
            cmd.arg(command);
            cmd
        }
    }
}

// Bound both retained output and individual streamed lines. Read bytes rather
// than UTF-8 lines: a single legacy-codepage byte must not stop draining a pipe.
const MAX_CAPTURE_BYTES: usize = 80_000;
const MAX_LOG_LINE_BYTES: usize = 4096;
#[derive(Default)]
struct PipeCapture {
    bytes: Vec<u8>,
    line: Vec<u8>,
    lines: usize,
    truncated: bool,
}

impl PipeCapture {
    fn emit_line(&mut self, stderr: bool, log: &impl Fn(&str)) {
        if self.lines < if stderr { 100 } else { 200 } {
            let text = String::from_utf8_lossy(&self.line);
            let text = text.trim_end_matches('\r');
            if stderr { log(&format!("  [stderr] {text}")); } else { log(text); }
        }
        self.lines += 1;
        self.line.clear();
    }

    fn push(&mut self, bytes: &[u8], stderr: bool, log: &impl Fn(&str)) {
        let keep = bytes.len().min(MAX_CAPTURE_BYTES - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..keep]);
        self.truncated |= keep < bytes.len();
        for &byte in bytes {
            if byte == b'\n' {
                self.emit_line(stderr, log);
            } else {
                self.line.push(byte);
                if self.line.len() == MAX_LOG_LINE_BYTES {
                    self.emit_line(stderr, log);
                }
            }
        }
    }

    fn finish(mut self, stderr: bool, log: &impl Fn(&str)) -> String {
        if !self.line.is_empty() { self.emit_line(stderr, log); }
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated { text.push_str("\n…（输出过长，已截断）"); }
        text
    }
}

async fn execute_shell(
    mut cmd: tokio::process::Command,
    ctrl: Arc<RunControl>,
    timeout_sec: Option<u64>,
    log: impl Fn(&str),
) -> (bool, i32, String, String) {
    use tokio::io::AsyncReadExt;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let deadline = timeout_sec.and_then(|t| tokio::time::Instant::now().checked_add(Duration::from_secs(t)));
    if ctrl.stop.load(Ordering::SeqCst) {
        return (false, -1, String::new(), String::new());
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            log(&format!("启动失败: {e}"));
            return (false, -1, String::new(), e.to_string());
        }
    };
    crate::child_job::attach(&child); // 随宿主退出回收（含强杀）
    let pid = child.id().unwrap_or(0);
    ctrl.shell_pids.lock().unwrap().push(pid);
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (mut out, mut err) = (PipeCapture::default(), PipeCapture::default());
    let (mut out_bytes, mut err_bytes) = ([0u8; 8192], [0u8; 8192]);
    let (mut out_open, mut err_open) = (true, true);
    let mut status = None;
    let mut drain_deadline = None;
    let mut failure = None;
    let mut poll = tokio::time::interval(Duration::from_millis(25));
    loop {
        let now = tokio::time::Instant::now();
        if ctrl.stop.load(Ordering::SeqCst) {
            failure = Some("命令已被终止".to_string());
            break;
        }
        if deadline.is_some_and(|d| now >= d) {
            failure = Some(format!("命令超时（{}s），已终止", timeout_sec.unwrap()));
            break;
        }
        if status.is_some() && !out_open && !err_open { break; }
        // A detached descendant may inherit stdout/stderr indefinitely. The
        // shell's exit must not leave a collector task or block the workflow.
        if drain_deadline.is_some_and(|d| now >= d) {
            log("子进程已退出；输出管道仍被后代进程占用，停止收集");
            break;
        }
        tokio::select! {
            _ = poll.tick() => {},
            result = child.wait(), if status.is_none() => {
                match result {
                    Ok(exit) => {
                        status = Some(exit);
                        ctrl.shell_pids.lock().unwrap().retain(|&p| p != pid);
                        drain_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(1));
                    }
                    Err(e) => { failure = Some(format!("等待命令失败: {e}")); break; }
                }
            },
            result = stdout.read(&mut out_bytes), if out_open => {
                match result {
                    Ok(0) => out_open = false,
                    Ok(n) => out.push(&out_bytes[..n], false, &log),
                    Err(e) => { failure = Some(format!("读取 stdout 失败: {e}")); break; }
                }
            },
            result = stderr.read(&mut err_bytes), if err_open => {
                match result {
                    Ok(0) => err_open = false,
                    Ok(n) => err.push(&err_bytes[..n], true, &log),
                    Err(e) => { failure = Some(format!("读取 stderr 失败: {e}")); break; }
                }
            },
        }
    }
    if failure.is_some() && status.is_none() {
        // Only kill a still-owned process ID; a reaped PID may be reused.
        if pid != 0 {
            let _ = tokio::task::spawn_blocking(move || kill_tree(pid)).await;
        }
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    }
    ctrl.shell_pids.lock().unwrap().retain(|&p| p != pid);
    let stdout = out.finish(false, &log);
    let stderr = err.finish(true, &log);
    if let Some(error) = failure {
        log(&error);
        return (false, -1, stdout, stderr);
    }
    let code = status.and_then(|s| s.code()).unwrap_or(-1);
    if code != 0 { log(&format!("命令退出码 {code}")); }
    (code == 0, code, stdout, stderr)
}

fn step_output_key(step: &WorkflowStep, i: usize) -> String {
    match step {
        WorkflowStep::Shell { name, .. } => {
            if name.is_empty() {
                format!("step{}", i + 1)
            } else {
                name.clone()
            }
        }
        _ => format!("step{}", i + 1),
    }
}

fn truncate_env(s: &str) -> String {
    const MAX: usize = 20_000;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let mut cut: String = s.chars().take(MAX).collect();
        cut.push_str("\n…（输出过长，已截断）");
        cut
    }
}

/// Replace `{{env.NAME}}` and `{{date:FORMAT}}` templates.
fn substitute(text: &str, env: &BTreeMap<String, String>) -> String {
    let mut out = text.to_string();
    for _ in 0..5 {
        let Some(start) = out.find("{{date:") else { break };
        // end_rel is relative to `start`; the closing braces start at start+end_rel
        let Some(end_rel) = out[start..].find("}}") else { break };
        // "{{date:" is 7 chars and "}}" is 2 — anything shorter has no format left
        if end_rel < 9 {
            break;
        }
        let fmt = &out[start + 7..start + end_rel];
        let rendered = chrono::Local::now().format(fmt).to_string();
        out.replace_range(start..start + end_rel + 2, &rendered);
    }
    for (k, v) in env {
        let needle = format!("{{{{env.{k}}}}}");
        if out.contains(&needle) {
            out = out.replace(&needle, v);
        }
    }
    out
}

fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .creation_flags_cfg()
        .output();
}

#[cfg(windows)]
trait CreationFlagsCfg {
    fn creation_flags_cfg(&mut self) -> &mut Self;
}
#[cfg(windows)]
impl CreationFlagsCfg for std::process::Command {
    fn creation_flags_cfg(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000)
    }
}
#[cfg(not(windows))]
trait CreationFlagsCfg {
    fn creation_flags_cfg(&mut self) -> &mut Self;
}
#[cfg(not(windows))]
impl CreationFlagsCfg for std::process::Command {
    fn creation_flags_cfg(&mut self) -> &mut Self {
        self
    }
}

fn step_name(step: &WorkflowStep, i: usize) -> String {
    match step {
        WorkflowStep::Note { name, .. } => {
            if name.is_empty() {
                "注释".to_string()
            } else {
                name.clone()
            }
        }
        WorkflowStep::Balloon { name, .. } => {
            if name.is_empty() {
                "气泡提醒".to_string()
            } else {
                name.clone()
            }
        }
        WorkflowStep::Start { name, .. } => {
            if name.is_empty() {
                "开始".to_string()
            } else {
                name.clone()
            }
        }
        WorkflowStep::Shell { name, command, .. } => {
            if name.is_empty() {
                format!("命令 {0}：{1}", i + 1, first_line(command))
            } else {
                name.clone()
            }
        }
        WorkflowStep::Agent { name, prompt, .. } => {
            if name.is_empty() {
                format!("Agent {0}：{1}", i + 1, first_line(prompt))
            } else {
                name.clone()
            }
        }
        WorkflowStep::Delay { name, seconds, .. } => {
            if name.is_empty() {
                format!("等待 {seconds}s")
            } else {
                name.clone()
            }
        }
        WorkflowStep::EnvSet { name, vars, .. } => {
            if name.is_empty() {
                format!("变量（{} 项）", vars.len())
            } else {
                name.clone()
            }
        }
    }
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("");
    if l.chars().count() > 60 {
        let mut cut: String = l.chars().take(60).collect();
        cut.push('…');
        cut
    } else {
        l.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control() -> Arc<RunControl> {
        Arc::new(RunControl {
            stop: Arc::new(AtomicBool::new(false)),
            shell_pids: Mutex::new(Vec::new()),
        })
    }

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/test-fixtures").join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn capture_drains_invalid_utf8_and_bounds_unterminated_lines() {
        let mut output = PipeCapture::default();
        output.push(b"before\xffafter\n", false, &|_| {});
        output.push(&vec![b'x'; MAX_CAPTURE_BYTES * 3], false, &|_| {});
        assert_eq!(output.bytes.len(), MAX_CAPTURE_BYTES);
        assert!(output.line.len() < MAX_LOG_LINE_BYTES);
        let text = output.finish(false, &|_| {});
        assert!(text.starts_with("before\u{fffd}after\n"));
        assert!(text.ends_with("（输出过长，已截断）"));
    }

    #[tokio::test]
    async fn agent_timeout_and_stop_drop_the_inflight_future() {
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
        }
        for timeout in [true, false] {
            let dropped = Arc::new(AtomicBool::new(false));
            let flag = dropped.clone();
            let future = async move {
                let _guard = Guard(flag);
                std::future::pending::<Result<(), String>>().await
            };
            if timeout {
                assert!(wait_for_agent(future, Some(0)).await.is_err());
            } else {
                let stop = Arc::new(AtomicBool::new(false));
                let signal = stop.clone();
                let cancel = async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    signal.store(true, Ordering::SeqCst);
                };
                let wait = async {
                    tokio::select! {
                        _ = future => panic!("pending prompt returned"),
                        _ = wait_for_stop(stop) => {},
                    }
                };
                tokio::join!(wait, cancel);
            }
            assert!(dropped.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn duplicate_edges_do_not_underflow_graph_indegrees() {
        let workflow: Workflow = serde_json::from_value(json!({
            "id":"w", "project_id":"p", "name":"w", "description":"", "enabled":true,
            "trigger_type":"manual", "schedule":null,
            "steps":[{"type":"start"},{"type":"note"}],
            "edges":[{"from":0,"to":1},{"from":0,"to":1}],
            "last_run_at":null,"next_run_at":null,"created_at":"","updated_at":""
        })).unwrap();
        assert_eq!(effective_edges(&workflow), (vec![(0, 1)], true));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn quoted_command_waits_preserves_env_and_exit_status() {
        let fixture = Fixture::new();
        let script = fixture.0.join("quoted script.cmd");
        std::fs::write(&script, "@echo off\r\necho %SHIDRIVE_TEST_VALUE%\r\necho problem 1>&2\r\nexit /b 7\r\n").unwrap();
        let mut command = shell_command("cmd", &format!("\"{}\"", script.display()), None);
        command.current_dir(&fixture.0).env("SHIDRIVE_TEST_VALUE", "captured value");
        let ctrl = control();
        let (ok, code, out, err) = execute_shell(command, ctrl.clone(), Some(10), |_| {}).await;
        assert!(!ok);
        assert_eq!(code, 7);
        assert!(out.contains("captured value"), "{out:?}");
        assert!(err.contains("problem"), "{err:?}");
        assert!(ctrl.shell_pids.lock().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn shell_deadline_applies_while_output_pipes_are_open() {
        let fixture = Fixture::new();
        let mut command = shell_command("powershell", "while ($true) { [Console]::WriteLine('alive'); Start-Sleep -Milliseconds 20 }", None);
        command.current_dir(&fixture.0);
        let ctrl = control();
        let result = tokio::time::timeout(Duration::from_secs(10), execute_shell(command, ctrl.clone(), Some(1), |_| {})).await.unwrap();
        assert!(!result.0);
        assert_eq!(result.1, -1);
        assert!(ctrl.shell_pids.lock().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn shell_without_deadline_returns_complete_output() {
        let fixture = Fixture::new();
        let mut command = shell_command("cmd", "echo hello & echo error 1>&2", None);
        command.current_dir(&fixture.0);
        let result = execute_shell(command, control(), None, |_| {}).await;
        assert!(result.0);
        assert_eq!(result.1, 0);
        assert!(result.2.contains("hello"));
        assert!(result.3.contains("error"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn quoted_command_preserves_arguments_and_redirection() {
        let fixture = Fixture::new();
        let script = fixture.0.join("with args.cmd");
        std::fs::write(&script, "@echo off\r\necho [%~1]\r\nexit /b 9\r\n").unwrap();
        let text = format!("\"{}\" \"two words\" > result.txt", script.display());
        let mut command = shell_command("cmd", &text, None);
        command.current_dir(&fixture.0);
        let result = execute_shell(command, control(), Some(10), |_| {}).await;
        assert_eq!(result.1, 9, "{result:?}");
        assert!(std::fs::read_to_string(fixture.0.join("result.txt")).unwrap().contains("[two words]"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn deadline_is_not_reset_when_pipes_close() {
        let fixture = Fixture::new();
        let mut command = shell_command("powershell", "[Console]::Out.Close(); [Console]::Error.Close(); Start-Sleep -Seconds 30", None);
        command.current_dir(&fixture.0);
        let result = tokio::time::timeout(Duration::from_secs(10), execute_shell(command, control(), Some(1), |_| {})).await.unwrap();
        assert!(!result.0);
        assert_eq!(result.1, -1);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn inherited_pipes_do_not_keep_shell_collectors_alive() {
        let fixture = Fixture::new();
        let script = "$child = Start-Process powershell -ArgumentList @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 15') -NoNewWindow -PassThru; [Console]::WriteLine($child.Id)";
        let mut command = shell_command("powershell", script, None);
        command.current_dir(&fixture.0);
        let started = std::time::Instant::now();
        let result = execute_shell(command, control(), None, |_| {}).await;
        // Clean up only the harmless child created by this fixture, even if the
        // elapsed assertion below detects a collector regression.
        if let Ok(pid) = result.2.trim().parse::<u32>() {
            tokio::task::spawn_blocking(move || kill_tree(pid)).await.unwrap();
        }
        assert!(result.0, "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(8), "inherited pipe blocked completion");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancellation_is_observed_without_external_taskkill() {
        let fixture = Fixture::new();
        let mut command = shell_command("powershell", "Start-Sleep -Seconds 30", None);
        command.current_dir(&fixture.0);
        let ctrl = control();
        let stop = ctrl.clone();
        let cancel = async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            stop.stop.store(true, Ordering::SeqCst);
        };
        let run = execute_shell(command, ctrl.clone(), None, |_| {});
        let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, cancel) }).await.unwrap();
        assert!(!result.0);
        assert!(ctrl.shell_pids.lock().unwrap().is_empty());
    }
}

