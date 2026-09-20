//! Workflow engine: graph execution (parallel branches, free-form edges), manual/scheduled.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDateTime, TimeZone, Weekday};
use futures::stream::{FuturesUnordered, StreamExt};
use std::future::Future;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::db::Db;
use crate::manager::{AgentManager, SessionTarget};
use crate::models::*;

pub const EVT_WF_LOG: &str = "wf://log";
pub const EVT_WF_STATUS: &str = "wf://status";

struct RunControl {
    stop: Arc<AtomicBool>,
    /// pids of running shell children (killed via taskkill /T)
    shell_pids: Mutex<Vec<u32>>,
    /// (context_id, agent_type) of running agent nodes
    agent_ctxs: Mutex<Vec<(String, String)>>,
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
    let raw: Vec<(usize, usize)> = w.edges.iter().map(|e| (e.from as usize, e.to as usize)).collect();
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
        let workflows = self.db.all_workflows()?;
        let now = Local::now();
        for w in workflows {
            if !w.enabled || w.trigger_type != "schedule" {
                continue;
            }
            let Some(sched) = w.schedule.clone() else { continue };
            match w.next_run_at.as_deref() {
                None => {
                    let next = compute_next(&sched, now);
                    self.db.update_workflow_times(&w.id, w.last_run_at.as_deref(), Some(&fmt_time(next)))?;
                }
                Some(t) => {
                    if let Some(due) = parse_time(t) {
                        if due <= now && !self.active.lock().unwrap().contains_key(&w.id) {
                            let next = compute_next(&sched, now);
                            self.db.update_workflow_times(&w.id, Some(&fmt_time(now)), Some(&fmt_time(next)))?;
                            self.spawn_run(&w, "schedule");
                        }
                    }
                }
            }
        }
        Ok(())
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
            for (ctx_id, agent_type) in ctrl.agent_ctxs.lock().unwrap().drain(..) {
                let agents = self.agents.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = agents.cancel(&ctx_id, &agent_type).await;
                });
            }
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
            agent_ctxs: Mutex::new(Vec::new()),
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
            if status == "success" && wf.trigger_type == "schedule" {
                if matches!(wf.schedule, Some(ScheduleConfig::Once { .. })) {
                    let mut w2 = wf.clone();
                    w2.enabled = false;
                    let _ = eng.db.update_workflow(&w2);
                }
            }
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
            if ctrl.stop.load(Ordering::SeqCst) {
                stopped = true;
            }
            let step_failed = !ok && !w.steps[idx].continue_on_error();
            if step_failed && failed_at.is_none() && !stopped {
                failed_at = Some(idx);
                ctrl.stop.store(true, Ordering::SeqCst);
                for pid in ctrl.shell_pids.lock().unwrap().drain(..) {
                    kill_tree(pid);
                }
                for (ctx_id, agent_type) in ctrl.agent_ctxs.lock().unwrap().drain(..) {
                    let agents = self.agents.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = agents.cancel(&ctx_id, &agent_type).await;
                    });
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
            WorkflowStep::Balloon { title, message, .. } => {
                let title = substitute(title, &snap);
                let message = substitute(message, &snap);
                self.show_balloon(run_id, &title, &message)
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

        let mut cmd = tokio::process::Command::new("cmd");
        let display: String;
        match shell_kind {
            "python" => {
                let py = python_path.unwrap_or("python");
                display = format!("[python] {command}");
                cmd = tokio::process::Command::new(py);
                cmd.arg("-c").arg(command);
            }
            "powershell" => {
                display = format!("[ps] {command}");
                cmd = tokio::process::Command::new("powershell");
                cmd.args(["-NoProfile", "-Command", command]);
            }
            _ => {
                let joined = command.replace("\r\n", " & ").replace('\n', " & ");
                let t = joined.trim().to_string();
                // 单个带引号程序（"C:\path\app.exe"）：cmd /C 传参时引号会被转义成命令名的一部分，
                // 直接 spawn 该程序（GUI 程序不等待退出）
                if t.len() > 2 && t.starts_with('"') && t.ends_with('"') && !t[1..t.len() - 1].contains('"') {
                    let exe = t[1..t.len() - 1].to_string();
                    self.log(run_id, &format!("▶ 直接启动 {exe}"));
                    let mut guicmd = tokio::process::Command::new(&exe);
                    guicmd.current_dir(&dir);
                    #[cfg(windows)]
                    guicmd.creation_flags(0x0800_0000);
                    match guicmd.spawn() {
                        Ok(_) => return (true, 0, String::new(), String::new()),
                        Err(e) => {
                            self.log(run_id, &format!("启动失败: {e}"));
                            return (false, -1, String::new(), String::new());
                        }
                    }
                }
                // 其余带引号路径的命令改写为 start "" 形式，规避 cmd 引号解析问题
                let effective = if t.starts_with('"') {
                    format!("start \"\" {}", t)
                } else {
                    joined.clone()
                };
                display = effective.clone();
                cmd.args(["/C", &effective]);
            }
        }
        self.log(run_id, &format!("$ {display}   （目录：{dir}）"));

        cmd.current_dir(&dir);
        // expose workflow vars to the child process (%VAR% in cmd, $env:VAR in ps, os.environ in python)
        cmd.envs(env);
        cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.log(run_id, &format!("启动失败: {e}"));
                return (false, -1, String::new(), String::new());
            }
        };
        let pid = child.id().unwrap_or(0);
        ctrl.shell_pids.lock().unwrap().push(pid);
        let out = child.stdout.take();
        let err = child.stderr.take();
        let out_buf = collect_pipe(out);
        let err_buf = collect_pipe(err);

        let wait = child.wait();
        let timeout = timeout_sec.unwrap_or(u64::MAX / 2);
        let outcome = tokio::time::timeout(Duration::from_secs(timeout), wait).await;

        let stdout_txt = out_buf.lock().unwrap().join("");
        let stderr_txt = err_buf.lock().unwrap().join("");
        ctrl.shell_pids.lock().unwrap().retain(|&p| p != pid);

        if ctrl.stop.load(Ordering::SeqCst) {
            kill_tree(pid);
            self.log(run_id, "命令已被终止");
            return (false, -1, stdout_txt, stderr_txt);
        }
        match outcome {
            Err(_) => {
                kill_tree(pid);
                self.log(run_id, &format!("命令超时（{timeout}s），已终止"));
                (false, -1, stdout_txt, stderr_txt)
            }
            Ok(Ok(status)) => {
                let code = status.code().unwrap_or(-1);
                if !stdout_txt.trim().is_empty() {
                    for line in stdout_txt.lines().take(200) {
                        self.log(run_id, line);
                    }
                }
                if !stderr_txt.trim().is_empty() {
                    for line in stderr_txt.lines().take(100) {
                        self.log(run_id, &format!("  ⚠ {line}"));
                    }
                }
                if code != 0 {
                    self.log(run_id, &format!("命令退出码 {code}"));
                    return (false, code, stdout_txt, stderr_txt);
                }
                (true, code, stdout_txt, stderr_txt)
            }
            Ok(Err(e)) => {
                self.log(run_id, &format!("等待命令失败: {e}"));
                (false, -1, stdout_txt, stderr_txt)
            }
        }
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
        ctrl.agent_ctxs.lock().unwrap().push((context_id.to_string(), agent_type.to_string()));
        let target_note = match &target {
            SessionTarget::Explicit(sid) => format!("（复用会话 {sid}）"),
            SessionTarget::Temp => "（临时会话）".to_string(),
            SessionTarget::Binding => "（绑定会话）".to_string(),
        };
        self.log(run_id, &format!("→ [{agent_type}]{target_note} {}", first_line(prompt)));

        let agents = self.agents.clone();
        let fut = agents.prompt_with(&ctx, agent_type, target, prompt, &[]);
        let result = match timeout_sec {
            Some(t) => match tokio::time::timeout(Duration::from_secs(t), fut).await {
                Ok(r) => r,
                Err(_) => {
                    self.log(run_id, &format!("Agent 节点超时（{t}s）"));
                    ctrl.agent_ctxs.lock().unwrap().retain(|(c, a)| !(c == context_id && a == agent_type));
                    return false;
                }
            },
            None => fut.await,
        };
        ctrl.agent_ctxs.lock().unwrap().retain(|(c, a)| !(c == context_id && a == agent_type));

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

    /// Windows 气泡提醒（系统托盘 balloon，与旧使驾同体验）
    fn show_balloon(&self, run_id: &str, title: &str, message: &str) -> bool {
        let script = format!(
            r#"$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $n=New-Object System.Windows.Forms.NotifyIcon; $n.Icon=[System.Drawing.SystemIcons]::Information; $n.Visible=$true; $n.ShowBalloonTip(5000,'{t}','{m}','Info'); Start-Sleep -Seconds 6; $n.Dispose()"#,
            t = title.replace('\'', "''"),
            m = message.replace('\'', "''"),
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

fn collect_pipe<R>(pipe: Option<R>) -> Arc<Mutex<Vec<String>>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let buf: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let Some(pipe) = pipe else { return buf };
    let sink = buf.clone();
    tauri::async_runtime::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            sink.lock().unwrap().push(line);
        }
    });
    buf
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

