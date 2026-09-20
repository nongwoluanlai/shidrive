use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;
use std::sync::Mutex;

use crate::models::*;

pub struct Db {
    conn: Mutex<Connection>,
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;").map_err(|e| e.to_string())?;
        let db = Self { conn: Mutex::new(conn) };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), String> {
        let sql = r#"
        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            root_path TEXT NOT NULL DEFAULT '',
            description TEXT NOT NULL DEFAULT '',
            sort INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS contexts (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            name TEXT NOT NULL,
            overview TEXT NOT NULL DEFAULT '',
            constraints TEXT NOT NULL DEFAULT '',
            sort INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS context_entries (
            id TEXT PRIMARY KEY,
            context_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            content TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'open',
            sort INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS agent_bindings (
            id TEXT PRIMARY KEY,
            context_id TEXT NOT NULL,
            agent_type TEXT NOT NULL,
            session_id TEXT,
            title TEXT,
            workspace TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(context_id, agent_type)
        );
        CREATE TABLE IF NOT EXISTS chat_messages (
            id TEXT PRIMARY KEY,
            context_id TEXT NOT NULL,
            agent_type TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_chat_ctx ON chat_messages(context_id, agent_type, created_at);
        CREATE TABLE IF NOT EXISTS workflows (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            trigger_type TEXT NOT NULL DEFAULT 'manual',
            schedule TEXT,
            steps TEXT NOT NULL DEFAULT '[]',
            last_run_at TEXT,
            next_run_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS workflow_runs (
            id TEXT PRIMARY KEY,
            workflow_id TEXT NOT NULL,
            trigger TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'running',
            log TEXT NOT NULL DEFAULT '',
            started_at TEXT NOT NULL,
            finished_at TEXT
        );
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sc_commits (
            id TEXT PRIMARY KEY,
            context_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            agent_type TEXT NOT NULL DEFAULT '',
            session_id TEXT,
            summary TEXT NOT NULL DEFAULT '',
            files TEXT NOT NULL DEFAULT '[]',
            snapshot TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_sc_ctx ON sc_commits(context_id, seq);
        "#;
        self.with(|c| {
            c.execute_batch(sql)?;
            // 内置「无项目」常驻
            c.execute(
                "INSERT OR IGNORE INTO projects (id,name,root_path,description,sort,created_at,updated_at) VALUES (?1,?2,'','存放便捷工作流与本地服务，不绑定目录',-1,?3,?3)",
                params![crate::models::NO_PROJECT_ID, crate::models::NO_PROJECT_NAME, now()],
            )?;
            // 内置 runfromweb（Coding MCP）：单一 toggle 工作流（run=已启动则停止，否则启动）
            {
                // 清理旧内置（拆分为启/停两条的历史版本）
                let _ = c.execute("DELETE FROM workflows WHERE id IN ('rfw-start-0001','rfw-stop-0001')", []);
                let _ = c.execute(
                    "DELETE FROM workflows WHERE project_id=?1 AND name LIKE 'runfromweb%' AND id <> 'rfw-0001'",
                    params![crate::models::NO_PROJECT_ID],
                );
                let exist = c.query_row("SELECT COUNT(*) FROM workflows WHERE id='rfw-0001'", [], |r| r.get::<_, i64>(0))? > 0;
                if !exist {
                    let ts = now();
                    let toggle_ps = r#"$port='{{env.CODING_MCP_PORT}}'
$existing = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
if ($existing) { Stop-Process -Id $existing.OwningProcess -Force; "stopped pid $($existing.OwningProcess)"; exit }
$cli = @('--coding-mcp','--port',$port,'--root','{{env.CODING_MCP_ROOT}}')
if ('{{env.CODING_MCP_TOKEN}}') { $cli += @('--token','{{env.CODING_MCP_TOKEN}}') }
if ('{{env.CODING_MCP_BIND}}') { $cli += @('--bind','{{env.CODING_MCP_BIND}}') }
if ('{{env.CODING_MCP_AUTH_USER}}') { $cli += @('--auth-user','{{env.CODING_MCP_AUTH_USER}}','--auth-pass','{{env.CODING_MCP_AUTH_PASS}}') }
if ('{{env.CODING_MCP_ALLOW_EXEC}}' -eq '0') { $cli += @('--exec','0') }
Start-Process -FilePath '{{env.__app__}}' -ArgumentList $cli -WindowStyle Hidden
'started'"#;
                    let steps = vec![
                        WorkflowStep::Start { name: String::new(), x: 60.0, y: 140.0 },
                        WorkflowStep::Note {
                            name: "作用".into(),
                            text: "Coding MCP（runfromweb）：不随使驾启动的独立 MCP 服务，供网络侧模型（如 ChatGPT）通过 HTTP 使用本地文件与命令能力。\n再次运行本工作流 = 切换状态：已启动则停止，未启动则启动。\n端点 http://127.0.0.1:端口/mcp；对外暴露建议设置令牌或 HTTP 认证并自行配置反向代理。".into(),
                            x: 330.0, y: 60.0,
                        },
                        WorkflowStep::EnvSet {
                            name: "服务配置".into(),
                            vars: [
                                ("CODING_MCP_PORT", "51667"),
                                ("CODING_MCP_ROOT", "D:\\"),
                                ("CODING_MCP_TOKEN", ""),
                                ("CODING_MCP_BIND", ""),
                                ("CODING_MCP_AUTH_USER", ""),
                                ("CODING_MCP_AUTH_PASS", ""),
                                ("CODING_MCP_ALLOW_EXEC", "1"),
                            ].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                            labels: [
                                ("CODING_MCP_PORT", "服务端口"),
                                ("CODING_MCP_ROOT", "允许访问的根目录"),
                                ("CODING_MCP_TOKEN", "访问令牌(可空)"),
                                ("CODING_MCP_BIND", "绑定地址(可空=仅本机)"),
                                ("CODING_MCP_AUTH_USER", "HTTP认证用户名(可空)"),
                                ("CODING_MCP_AUTH_PASS", "HTTP认证密码"),
                                ("CODING_MCP_ALLOW_EXEC", "允许执行命令(1/0)"),
                            ].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                            x: 330.0, y: 250.0,
                        },
                        WorkflowStep::Shell {
                            name: "启动/停止(toggle)".into(),
                            command: toggle_ps.to_string(),
                            cwd: String::new(),
                            shell: "powershell".into(),
                            timeout_sec: Some(30),
                            continue_on_error: false,
                            x: 680.0, y: 140.0,
                        },
                    ];
                    let steps_json = serde_json::to_string(&steps).unwrap_or_default();
                    let edges_json = serde_json::to_string(&vec![
                        Edge { from: 0, to: 1 },
                        Edge { from: 1, to: 2 },
                        Edge { from: 2, to: 3 },
                    ]).unwrap_or_default();
                    c.execute(
                        "INSERT INTO workflows (id,project_id,name,description,enabled,trigger_type,schedule,steps,last_run_at,next_run_at,created_at,updated_at,env,edges) VALUES ('rfw-0001',?1,'runfromweb','Coding MCP 独立服务（内置）：运行=切换 启动/停止',1,'manual','',?2,NULL,NULL,?3,?3,'{}',?4)",
                        params![crate::models::NO_PROJECT_ID, steps_json, ts, edges_json],
                    )?;
                }
            }
            // migrations for existing databases
            let _ = c.execute_batch("ALTER TABLE workflows ADD COLUMN env TEXT NOT NULL DEFAULT '{}'");
            let _ = c.execute_batch("ALTER TABLE workflows ADD COLUMN edges TEXT NOT NULL DEFAULT '[]'");
            let _ = c.execute_batch("ALTER TABLE workflows ADD COLUMN sort INTEGER NOT NULL DEFAULT 0");
            let _ = c.execute_batch("ALTER TABLE agent_bindings ADD COLUMN status TEXT NOT NULL DEFAULT ''");
            let _ = c.execute_batch("ALTER TABLE agent_bindings ADD COLUMN model TEXT NOT NULL DEFAULT ''");
            // guard against duplicate seq from concurrent commits; ignore failure on legacy dup data
            let _ = c.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS idx_sc_ctx_seq ON sc_commits(context_id, seq)");

            Ok(())
        })
    }

    /// Run a closure with the connection; rusqlite errors are converted to String.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        f(&conn).map_err(|e| e.to_string())
    }

    // ---------- projects ----------

    pub fn list_projects(&self) -> Result<Vec<Project>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,name,root_path,description,sort,created_at,updated_at FROM projects ORDER BY sort, created_at")?;
            let rows = st.query_map([], row_project)?.collect::<rusqlite::Result<Vec<Project>>>()?;
            Ok(rows)
        })
    }

    pub fn create_project(&self, name: &str, root_path: &str, description: &str) -> Result<Project, String> {
        let p = Project {
            id: uuid(),
            name: name.to_string(),
            root_path: root_path.trim_end_matches(['\\', '/']).to_string(),
            description: description.to_string(),
            sort: 0,
            created_at: now(),
            updated_at: now(),
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO projects (id,name,root_path,description,sort,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![p.id, p.name, p.root_path, p.description, p.sort, p.created_at, p.updated_at],
            )?;
            Ok(())
        })?;
        Ok(p)
    }

    pub fn update_project(&self, id: &str, name: &str, root_path: &str, description: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE projects SET name=?2, root_path=?3, description=?4, updated_at=?5 WHERE id=?1",
                params![id, name, root_path.trim_end_matches(['\\', '/']), description, now()],
            )?;
            Ok(())
        })
    }

    pub fn delete_project(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM projects WHERE id=?1", params![id]).map(|_| ()))
    }

    // ---------- contexts ----------

    pub fn list_contexts(&self, project_id: &str) -> Result<Vec<Context>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,project_id,name,overview,constraints,sort,created_at,updated_at FROM contexts WHERE project_id=?1 ORDER BY sort, created_at")?;
            let rows = st.query_map(params![project_id], row_context)?.collect::<rusqlite::Result<Vec<Context>>>()?;
            Ok(rows)
        })
    }

    pub fn create_context(&self, project_id: &str, name: &str) -> Result<Context, String> {
        let ctx = Context {
            id: uuid(),
            project_id: project_id.to_string(),
            name: name.to_string(),
            overview: String::new(),
            constraints: String::new(),
            sort: 0,
            created_at: now(),
            updated_at: now(),
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO contexts (id,project_id,name,overview,constraints,sort,created_at,updated_at) VALUES (?1,?2,?3,'','',?4,?5,?6)",
                params![ctx.id, ctx.project_id, ctx.name, ctx.sort, ctx.created_at, ctx.updated_at],
            )?;
            Ok(())
        })?;
        Ok(ctx)
    }

    pub fn update_context(&self, id: &str, name: &str, overview: &str, constraints: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE contexts SET name=?2, overview=?3, constraints=?4, updated_at=?5 WHERE id=?1",
                params![id, name, overview, constraints, now()],
            )?;
            Ok(())
        })
    }

    pub fn delete_context(&self, id: &str) -> Result<(), String> {
        self.with(|c| {
            let tx = c.unchecked_transaction()?;
            tx.execute("DELETE FROM contexts WHERE id=?1", params![id])?;
            tx.execute("DELETE FROM context_entries WHERE context_id=?1", params![id])?;
            tx.execute("DELETE FROM agent_bindings WHERE context_id=?1", params![id])?;
            tx.execute("DELETE FROM chat_messages WHERE context_id=?1", params![id])?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_context_row(&self, id: &str) -> Option<Context> {
        self.with(|c| {
            let mut st = c
                .prepare("SELECT id,project_id,name,overview,constraints,sort,created_at,updated_at FROM contexts WHERE id=?1")?;
            st.query_row(params![id], row_context).optional()
        })
        .ok()
        .flatten()
    }

    // ---------- context entries ----------

    pub fn list_entries(&self, context_id: &str) -> Result<Vec<ContextEntry>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,context_id,kind,content,status,sort,created_at,updated_at FROM context_entries WHERE context_id=?1 ORDER BY kind, sort, rowid")?;
            let rows = st.query_map(params![context_id], row_entry)?.collect::<rusqlite::Result<Vec<ContextEntry>>>()?;
            Ok(rows)
        })
    }

    pub fn add_entry(&self, context_id: &str, kind: &str, content: &str) -> Result<ContextEntry, String> {
        let e = ContextEntry {
            id: uuid(),
            context_id: context_id.to_string(),
            kind: kind.to_string(),
            content: content.to_string(),
            status: "open".into(),
            sort: 0,
            created_at: now(),
            updated_at: now(),
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO context_entries (id,context_id,kind,content,status,sort,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![e.id, e.context_id, e.kind, e.content, e.status, e.sort, e.created_at, e.updated_at],
            )?;
            Ok(())
        })?;
        Ok(e)
    }

    pub fn update_entry(&self, id: &str, content: &str, status: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE context_entries SET content=?2, status=?3, updated_at=?4 WHERE id=?1",
                params![id, content, status, now()],
            )?;
            Ok(())
        })
    }

    pub fn delete_entry(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM context_entries WHERE id=?1", params![id]).map(|_| ()))
    }

    // ---------- bindings ----------

    pub fn get_binding(&self, context_id: &str, agent_type: &str) -> Result<Option<AgentBinding>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,context_id,agent_type,session_id,title,workspace,status,model,created_at,updated_at FROM agent_bindings WHERE context_id=?1 AND agent_type=?2")?;
            st.query_row(params![context_id, agent_type], row_binding).optional()
        })
    }

    pub fn set_binding_session(
        &self,
        context_id: &str,
        agent_type: &str,
        session_id: Option<&str>,
        title: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<AgentBinding, String> {
        let existing = self.get_binding(context_id, agent_type)?;
        let ts = now();
        match existing {
            Some(b) => {
                self.with(|c| {
                    c.execute(
                        "UPDATE agent_bindings SET session_id=?2, title=COALESCE(?3,title), workspace=COALESCE(?4,workspace), updated_at=?5 WHERE id=?1",
                        params![b.id, session_id, title, workspace, ts],
                    )?;
                    Ok(())
                })?;
            }
            None => {
                let b = AgentBinding {
                    id: uuid(),
                    context_id: context_id.to_string(),
                    agent_type: agent_type.to_string(),
                    session_id: session_id.map(|s| s.to_string()),
                    title: title.map(|s| s.to_string()),
                    workspace: workspace.map(|s| s.to_string()),
                    status: String::new(),
                    model: String::new(),
                    created_at: ts.clone(),
                    updated_at: ts,
                };
                self.with(|c| {
                    c.execute(
                        "INSERT INTO agent_bindings (id,context_id,agent_type,session_id,title,workspace,status,model,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,'','',?7,?8)",
                        params![b.id, b.context_id, b.agent_type, b.session_id, b.title, b.workspace, b.created_at, b.updated_at],
                    )?;
                    Ok(())
                })?;
            }
        }
        self.get_binding(context_id, agent_type)?.ok_or_else(|| "binding missing".into())
    }

    pub fn unbind(&self, context_id: &str, agent_type: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "DELETE FROM agent_bindings WHERE context_id=?1 AND agent_type=?2",
                params![context_id, agent_type],
            )?;
            Ok(())
        })
    }

    // ---------- workflows ----------

    pub fn list_workflows(&self, project_id: &str) -> Result<Vec<Workflow>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,project_id,name,description,enabled,trigger_type,schedule,steps,last_run_at,next_run_at,created_at,updated_at,env,edges FROM workflows WHERE project_id=?1 ORDER BY sort, created_at")?;
            let rows = st.query_map(params![project_id], row_workflow)?.collect::<rusqlite::Result<Vec<Workflow>>>()?;
            Ok(rows)
        })
    }

    pub fn all_workflows(&self) -> Result<Vec<Workflow>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,project_id,name,description,enabled,trigger_type,schedule,steps,last_run_at,next_run_at,created_at,updated_at,env,edges FROM workflows")?;
            let rows = st.query_map([], row_workflow)?.collect::<rusqlite::Result<Vec<Workflow>>>()?;
            Ok(rows)
        })
    }

    pub fn get_workflow(&self, id: &str) -> Result<Option<Workflow>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,project_id,name,description,enabled,trigger_type,schedule,steps,last_run_at,next_run_at,created_at,updated_at,env,edges FROM workflows WHERE id=?1")?;
            st.query_row(params![id], row_workflow).optional()
        })
    }

    pub fn create_workflow(
        &self,
        project_id: &str,
        name: &str,
        description: &str,
        enabled: bool,
        trigger_type: &str,
        schedule: Option<&ScheduleConfig>,
        steps: &[WorkflowStep],
        env: &std::collections::BTreeMap<String, String>,
        edges: &[Edge],
    ) -> Result<Workflow, String> {
        let w = Workflow {
            id: uuid(),
            project_id: project_id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            enabled,
            trigger_type: trigger_type.to_string(),
            schedule: schedule.cloned(),
            steps: steps.to_vec(),
            env: env.clone(),
            edges: edges.to_vec(),
            last_run_at: None,
            next_run_at: None,
            created_at: now(),
            updated_at: now(),
        };
        self.insert_workflow(&w)?;
        Ok(w)
    }

    pub fn insert_workflow(&self, w: &Workflow) -> Result<(), String> {
        let steps = serde_json::to_string(&w.steps).map_err(|e| e.to_string())?;
        let env_json = serde_json::to_string(&w.env).map_err(|e| e.to_string())?;
        let edges_json = serde_json::to_string(&w.edges).unwrap_or_default();
        let schedule = match &w.schedule {
            Some(s) => serde_json::to_string(s).map_err(|e| e.to_string())?,
            None => String::new(),
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO workflows (id,project_id,name,description,enabled,trigger_type,schedule,steps,last_run_at,next_run_at,created_at,updated_at,env,edges)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![w.id, w.project_id, w.name, w.description, w.enabled, w.trigger_type, schedule, steps, w.last_run_at, w.next_run_at, w.created_at, w.updated_at, env_json, edges_json],
            )?;
            Ok(())
        })
    }

    pub fn update_workflow(&self, w: &Workflow) -> Result<(), String> {
        let steps = serde_json::to_string(&w.steps).map_err(|e| e.to_string())?;
        let env_json = serde_json::to_string(&w.env).map_err(|e| e.to_string())?;
        let edges_json = serde_json::to_string(&w.edges).unwrap_or_default();
        let schedule = match &w.schedule {
            Some(s) => serde_json::to_string(s).map_err(|e| e.to_string())?,
            None => String::new(),
        };
        self.with(|c| {
            c.execute(
                "UPDATE workflows SET name=?2, description=?3, enabled=?4, trigger_type=?5, schedule=?6, steps=?7, last_run_at=?8, next_run_at=?9, updated_at=?10, env=?11, edges=?12 WHERE id=?1",
                params![w.id, w.name, w.description, w.enabled, w.trigger_type, schedule, steps, w.last_run_at, w.next_run_at, now(), env_json, edges_json],
            )?;
            Ok(())
        })
    }

    pub fn update_workflow_times(&self, id: &str, last_run_at: Option<&str>, next_run_at: Option<&str>) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE workflows SET last_run_at=?2, next_run_at=?3 WHERE id=?1",
                params![id, last_run_at, next_run_at],
            )?;
            Ok(())
        })
    }

    pub fn delete_workflow(&self, id: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("DELETE FROM workflows WHERE id=?1", params![id])?;
            c.execute("DELETE FROM workflow_runs WHERE workflow_id=?1", params![id])?;
            Ok(())
        })
    }

    // ---------- runs ----------

    pub fn create_run(&self, workflow_id: &str, trigger: &str) -> Result<String, String> {
        let id = uuid();
        self.with(|c| {
            c.execute(
                "INSERT INTO workflow_runs (id,workflow_id,trigger,status,log,started_at) VALUES (?1,?2,?3,'running','',?4)",
                params![id, workflow_id, trigger, now()],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    pub fn finish_run(&self, id: &str, status: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE workflow_runs SET status=?2, finished_at=?3 WHERE id=?1",
                params![id, status, now()],
            )?;
            Ok(())
        })
    }

    pub fn append_run_log(&self, id: &str, line: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("UPDATE workflow_runs SET log = log || ?2 WHERE id=?1", params![id, format!("{line}\n")])?;
            Ok(())
        })
    }

    pub fn list_runs(&self, workflow_id: &str, limit: i64) -> Result<Vec<WorkflowRun>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,workflow_id,trigger,status,log,started_at,finished_at FROM workflow_runs WHERE workflow_id=?1 ORDER BY started_at DESC LIMIT ?2")?;
            let rows = st.query_map(params![workflow_id, limit], row_run)?.collect::<rusqlite::Result<Vec<WorkflowRun>>>()?;
            Ok(rows)
        })
    }

    pub fn get_run(&self, id: &str) -> Result<Option<WorkflowRun>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,workflow_id,trigger,status,log,started_at,finished_at FROM workflow_runs WHERE id=?1")?;
            st.query_row(params![id], row_run).optional()
        })
    }

    /// Mark runs that were still "running" when the app exited.
    pub fn fail_stale_runs(&self) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE workflow_runs SET status='failed', finished_at=?1 WHERE status='running'",
                params![now()],
            )?;
            Ok(())
        })
    }

    // ---------- shared context commits ----------

    pub fn sc_head_seq(&self, context_id: &str) -> Result<i64, String> {
        self.with(|c| {
            c.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM sc_commits WHERE context_id=?1",
                params![context_id],
                |r| r.get(0),
            )
        })
    }

    /// Atomic git-style commit: validate base_version against head and insert in one
    /// transaction, so two concurrent agents can never both pass the check.
    /// Returns Err("__conflict__:<head>") when the base no longer matches.
    pub fn sc_commit_atomic(&self, commit: &ScCommit, base_version: i64) -> Result<(), String> {
        let head = self.with(|c| {
            let tx = c.unchecked_transaction()?;
            let head: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM sc_commits WHERE context_id=?1",
                params![commit.context_id],
                |r| r.get(0),
            )?;
            if head == base_version {
                tx.execute(
                    "INSERT INTO sc_commits (id,context_id,seq,agent_type,session_id,summary,files,snapshot,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![commit.id, commit.context_id, commit.seq, commit.agent_type, commit.session_id, commit.summary, commit.files, commit.snapshot, commit.created_at],
                )?;
            }
            tx.commit()?;
            Ok(head)
        })?;
        if head != base_version {
            return Err(format!("__conflict__:{head}"));
        }
        Ok(())
    }

    pub fn sc_list_commits(&self, context_id: &str, limit: i64) -> Result<Vec<ScCommit>, String> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id,context_id,seq,agent_type,session_id,summary,files,snapshot,created_at FROM sc_commits WHERE context_id=?1 ORDER BY seq DESC LIMIT ?2",
            )?;
            let rows = st
                .query_map(params![context_id, limit], row_sc_commit)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn sc_search(&self, context_id: &str, query: &str, limit: i64) -> Result<Vec<ScCommit>, String> {
        // escape LIKE wildcards so user keywords match literally
        let esc = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let like = format!("%{esc}%");
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id,context_id,seq,agent_type,session_id,summary,files,snapshot,created_at FROM sc_commits
                 WHERE context_id=?1 AND (summary LIKE ?2 ESCAPE '\\' OR files LIKE ?2 ESCAPE '\\' OR snapshot LIKE ?2 ESCAPE '\\')
                 ORDER BY seq DESC LIMIT ?3",
            )?;
            let rows = st
                .query_map(params![context_id, like, limit], row_sc_commit)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Replace every entry of one kind under a context (used by shared-context write-through).
    pub fn replace_entries(&self, context_id: &str, kind: &str, entries: &[ScEntry]) -> Result<(), String> {
        let ts = now();
        self.with(|c| {
            let tx = c.unchecked_transaction()?;
            tx.execute("DELETE FROM context_entries WHERE context_id=?1 AND kind=?2", params![context_id, kind])?;
            for e in entries {
                tx.execute(
                    "INSERT INTO context_entries (id,context_id,kind,content,status,sort,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,0,?6,?6)",
                    params![uuid(), context_id, kind, e.content, if e.status.is_empty() { "open" } else { &e.status }, ts],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn set_binding_status(&self, context_id: &str, agent_type: &str, status: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE agent_bindings SET status=?3, updated_at=?4 WHERE context_id=?1 AND agent_type=?2",
                params![context_id, agent_type, status, now()],
            )?;
            Ok(())
        })
    }

    pub fn set_binding_model(&self, context_id: &str, agent_type: &str, model: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE agent_bindings SET model=?3, updated_at=?4 WHERE context_id=?1 AND agent_type=?2",
                params![context_id, agent_type, model, now()],
            )?;
            Ok(())
        })
    }

    pub fn set_binding_title(&self, context_id: &str, agent_type: &str, title: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE agent_bindings SET title=?3, updated_at=?4 WHERE context_id=?1 AND agent_type=?2",
                params![context_id, agent_type, title, now()],
            )?;
            Ok(())
        })
    }

    /// Swap workflow sort order with its neighbour (dir: -1 up, +1 down).
    pub fn workflow_move(&self, id: &str, dir: i64) -> Result<(), String> {
        let moved = self.with(|c| {
            let tx = c.unchecked_transaction()?;
            let rows: Vec<(String, i64)> = {
                let mut st = tx.prepare("SELECT id, sort FROM workflows ORDER BY sort, created_at")?;
                let rows = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let mut moved = false;
            if let Some(pos) = rows.iter().position(|(rid, _)| rid == id) {
                let target = if dir < 0 { pos.checked_sub(1) } else { Some(pos + 1) };
                if let Some(t) = target {
                    if t < rows.len() {
                        let mut sorted = rows.clone();
                        sorted.swap(pos, t);
                        for (i, (rid, _)) in sorted.iter().enumerate() {
                            tx.execute("UPDATE workflows SET sort=?1 WHERE id=?2", params![i as i64, rid])?;
                        }
                        moved = true;
                    }
                }
            }
            tx.commit()?;
            Ok(moved)
        })?;
        if !moved {
            return Err("工作流不存在".into());
        }
        Ok(())
    }

    /// All bindings across projects, with context/project names for the session manager.
    pub fn bindings_list_all(&self) -> Result<Vec<BindingInfo>, String> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT b.id,b.context_id,b.agent_type,b.session_id,b.title,b.workspace,b.status,b.model,b.created_at,b.updated_at,
                        c.name, p.name
                 FROM agent_bindings b
                 JOIN contexts c ON c.id = b.context_id
                 JOIN projects p ON p.id = c.project_id
                 ORDER BY b.updated_at DESC",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(BindingInfo {
                        binding: row_binding(r)?,
                        context_name: r.get(10)?,
                        project_name: r.get(11)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// All contexts of all projects (for pickers), with project name.
    pub fn all_contexts(&self) -> Result<Vec<(Context, String)>, String> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT x.id,x.project_id,x.name,x.overview,x.constraints,x.sort,x.created_at,x.updated_at, p.name
                 FROM contexts x JOIN projects p ON p.id = x.project_id ORDER BY p.name, x.sort, x.created_at",
            )?;
            let rows = st
                .query_map([], |r| Ok((row_context(r)?, r.get::<_, String>(8)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    // ---------- settings ----------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT value FROM settings WHERE key=?1")?;
            st.query_row(params![key], |r| r.get::<_, String>(0)).optional()
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO settings (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
                params![key, value],
            )?;
            Ok(())
        })
    }
}

