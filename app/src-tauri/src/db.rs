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
            // 列迁移紧跟建表执行（幂等）：全新库的基础 schema 是历史形态，
            // 若先跑种子数据（用到 env/edges 列）会直接失败 → 启动闪退
            let _ = c.execute_batch("ALTER TABLE workflows ADD COLUMN env TEXT NOT NULL DEFAULT '{}'");
            let _ = c.execute_batch("ALTER TABLE workflows ADD COLUMN edges TEXT NOT NULL DEFAULT '[]'");
            let _ = c.execute_batch("ALTER TABLE workflows ADD COLUMN sort INTEGER NOT NULL DEFAULT 0");
            let _ = c.execute_batch("ALTER TABLE agent_bindings ADD COLUMN status TEXT NOT NULL DEFAULT ''");
            let _ = c.execute_batch("ALTER TABLE agent_bindings ADD COLUMN model TEXT NOT NULL DEFAULT ''");
            // 外部编程接入（MCP）：项目级授权
            let _ = c.execute_batch(
                "CREATE TABLE IF NOT EXISTS remote_grants (                  id TEXT PRIMARY KEY,                  project_id TEXT NOT NULL,                  project_name TEXT NOT NULL DEFAULT '',                  project_root TEXT NOT NULL,                  context_id TEXT,                  context_name TEXT NOT NULL DEFAULT '',                  context_enabled INTEGER NOT NULL DEFAULT 0,                  fs_write INTEGER NOT NULL DEFAULT 0,                  exec_allowed INTEGER NOT NULL DEFAULT 0,                  token_hash TEXT NOT NULL UNIQUE,                  token_plain TEXT NOT NULL DEFAULT '',                  paused_at TEXT,                  created_at TEXT NOT NULL,                  revoked_at TEXT,                  last_used_at TEXT                )",
            );
            // 迁移：旧库补列（已存在时报 duplicate column，静默忽略）
            let _ = c.execute("ALTER TABLE remote_grants ADD COLUMN token_plain TEXT NOT NULL DEFAULT ''", []);
            let _ = c.execute("ALTER TABLE remote_grants ADD COLUMN paused_at TEXT", []);

            // OAuth 2.1（MCP 标准握手）：动态注册客户端 + 令牌（access/refresh 只存哈希）。
            // remote_oauth_tokens.grant_id='' 表示共享当前开放列表；非空值保留旧版单项目令牌。
            let _ = c.execute_batch(
                "CREATE TABLE IF NOT EXISTS remote_oauth_clients (                  client_id TEXT PRIMARY KEY,                  client_name TEXT NOT NULL DEFAULT '',                  redirect_uris TEXT NOT NULL DEFAULT '[]',                  created_at TEXT NOT NULL                )",
            );
            let _ = c.execute_batch(
                "CREATE TABLE IF NOT EXISTS remote_oauth_tokens (                  id TEXT PRIMARY KEY,                  client_id TEXT NOT NULL,                  grant_id TEXT NOT NULL,                  access_hash TEXT NOT NULL UNIQUE,                  refresh_hash TEXT NOT NULL UNIQUE,                  scopes TEXT NOT NULL DEFAULT '',                  created_at TEXT NOT NULL,                  access_expires_epoch INTEGER NOT NULL,                  refresh_expires_epoch INTEGER NOT NULL,                  last_used_at TEXT,                  revoked_at TEXT                )",
            );
            let _ = c.execute_batch("CREATE INDEX IF NOT EXISTS idx_oauth_tokens_grant ON remote_oauth_tokens(grant_id)");
            let _ = c.execute_batch("CREATE INDEX IF NOT EXISTS idx_remote_grants_project ON remote_grants(project_id)");

            // 会话对话本地持久化：key=ctxId:agent，整份条目 JSON，开聊天页秒开不再等适配器全量重放
            let _ = c.execute_batch(
                "CREATE TABLE IF NOT EXISTS chat_store (                key TEXT PRIMARY KEY,                items_json TEXT NOT NULL,                updated_at TEXT NOT NULL          )",
            );

            // 内置「无项目」常驻
            c.execute(
                "INSERT OR IGNORE INTO projects (id,name,root_path,description,sort,created_at,updated_at) VALUES (?1,?2,'','存放便捷工作流与本地服务，不绑定目录',-1,?3,?3)",
                params![crate::models::NO_PROJECT_ID, crate::models::NO_PROJECT_NAME, now()],
            )?;
            // MCP-10：内置「外部编程mcp」工作流已迁移到「设置 → 外部编程接入」独立服务；
            // 未被用户定制的内置副本直接移除，用户自行创建/改名的工作流保留
            {
                let _ = c.execute(
                    "DELETE FROM workflows WHERE id='rfw-0001' AND name IN ('runfromweb','外部编程mcp')",
                    [],
                );
            }
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
            // 每个工作流只保留最新 50 条运行历史
            c.execute(
                "DELETE FROM workflow_runs WHERE workflow_id=?1 AND id NOT IN (SELECT id FROM workflow_runs WHERE workflow_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT 50)",
                params![workflow_id],
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

    /// 读取一个会话的本地聊天记录（JSON 数组字符串）。
    pub fn chat_store_get(&self, key: &str) -> Result<Option<String>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT items_json FROM chat_store WHERE key=?1")?;
            let mut rows = st.query(params![key])?;
            if let Some(r) = rows.next()? {
                return Ok(Some(r.get::<_, String>(0)?));
            }
            Ok(None)
        })
    }

    /// 覆盖写入一个会话的本地聊天记录（每回合结束时整份快照，简单可靠）。
    pub fn chat_store_set(&self, key: &str, items_json: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO chat_store (key,items_json,updated_at) VALUES (?1,?2,?3)
                 ON CONFLICT(key) DO UPDATE SET items_json=?2, updated_at=?3",
                params![key, items_json, now()],
            )?;
            Ok(())
        })
    }

    /// 删除一个会话的本地聊天记录（新建会话时调用）。
    pub fn chat_store_delete(&self, key: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("DELETE FROM chat_store WHERE key=?1", params![key])?;
            Ok(())
        })
    }