pub fn fmt_time(t: DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn parse_time(s: &str) -> Option<DateTime<Local>> {
    NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M"))
        .ok()
        .map(|n| Local.from_local_datetime(&n).single().unwrap_or_else(|| Local.from_utc_datetime(&n)))
}

pub fn compute_next(sched: &ScheduleConfig, from: DateTime<Local>) -> DateTime<Local> {
    match sched {
        ScheduleConfig::Interval { every_minutes } => from + ChronoDuration::minutes((*every_minutes).max(1)),
        ScheduleConfig::Daily { time } => {
            let (h, mi) = parse_hhmm(time);
            let today = from.date_naive().and_hms_opt(h, mi, 0).unwrap_or_default();
            let today = Local.from_local_datetime(&today).single().unwrap_or(from);
            if today > from {
                today
            } else {
                today + ChronoDuration::days(1)
            }
        }
        ScheduleConfig::Weekly { weekdays, time } => {
            let (h, mi) = parse_hhmm(time);
            for add in 0..8 {
                let day = from.date_naive() + ChronoDuration::days(add);
                let wd = num_weekday(day.weekday());
                if weekdays.contains(&wd) {
                    if let Some(t) = day.and_hms_opt(h, mi, 0) {
                        if let Some(t) = Local.from_local_datetime(&t).single() {
                            if t > from {
                                return t;
                            }
                        }
                    }
                }
            }
            from + ChronoDuration::weeks(1)
        }
        ScheduleConfig::Once { at } => parse_time(at).unwrap_or(from + ChronoDuration::minutes(1)),
    }
}

fn num_weekday(w: Weekday) -> u32 {
    match w {
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
        Weekday::Sun => 7,
    }
}

fn parse_hhmm(s: &str) -> (u32, u32) {
    let mut it = s.trim().split(':');
    let h = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let m = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (h.min(23), m.min(59))
}