fn row_project(r: &Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        root_path: r.get(2)?,
        description: r.get(3)?,
        sort: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

fn row_context(r: &Row) -> rusqlite::Result<Context> {
    Ok(Context {
        id: r.get(0)?,
        project_id: r.get(1)?,
        name: r.get(2)?,
        overview: r.get(3)?,
        constraints: r.get(4)?,
        sort: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

fn row_entry(r: &Row) -> rusqlite::Result<ContextEntry> {
    Ok(ContextEntry {
        id: r.get(0)?,
        context_id: r.get(1)?,
        kind: r.get(2)?,
        content: r.get(3)?,
        status: r.get(4)?,
        sort: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

fn row_binding(r: &Row) -> rusqlite::Result<AgentBinding> {
    Ok(AgentBinding {
        id: r.get(0)?,
        context_id: r.get(1)?,
        agent_type: r.get(2)?,
        session_id: r.get(3)?,
        title: r.get(4)?,
        workspace: r.get(5)?,
        status: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        model: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn row_workflow(r: &Row) -> rusqlite::Result<Workflow> {
    let schedule: String = r.get(6)?;
    let steps: String = r.get(7)?;
    let env: String = r.get::<_, Option<String>>(12)?.unwrap_or_else(|| "{}".into());
    let edges_raw: String = r.get::<_, Option<String>>(13)?.unwrap_or_else(|| "[]".into());
    Ok(Workflow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
        trigger_type: r.get(5)?,
        schedule: if schedule.is_empty() { None } else { serde_json::from_str(&schedule).ok() },
        steps: serde_json::from_str(&steps).unwrap_or_default(),
        env: serde_json::from_str(&env).unwrap_or_default(),
        edges: serde_json::from_str(&edges_raw).unwrap_or_default(),
        last_run_at: r.get(8)?,
        next_run_at: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

fn row_sc_commit(r: &Row) -> rusqlite::Result<ScCommit> {
    Ok(ScCommit {
        id: r.get(0)?,
        context_id: r.get(1)?,
        seq: r.get(2)?,
        agent_type: r.get(3)?,
        session_id: r.get(4)?,
        summary: r.get(5)?,
        files: r.get(6)?,
        snapshot: r.get(7)?,
        created_at: r.get(8)?,
    })
}

fn row_run(r: &Row) -> rusqlite::Result<WorkflowRun> {
    Ok(WorkflowRun {
        id: r.get(0)?,
        workflow_id: r.get(1)?,
        trigger: r.get(2)?,
        status: r.get(3)?,
        log: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
    })
}