/// 全量导出（按表 → 行对象数组）。
pub fn export_backup(&self) -> Result<serde_json::Value, String> {
    self.with(|c| {
        let mut out = serde_json::Map::new();
        for (table, cols) in BACKUP_TABLES {
            let sql = format!("SELECT {} FROM {table}", cols.join(","));
            let mut st = c.prepare(&sql)?;
            let names: Vec<String> = st.column_names().into_iter().map(|s| s.to_string()).collect();
            let mut rows = st.query([])?;
            let mut arr = Vec::new();
            while let Some(r) = rows.next()? {
                let mut obj = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let v: serde_json::Value = match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => serde_json::json!(n),
                        rusqlite::types::ValueRef::Real(f) => serde_json::json!(f),
                        rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => {
                            serde_json::json!(String::from_utf8_lossy(t).to_string())
                        }
                    };
                    obj.insert(name.clone(), v);
                }
                arr.push(serde_json::Value::Object(obj));
            }
            out.insert(table.to_string(), serde_json::Value::Array(arr));
        }
        out.insert("app".into(), serde_json::json!("shidrive"));
        out.insert("exported_at".into(), serde_json::json!(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()));
        Ok(serde_json::Value::Object(out))
    })
}

/// Merge a backup atomically. Updating a parent must never delete its children.
pub fn import_backup(&self, data: &serde_json::Value) -> Result<Vec<(String, usize)>, String> {
    let data = data.as_object().ok_or("backup must be an object")?;
    if data.get("app").and_then(|v| v.as_str()) != Some("shidrive") {
        return Err("backup must have app=shidrive".into());
    }
    if !BACKUP_TABLES.iter().any(|(table, _)| data.contains_key(*table)) {
        return Err("backup contains no supported tables".into());
    }
    let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut counts = Vec::new();
    for (table, cols) in BACKUP_TABLES {
        let Some(value) = data.get(*table) else { continue };
        let rows = value.as_array().ok_or_else(|| format!("{table} must be an array"))?;
        let mut ids = std::collections::HashSet::new();
        for (index, row) in rows.iter().enumerate() {
            let location = format!("{table}[{index}]");
            let row = row.as_object().ok_or_else(|| format!("{location} must be an object"))?;
            let id = row.get("id").and_then(|v| v.as_str()).filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("{location}.id must be a nonempty string"))?;
            if !ids.insert(id) {
                return Err(format!("{location}: duplicate id {id}"));
            }
            let mut names = Vec::new();
            let mut vals = Vec::new();
            for &col in *cols {
                // Old backups predate these columns. Omit them so an update preserves
                // the current value and an insert uses the schema default.
                if *table == "workflows" && matches!(col, "env" | "edges" | "sort") && !row.contains_key(col) {
                    continue;
                }
                let value = row.get(col).ok_or_else(|| format!("{location}.{col} is missing"))?;
                let value = if matches!(col, "sort" | "enabled") {
                    let n = value.as_i64().ok_or_else(|| format!("{location}.{col} must be an integer"))?;
                    if col == "enabled" && n != 0 && n != 1 {
                        return Err(format!("{location}.enabled must be 0 or 1"));
                    }
                    rusqlite::types::Value::Integer(n)
                } else if *table == "workflows" && col == "schedule" && value.is_null() {
                    // Historical schema permits NULL, but the workflow reader expects text.
                    rusqlite::types::Value::Text(String::new())
                } else if *table == "workflows" && matches!(col, "last_run_at" | "next_run_at") && value.is_null() {
                    rusqlite::types::Value::Null
                } else {
                    let text = value.as_str().ok_or_else(|| format!("{location}.{col} must be a string"))?;
                    if matches!(col, "project_id" | "context_id") && text.trim().is_empty() {
                        return Err(format!("{location}.{col} must not be empty"));
                    }
                    if *table == "workflows" {
                        validate_backup_workflow_json(col, text).map_err(|e| format!("{location}.{col}: {e}"))?;
                    }
                    rusqlite::types::Value::Text(text.to_string())
                };
                names.push(col);
                vals.push(value);
            }
            let placeholders: Vec<String> = (1..=names.len()).map(|i| format!("?{i}")).collect();
            let updates: Vec<String> = names.iter().filter(|&&c| c != "id")
                .map(|c| format!("{c}=excluded.{c}")).collect();
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
                names.join(","), placeholders.join(","), updates.join(",")
            );
            tx.execute(&sql, rusqlite::params_from_iter(vals.iter()))
                .map_err(|e| format!("{location}: {e}"))?;
            let parent = match *table {
                "contexts" | "workflows" => Some(("projects", "project_id")),
                "context_entries" => Some(("contexts", "context_id")),
                _ => None,
            };
            if let Some((parent_table, column)) = parent {
                let exists: bool = tx.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {parent_table} WHERE id=?1)"),
                    params![row[column].as_str().unwrap()], |r| r.get(0),
                ).map_err(|e| e.to_string())?;
                if !exists {
                    return Err(format!("{location}.{column} references a missing {parent_table} row"));
                }
            }
        }
        counts.push((table.to_string(), rows.len()));
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(counts)
}
}

fn validate_backup_workflow_json(column: &str, text: &str) -> Result<(), String> {
    let result = match column {
        "steps" => serde_json::from_str::<Vec<WorkflowStep>>(text).map(|_| ()),
        "env" => serde_json::from_str::<std::collections::BTreeMap<String, String>>(text).map(|_| ()),
        "edges" => serde_json::from_str::<Vec<Edge>>(text).map(|_| ()),
        "schedule" if !text.is_empty() => serde_json::from_str::<ScheduleConfig>(text).map(|_| ()),
        _ => return Ok(()),
    };
    result.map_err(|e| e.to_string())
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

// ---------- backup: export / import (projects + contexts + entries + workflows) ----------

const BACKUP_TABLES: &[(&str, &[&str])] = &[
    (
        "projects",
        &["id", "name", "root_path", "description", "sort", "created_at", "updated_at"],
    ),
    (
        "contexts",
        &["id", "project_id", "name", "overview", "constraints", "sort", "created_at", "updated_at"],
    ),
    (
        "context_entries",
        &["id", "context_id", "kind", "content", "status", "sort", "created_at", "updated_at"],
    ),
    (
        "workflows",
        &[
            "id", "project_id", "name", "description", "enabled", "trigger_type", "schedule",
            "steps", "last_run_at", "next_run_at", "created_at", "updated_at", "env", "edges", "sort",
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture_db() -> Db { Db::open(Path::new(":memory:")).unwrap() }
    /// MCP-10 后全新库不再内置工作流 seed，测试需要时自插一行最小工作流
    /// （备份导入校验 project 外键，故连同项目行一起建）。
    fn seed_test_workflow(db: &Db, id: &str) {
        db.with(|c| {
            c.execute(
                "INSERT INTO projects (id,name,root_path,description,sort,created_at,updated_at) VALUES ('p','p','',\"\",0,'now','now')",
                [],
            )
        })
        .unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO workflows (id,project_id,name,description,enabled,trigger_type,schedule,steps,last_run_at,next_run_at,created_at,updated_at,env,edges)
                 VALUES (?1,'p','t','',1,'manual','','[{\"type\":\"start\"}]',NULL,NULL,'now','now','{}','[]')",
                params![id],
            )
        })
        .unwrap();
    }
    fn project(id: &str, name: &str) -> serde_json::Value {
        json!({"id":id,"name":name,"root_path":"","description":"","sort":0,"created_at":"now","updated_at":"now"})
    }

    #[test]
    fn backup_merge_preserves_cascading_children() {
        let db = fixture_db();
        let p = db.create_project("original", "", "").unwrap();
        db.with(|c| c.execute_batch("CREATE TABLE cascade_child (project_id TEXT REFERENCES projects(id) ON DELETE CASCADE, value TEXT);")).unwrap();
        db.with(|c| c.execute("INSERT INTO cascade_child VALUES (?1,'keep')", params![p.id])).unwrap();
        db.import_backup(&json!({"app":"shidrive","projects":[project(&p.id,"updated")]})).unwrap();
        assert_eq!(db.list_projects().unwrap().iter().find(|x| x.id == p.id).unwrap().name, "updated");
        assert_eq!(db.with(|c| c.query_row("SELECT COUNT(*) FROM cascade_child", [], |r| r.get::<_, i64>(0))).unwrap(), 1);
    }

    #[test]
    fn backup_rolls_back_all_tables_on_validation_or_sql_failure() {
        let db = fixture_db();
        let original = db.export_backup().unwrap();
        db.with(|c| c.execute_batch("CREATE TRIGGER reject_project BEFORE INSERT ON projects WHEN NEW.id='reject' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;")).unwrap();
        let cases = [
            json!({"app":"shidrive","projects":[project("first","new"),project("reject","fail")]}),
            json!({"app":"shidrive","projects":[project("first","new")],"contexts":{}}),
            json!({"app":"shidrive","projects":[project("first","new")],"contexts":[{"id":"c","project_id":"missing","name":"c","overview":"","constraints":"","sort":0,"created_at":"now","updated_at":"now"}]}),
        ];
        for backup in cases {
            assert!(db.import_backup(&backup).is_err());
            let after = db.export_backup().unwrap();
            for (table, _) in BACKUP_TABLES { assert_eq!(original[*table], after[*table], "{table}"); }
        }
    }

    #[test]
    fn backup_rejects_invalid_shapes_types_and_duplicate_ids() {
        let db = fixture_db();
        let mut bad_sort = project("p", "p");
        bad_sort["sort"] = json!(1.5);
        for backup in [
            json!([]), json!({"app":"another","projects":[]}), json!({"app":"shidrive"}),
            json!({"app":"shidrive","projects":[null]}),
            json!({"app":"shidrive","projects":[project("", "p")]}),
            json!({"app":"shidrive","projects":[bad_sort]}),
            json!({"app":"shidrive","projects":[project("p","first"),project("p","second")]}),
        ] { assert!(db.import_backup(&backup).is_err(), "{backup}"); }
    }

    #[test]
    fn workflow_backup_roundtrip_and_legacy_columns() {
        let db = fixture_db();
        seed_test_workflow(&db, "wf-test");
        db.with(|c| c.execute("UPDATE workflows SET sort=42", [])).unwrap();
        let mut backup = db.export_backup().unwrap();
        assert_eq!(backup["workflows"][0]["sort"], 42);
        db.import_backup(&backup).unwrap();
        let row = backup["workflows"][0].as_object_mut().unwrap();
        row.remove("sort"); row.remove("env"); row.remove("edges");
        row.insert("schedule".into(), serde_json::Value::Null);
        db.import_backup(&backup).unwrap();
        assert!(db.get_workflow("wf-test").unwrap().is_some());
        assert_eq!(db.export_backup().unwrap()["workflows"][0]["sort"], 42);
        let fresh = fixture_db();
        fresh.with(|c| c.execute("DELETE FROM workflows", [])).unwrap();
        fresh.import_backup(&backup).unwrap();
        assert_eq!(fresh.export_backup().unwrap()["workflows"][0]["env"], "{}");
    }

    #[test]
    fn malformed_embedded_workflow_data_does_not_silently_empty_steps() {
        let db = fixture_db();
        seed_test_workflow(&db, "wf-test");
        for (column, value) in [("steps", "[{}]"), ("env", "[]"), ("edges", "[{}]"), ("schedule", "{}"), ("steps", "not json")] {
            let mut backup = db.export_backup().unwrap();
            backup["workflows"][0][column] = json!(value);
            assert!(db.import_backup(&backup).is_err(), "{column}");
            assert!(!db.get_workflow("wf-test").unwrap().unwrap().steps.is_empty());
        }
    }
}

