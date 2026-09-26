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
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#;
        self.with(|c| {
            c.execute_batch(sql)?;
            // Child tables carry ON DELETE CASCADE foreign keys (see CHILD_TABLES); legacy
            // databases created without them are rebuilt below in upgrade_foreign_keys().
            for (table, body) in CHILD_TABLES {
                c.execute_batch(&format!("CREATE TABLE IF NOT EXISTS {table} ({body})"))?;
            }
            c.execute_batch(CHILD_INDEXES)?;
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
        })?;
        self.upgrade_foreign_keys()
    }

    /// One-time upgrade for databases created before child tables had foreign keys:
    /// purge orphan rows left behind by the old non-cascading deletes, then rebuild
    /// every child table with `ON DELETE CASCADE` (SQLite cannot ALTER a constraint in).
    /// Follows the documented 12-step ALTER TABLE procedure: foreign_keys must be OFF
    /// outside the transaction, and `foreign_key_check` must be clean before COMMIT.
    fn upgrade_foreign_keys(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let mut needs_rebuild = false;
        for (table, _) in CHILD_TABLES {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM pragma_foreign_key_list('{table}')"), [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            needs_rebuild |= n == 0;
        }
        if !needs_rebuild {
            return Ok(());
        }
        conn.execute_batch("PRAGMA foreign_keys=OFF").map_err(|e| e.to_string())?;
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.unchecked_transaction()?;
            let orphans = delete_orphans(&tx)?;
            if orphans > 0 {
                log::warn!("schema upgrade: removed {orphans} orphan rows left by non-cascading deletes");
            }
            for (table, body) in CHILD_TABLES {
                let cols = table_columns(&tx, table)?.join(",");
                tx.execute_batch(&format!(
                    "CREATE TABLE {table}__fk ({body});
                     INSERT INTO {table}__fk ({cols}) SELECT {cols} FROM {table};
                     DROP TABLE {table};
                     ALTER TABLE {table}__fk RENAME TO {table};"
                ))?;
            }
            tx.execute_batch(CHILD_INDEXES)?;
            tx.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS idx_sc_ctx_seq ON sc_commits(context_id, seq)")?;
            let violations: i64 = tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
            if violations > 0 {
                return Err(rusqlite::Error::InvalidQuery); // rolled back by drop below
            }
            tx.commit()
        })();
        conn.execute_batch("PRAGMA foreign_keys=ON").map_err(|e| e.to_string())?;
        match result {
            Ok(()) => {
                log::info!("schema upgrade: foreign keys added to child tables");
                Ok(())
            }
            Err(e) => {
                // Keep running on the legacy schema rather than refusing to start; the
                // explicit cascading deletes in this module do not depend on the FKs.
                log::error!("schema upgrade: foreign-key rebuild skipped: {e}");
                Ok(())
            }
        }
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

    /// Workflow ids of a project (so the engine can stop in-flight runs before deletion).
    pub fn project_workflow_ids(&self, project_id: &str) -> Result<Vec<String>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id FROM workflows WHERE project_id=?1")?;
            let ids = st.query_map(params![project_id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(ids)
        })
    }

    /// Delete a project and *everything* that hangs off it in one transaction:
    /// contexts (entries, bindings, chat history, shared-context commits, chat
    /// snapshots), workflows (+ run history) and the remote-coding authorizations
    /// of that directory. OAuth tokens that were bound to those authorizations are
    /// revoked so an external client cannot keep using the deleted project's root.
    pub fn delete_project(&self, id: &str) -> Result<ProjectCleanup, String> {
        self.with(|c| {
            let tx = c.unchecked_transaction()?;
            let context_ids: Vec<String> = {
                let mut st = tx.prepare("SELECT id FROM contexts WHERE project_id=?1")?;
                let ids = st.query_map(params![id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                ids
            };
            for ctx in &context_ids {
                delete_context_children(&tx, ctx)?;
            }
            let contexts = tx.execute("DELETE FROM contexts WHERE project_id=?1", params![id])?;
            tx.execute(
                "DELETE FROM workflow_runs WHERE workflow_id IN (SELECT id FROM workflows WHERE project_id=?1)",
                params![id],
            )?;
            let workflows = tx.execute("DELETE FROM workflows WHERE project_id=?1", params![id])?;
            let tokens_revoked = tx.execute(
                "UPDATE remote_oauth_tokens SET revoked_at=?2 WHERE revoked_at IS NULL
                 AND grant_id IN (SELECT id FROM remote_grants WHERE project_id=?1)",
                params![id, now()],
            )?;
            let grants = tx.execute("DELETE FROM remote_grants WHERE project_id=?1", params![id])?;
            tx.execute("DELETE FROM projects WHERE id=?1", params![id])?;
            tx.commit()?;
            Ok(ProjectCleanup { contexts, workflows, grants, tokens_revoked })
        })
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
            delete_context_children(&tx, id)?;
            tx.execute("DELETE FROM contexts WHERE id=?1", params![id])?;
            // A remote authorization that exposed this context keeps its directory
            // rights but must stop advertising a context that no longer exists.
            tx.execute(
                "UPDATE remote_grants SET context_id=NULL, context_name='', context_enabled=0 WHERE context_id=?1",
                params![id],
            )?;
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

    // Entry writes go through the *_committed variants below so every change
    // (UI or MCP) advances the shared-context version.

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

    /// Enabled, schedule-triggered workflows whose project still exists — the only
    /// rows the scheduler may ever consider.
    pub fn scheduled_workflows(&self) -> Result<Vec<Workflow>, String> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT w.id,w.project_id,w.name,w.description,w.enabled,w.trigger_type,w.schedule,w.steps,w.last_run_at,w.next_run_at,w.created_at,w.updated_at,w.env,w.edges
                 FROM workflows w JOIN projects p ON p.id = w.project_id
                 WHERE w.enabled=1 AND w.trigger_type='schedule'",
            )?;
            let rows = st.query_map([], row_workflow)?.collect::<rusqlite::Result<Vec<Workflow>>>()?;
            Ok(rows)
        })
    }

    pub fn get_workflow(&self, id: &str) -> Result<Option<Workflow>, String> {
        self.with(|c| get_workflow_tx(c, id))
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
        let schedule = schedule.cloned().map(crate::schedule::normalize);
        let armed = trigger_type == "schedule" && enabled;
        let next_run_at = validated_next_run(trigger_type, schedule.as_ref(), armed)?;
        let w = Workflow {
            id: uuid(),
            project_id: project_id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            enabled,
            trigger_type: trigger_type.to_string(),
            schedule,
            steps: steps.to_vec(),
            env: env.clone(),
            edges: edges.to_vec(),
            last_run_at: None,
            next_run_at,
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

    /// Save the user-editable fields of a workflow. The runtime fields carried by
    /// `w` (`last_run_at` / `next_run_at`) are **ignored** — they belong to the
    /// scheduler, and an editor holding a stale copy must not be able to roll them
    /// back. `next_run_at` is recomputed in the same transaction only when the
    /// trigger configuration (enabled / trigger_type / schedule) actually changed;
    /// a plain rename keeps the already-armed slot. Returns the stored row.
    pub fn update_workflow(&self, w: &Workflow) -> Result<Workflow, String> {
        let schedule_cfg = w.schedule.clone().map(crate::schedule::normalize);
        let steps = serde_json::to_string(&w.steps).map_err(|e| e.to_string())?;
        let env_json = serde_json::to_string(&w.env).map_err(|e| e.to_string())?;
        let edges_json = serde_json::to_string(&w.edges).unwrap_or_default();
        let schedule = match &schedule_cfg {
            Some(s) => serde_json::to_string(s).map_err(|e| e.to_string())?,
            None => String::new(),
        };
        if w.trigger_type == "schedule" {
            match &schedule_cfg {
                Some(s) => crate::schedule::validate(s)?,
                None => return Err("请配置定时规则".into()),
            }
        }
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let old = get_workflow_tx(&tx, &w.id).map_err(|e| e.to_string())?.ok_or("工作流不存在")?;
        let old_schedule = old.schedule.as_ref().map(|s| serde_json::to_string(s).unwrap_or_default()).unwrap_or_default();
        let trigger_changed = old.enabled != w.enabled || old.trigger_type != w.trigger_type || old_schedule != schedule;
        let armed = w.enabled && w.trigger_type == "schedule";
        let next_run_at = if trigger_changed {
            validated_next_run(&w.trigger_type, schedule_cfg.as_ref(), armed)?
        } else {
            old.next_run_at.clone()
        };
        tx.execute(
            "UPDATE workflows SET name=?2, description=?3, enabled=?4, trigger_type=?5, schedule=?6, steps=?7, next_run_at=?8, updated_at=?9, env=?10, edges=?11 WHERE id=?1",
            params![w.id, w.name, w.description, w.enabled, w.trigger_type, schedule, steps, next_run_at, now(), env_json, edges_json],
        )
        .map_err(|e| e.to_string())?;
        let stored = get_workflow_tx(&tx, &w.id).map_err(|e| e.to_string())?.ok_or("工作流不存在")?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(stored)
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

    pub fn set_workflow_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE workflows SET enabled=?2, next_run_at=CASE WHEN ?2 THEN next_run_at ELSE NULL END WHERE id=?1",
                params![id, enabled],
            )?;
            Ok(())
        })
    }

    /// Claim a due scheduled slot: record the run time, arm the following slot (or
    /// retire the workflow when `retire`), but only if the row still carries the
    /// `expected_next` the scheduler read. Returns false when an edit/delete raced
    /// with the tick, in which case the caller must not start the run.
    pub fn claim_scheduled_run(
        &self,
        id: &str,
        expected_next: Option<&str>,
        last_run_at: &str,
        next_run_at: Option<&str>,
        retire: bool,
    ) -> Result<bool, String> {
        self.with(|c| {
            let n = c.execute(
                "UPDATE workflows SET last_run_at=?2, next_run_at=?3, enabled=CASE WHEN ?4 THEN 0 ELSE enabled END
                 WHERE id=?1 AND enabled=1 AND trigger_type='schedule' AND next_run_at IS ?5",
                params![id, last_run_at, if retire { None } else { next_run_at }, retire, expected_next],
            )?;
            Ok(n == 1)
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

    /// Head version + live snapshot of a shared context, read in one transaction so
    /// the `version` handed to an agent always matches the sections it received.
    pub fn sc_snapshot(&self, context_id: &str) -> Result<(i64, ScSnapshot), String> {
        self.with(|c| {
            let tx = c.unchecked_transaction()?;
            let head = sc_head_tx(&tx, context_id)?;
            let snap = sc_snapshot_tx(&tx, context_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            tx.commit()?;
            Ok((head, snap))
        })
    }

    /// Run `op` against the live tables of a shared context and record a commit
    /// (snapshot of the resulting state, `seq = head + 1`) for it — all in ONE
    /// transaction. `base_version = Some(v)` turns it into a git-style CAS commit:
    /// when the head moved past `v` nothing is written and
    /// `Err("__conflict__:<head>")` is returned. `None` (UI edits) skips the check
    /// but still advances the version, so an agent holding an older base is told
    /// to re-read instead of silently replacing the user's edits.
    pub fn sc_commit_with<T>(
        &self,
        context_id: &str,
        base_version: Option<i64>,
        meta: &ScCommitMeta,
        op: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<(T, ScCommit), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let head = sc_head_tx(&tx, context_id).map_err(|e| e.to_string())?;
        if let Some(base) = base_version {
            if base != head {
                return Err(format!("__conflict__:{head}")); // tx dropped → rollback
            }
        }
        let out = op(&tx)?;
        let snap = sc_snapshot_tx(&tx, context_id).map_err(|e| e.to_string())?.ok_or("context missing")?;
        let commit = ScCommit {
            id: uuid(),
            context_id: context_id.to_string(),
            seq: head + 1,
            agent_type: meta.agent_type.clone(),
            session_id: meta.session_id.clone(),
            summary: meta.summary.clone(),
            files: serde_json::to_string(&meta.files).unwrap_or_else(|_| "[]".into()),
            snapshot: serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()),
            created_at: now(),
        };
        tx.execute(
            "INSERT INTO sc_commits (id,context_id,seq,agent_type,session_id,summary,files,snapshot,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![commit.id, commit.context_id, commit.seq, commit.agent_type, commit.session_id, commit.summary, commit.files, commit.snapshot, commit.created_at],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok((out, commit))
    }

    /// Apply an MCP `context_update` patch (overview / constraints / whole entry
    /// lists) as one CAS commit. Nothing is applied when the base version is stale
    /// or any write fails.
    pub fn sc_commit_patch(&self, context_id: &str, base_version: i64, patch: &ScPatch, meta: &ScCommitMeta) -> Result<ScCommit, String> {
        let ((), commit) = self.sc_commit_with(context_id, Some(base_version), meta, |tx| {
            if patch.overview.is_some() || patch.constraints.is_some() {
                tx.execute(
                    "UPDATE contexts SET overview=COALESCE(?2, overview), constraints=COALESCE(?3, constraints), updated_at=?4 WHERE id=?1",
                    params![context_id, patch.overview, patch.constraints, now()],
                )
                .map_err(|e| e.to_string())?;
            }
            for (kind, list) in [("todo", &patch.todos), ("progress", &patch.progress), ("note", &patch.notes)] {
                if let Some(entries) = list {
                    replace_entries_tx(tx, context_id, kind, entries).map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        })?;
        Ok(commit)
    }

    /// UI: add one entry and record it as a version of the shared context.
    pub fn add_entry_committed(&self, context_id: &str, kind: &str, content: &str) -> Result<(ContextEntry, ScCommit), String> {
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
        let meta = ScCommitMeta::ui(format!("界面编辑：新增{}「{}」", kind_label(kind), short(content)));
        let (entry, commit) = self.sc_commit_with(context_id, None, &meta, |tx| {
            tx.execute(
                "INSERT INTO context_entries (id,context_id,kind,content,status,sort,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![e.id, e.context_id, e.kind, e.content, e.status, e.sort, e.created_at, e.updated_at],
            )
            .map_err(|e| e.to_string())?;
            Ok(e.clone())
        })?;
        Ok((entry, commit))
    }

    /// UI: edit one entry (content / status). No-op — and no new version — when nothing changed.
    pub fn update_entry_committed(&self, id: &str, content: &str, status: &str) -> Result<Option<ScCommit>, String> {
        let Some(cur) = self.get_entry(id)? else { return Err("条目不存在".into()) };
        if cur.content == content && cur.status == status {
            return Ok(None);
        }
        let what = if cur.status != status { format!("状态 → {status}") } else { "内容".to_string() };
        let meta = ScCommitMeta::ui(format!("界面编辑：{}{}「{}」", kind_label(&cur.kind), what, short(content)));
        let ((), commit) = self.sc_commit_with(&cur.context_id, None, &meta, |tx| {
            tx.execute(
                "UPDATE context_entries SET content=?2, status=?3, updated_at=?4 WHERE id=?1",
                params![id, content, status, now()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })?;
        Ok(Some(commit))
    }

    /// UI: delete one entry as a version of the shared context.
    pub fn delete_entry_committed(&self, id: &str) -> Result<Option<ScCommit>, String> {
        let Some(cur) = self.get_entry(id)? else { return Ok(None) };
        let meta = ScCommitMeta::ui(format!("界面编辑：删除{}「{}」", kind_label(&cur.kind), short(&cur.content)));
        let ((), commit) = self.sc_commit_with(&cur.context_id, None, &meta, |tx| {
            tx.execute("DELETE FROM context_entries WHERE id=?1", params![id]).map_err(|e| e.to_string())?;
            Ok(())
        })?;
        Ok(Some(commit))
    }

    /// UI: rename / edit overview & constraints. Only overview/constraints changes
    /// produce a version (the name is not part of the shared snapshot).
    pub fn update_context_committed(&self, id: &str, name: &str, overview: &str, constraints: &str) -> Result<Option<ScCommit>, String> {
        let Some(cur) = self.get_context_row(id) else { return Err("上下文不存在".into()) };
        if cur.overview == overview && cur.constraints == constraints {
            self.update_context(id, name, overview, constraints)?;
            return Ok(None);
        }
        let mut what = Vec::new();
        if cur.overview != overview {
            what.push("概述");
        }
        if cur.constraints != constraints {
            what.push("约束");
        }
        let meta = ScCommitMeta::ui(format!("界面编辑：修改{}", what.join("与")));
        let ((), commit) = self.sc_commit_with(id, None, &meta, |tx| {
            tx.execute(
                "UPDATE contexts SET name=?2, overview=?3, constraints=?4, updated_at=?5 WHERE id=?1",
                params![id, name, overview, constraints, now()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })?;
        Ok(Some(commit))
    }

    pub fn get_entry(&self, id: &str) -> Result<Option<ContextEntry>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,context_id,kind,content,status,sort,created_at,updated_at FROM context_entries WHERE id=?1")?;
            st.query_row(params![id], row_entry).optional()
        })
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
            // Parent check first: with foreign keys enforced the INSERT itself would
            // otherwise fail with a bare "FOREIGN KEY constraint failed".
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
            let placeholders: Vec<String> = (1..=names.len()).map(|i| format!("?{i}")).collect();
            let updates: Vec<String> = names.iter().filter(|&&c| c != "id")
                .map(|c| format!("{c}=excluded.{c}")).collect();
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
                names.join(","), placeholders.join(","), updates.join(",")
            );
            tx.execute(&sql, rusqlite::params_from_iter(vals.iter()))
                .map_err(|e| format!("{location}: {e}"))?;
        }
        counts.push((table.to_string(), rows.len()));
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(counts)
}
}

// ---------- schema: child tables with cascading foreign keys ----------

/// Every table that hangs off `projects` / `contexts` / `workflows`. The bodies are
/// used both for fresh databases (CREATE TABLE IF NOT EXISTS) and for the one-time
/// rebuild of legacy databases (see `Db::upgrade_foreign_keys`). Order matters:
/// parents before children.
const CHILD_TABLES: &[(&str, &str)] = &[
    (
        "contexts",
        "id TEXT PRIMARY KEY,
         project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
         name TEXT NOT NULL,
         overview TEXT NOT NULL DEFAULT '',
         constraints TEXT NOT NULL DEFAULT '',
         sort INTEGER NOT NULL DEFAULT 0,
         created_at TEXT NOT NULL,
         updated_at TEXT NOT NULL",
    ),
    (
        "context_entries",
        "id TEXT PRIMARY KEY,
         context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
         kind TEXT NOT NULL,
         content TEXT NOT NULL DEFAULT '',
         status TEXT NOT NULL DEFAULT 'open',
         sort INTEGER NOT NULL DEFAULT 0,
         created_at TEXT NOT NULL,
         updated_at TEXT NOT NULL",
    ),
    (
        "agent_bindings",
        "id TEXT PRIMARY KEY,
         context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
         agent_type TEXT NOT NULL,
         session_id TEXT,
         title TEXT,
         workspace TEXT,
         created_at TEXT NOT NULL,
         updated_at TEXT NOT NULL,
         status TEXT NOT NULL DEFAULT '',
         model TEXT NOT NULL DEFAULT '',
         UNIQUE(context_id, agent_type)",
    ),
    (
        "chat_messages",
        "id TEXT PRIMARY KEY,
         context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
         agent_type TEXT NOT NULL,
         role TEXT NOT NULL,
         content TEXT NOT NULL,
         created_at TEXT NOT NULL",
    ),
    (
        "sc_commits",
        "id TEXT PRIMARY KEY,
         context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
         seq INTEGER NOT NULL,
         agent_type TEXT NOT NULL DEFAULT '',
         session_id TEXT,
         summary TEXT NOT NULL DEFAULT '',
         files TEXT NOT NULL DEFAULT '[]',
         snapshot TEXT NOT NULL DEFAULT '{}',
         created_at TEXT NOT NULL",
    ),
    (
        "workflows",
        "id TEXT PRIMARY KEY,
         project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
         name TEXT NOT NULL,
         description TEXT NOT NULL DEFAULT '',
         enabled INTEGER NOT NULL DEFAULT 1,
         trigger_type TEXT NOT NULL DEFAULT 'manual',
         schedule TEXT,
         steps TEXT NOT NULL DEFAULT '[]',
         last_run_at TEXT,
         next_run_at TEXT,
         created_at TEXT NOT NULL,
         updated_at TEXT NOT NULL,
         env TEXT NOT NULL DEFAULT '{}',
         edges TEXT NOT NULL DEFAULT '[]',
         sort INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "workflow_runs",
        "id TEXT PRIMARY KEY,
         workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
         trigger TEXT NOT NULL,
         status TEXT NOT NULL DEFAULT 'running',
         log TEXT NOT NULL DEFAULT '',
         started_at TEXT NOT NULL,
         finished_at TEXT",
    ),
];

const CHILD_INDEXES: &str = "
    CREATE INDEX IF NOT EXISTS idx_chat_ctx ON chat_messages(context_id, agent_type, created_at);
    CREATE INDEX IF NOT EXISTS idx_sc_ctx ON sc_commits(context_id, seq);
    CREATE INDEX IF NOT EXISTS idx_ctx_project ON contexts(project_id);
    CREATE INDEX IF NOT EXISTS idx_entries_ctx ON context_entries(context_id);
    CREATE INDEX IF NOT EXISTS idx_wf_project ON workflows(project_id);
    CREATE INDEX IF NOT EXISTS idx_runs_wf ON workflow_runs(workflow_id);
";

/// Column names of a table as stored (needed to copy legacy rows whose column order
/// differs from the canonical definition because of past ALTER TABLE ADD COLUMN).
fn table_columns(c: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
    let cols = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cols)
}

/// Remove rows whose parent no longer exists (left behind by the pre-FK deletes).
/// Also drops remote authorizations of vanished projects and chat snapshots of
/// vanished contexts, which are not FK-enforced. Returns the number of rows removed.
fn delete_orphans(c: &Connection) -> rusqlite::Result<usize> {
    let mut n = 0;
    n += c.execute("DELETE FROM contexts WHERE project_id NOT IN (SELECT id FROM projects)", [])?;
    n += c.execute("DELETE FROM workflows WHERE project_id NOT IN (SELECT id FROM projects)", [])?;
    n += c.execute("DELETE FROM workflow_runs WHERE workflow_id NOT IN (SELECT id FROM workflows)", [])?;
    n += c.execute("DELETE FROM context_entries WHERE context_id NOT IN (SELECT id FROM contexts)", [])?;
    n += c.execute("DELETE FROM agent_bindings WHERE context_id NOT IN (SELECT id FROM contexts)", [])?;
    n += c.execute("DELETE FROM chat_messages WHERE context_id NOT IN (SELECT id FROM contexts)", [])?;
    n += c.execute("DELETE FROM sc_commits WHERE context_id NOT IN (SELECT id FROM contexts)", [])?;
    n += c.execute(
        "UPDATE remote_oauth_tokens SET revoked_at=?1 WHERE revoked_at IS NULL AND grant_id <> ''
         AND grant_id IN (SELECT id FROM remote_grants WHERE project_id NOT IN (SELECT id FROM projects))",
        params![now()],
    )?;
    n += c.execute("DELETE FROM remote_grants WHERE project_id NOT IN (SELECT id FROM projects)", [])?;
    n += c.execute(
        "UPDATE remote_grants SET context_id=NULL, context_name='', context_enabled=0
         WHERE context_id IS NOT NULL AND context_id <> '' AND context_id NOT IN (SELECT id FROM contexts)",
        [],
    )?;
    // chat_store keys are `<context_id>:<agent>`; `-` is the no-context placeholder.
    n += c.execute(
        "DELETE FROM chat_store WHERE instr(key, ':') > 1
         AND substr(key, 1, instr(key, ':') - 1) <> '-'
         AND substr(key, 1, instr(key, ':') - 1) NOT IN (SELECT id FROM contexts)",
        [],
    )?;
    Ok(n)
}

/// Everything owned by a context except the context row itself. Explicit even
/// though the FKs cascade, because chat_store is keyed by string prefix.
fn delete_context_children(c: &Connection, context_id: &str) -> rusqlite::Result<()> {
    c.execute("DELETE FROM context_entries WHERE context_id=?1", params![context_id])?;
    c.execute("DELETE FROM agent_bindings WHERE context_id=?1", params![context_id])?;
    c.execute("DELETE FROM chat_messages WHERE context_id=?1", params![context_id])?;
    c.execute("DELETE FROM sc_commits WHERE context_id=?1", params![context_id])?;
    c.execute(
        "DELETE FROM chat_store WHERE substr(key, 1, length(?1) + 1) = ?1 || ':'",
        params![context_id],
    )?;
    Ok(())
}

/// Result of `Db::delete_project` (numbers are for logging / UI feedback).
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct ProjectCleanup {
    pub contexts: usize,
    pub workflows: usize,
    pub grants: usize,
    pub tokens_revoked: usize,
}

fn get_workflow_tx(c: &Connection, id: &str) -> rusqlite::Result<Option<Workflow>> {
    let mut st = c.prepare("SELECT id,project_id,name,description,enabled,trigger_type,schedule,steps,last_run_at,next_run_at,created_at,updated_at,env,edges FROM workflows WHERE id=?1")?;
    st.query_row(params![id], row_workflow).optional()
}

/// Validate the trigger configuration and, when `armed`, compute the first slot.
/// A schedule with no future occurrence (e.g. a one-shot in the past) is rejected
/// here so the user gets a clear error instead of a workflow that silently never
/// runs — or, as before, ran "one minute from now".
fn validated_next_run(trigger_type: &str, schedule: Option<&ScheduleConfig>, armed: bool) -> Result<Option<String>, String> {
    if trigger_type != "schedule" {
        return Ok(None);
    }
    let Some(s) = schedule else { return Err("请配置定时规则".into()) };
    crate::schedule::validate(s)?;
    if !armed {
        return Ok(None);
    }
    match crate::schedule::compute_next(s, chrono::Local::now()) {
        Some(t) => Ok(Some(crate::schedule::fmt_time(t))),
        None => Err("单次执行时间已过去，请重新选择一个未来的时间".into()),
    }
}

// ---------- shared context: transaction helpers ----------

/// Who / what produced a shared-context commit.
#[derive(Debug, Clone, Default)]
pub struct ScCommitMeta {
    pub agent_type: String,
    pub session_id: Option<String>,
    pub summary: String,
    pub files: Vec<String>,
}

impl ScCommitMeta {
    /// Commits produced by the desktop UI (no agent, no session).
    pub fn ui(summary: String) -> Self {
        Self { agent_type: "user".into(), summary, ..Default::default() }
    }
}

/// Sections an MCP `context_update` may replace (None = untouched).
#[derive(Debug, Clone, Default)]
pub struct ScPatch {
    pub overview: Option<String>,
    pub constraints: Option<String>,
    pub todos: Option<Vec<ScEntry>>,
    pub progress: Option<Vec<ScEntry>>,
    pub notes: Option<Vec<ScEntry>>,
}

fn sc_head_tx(c: &Connection, context_id: &str) -> rusqlite::Result<i64> {
    c.query_row("SELECT COALESCE(MAX(seq), 0) FROM sc_commits WHERE context_id=?1", params![context_id], |r| r.get(0))
}

fn sc_snapshot_tx(c: &Connection, context_id: &str) -> rusqlite::Result<Option<ScSnapshot>> {
    let ctx = {
        let mut st = c.prepare("SELECT id,project_id,name,overview,constraints,sort,created_at,updated_at FROM contexts WHERE id=?1")?;
        st.query_row(params![context_id], row_context).optional()?
    };
    let Some(ctx) = ctx else { return Ok(None) };
    let mut snap = ScSnapshot { overview: ctx.overview, constraints: ctx.constraints, ..Default::default() };
    let mut st = c.prepare("SELECT kind,content,status FROM context_entries WHERE context_id=?1 ORDER BY kind, sort, rowid")?;
    let rows = st.query_map(params![context_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    for row in rows {
        let (kind, content, status) = row?;
        let item = ScEntry { content, status };
        match kind.as_str() {
            "todo" => snap.todos.push(item),
            "progress" => snap.progress.push(item),
            "note" | "decision" => snap.notes.push(item),
            _ => {}
        }
    }
    Ok(Some(snap))
}

/// Replace every entry of one kind under a context (shared-context write-through).
fn replace_entries_tx(c: &Connection, context_id: &str, kind: &str, entries: &[ScEntry]) -> rusqlite::Result<()> {
    let ts = now();
    c.execute("DELETE FROM context_entries WHERE context_id=?1 AND kind=?2", params![context_id, kind])?;
    for e in entries {
        c.execute(
            "INSERT INTO context_entries (id,context_id,kind,content,status,sort,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,0,?6,?6)",
            params![uuid(), context_id, kind, e.content, if e.status.is_empty() { "open" } else { &e.status }, ts],
        )?;
    }
    Ok(())
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "todo" => "待办",
        "progress" => "进展",
        "decision" => "决策",
        "note" => "备注",
        _ => "条目",
    }
}

fn short(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > 40 {
        let mut cut: String = line.chars().take(40).collect();
        cut.push('…');
        cut
    } else {
        line.to_string()
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

    fn count(db: &Db, sql: &str) -> i64 {
        db.with(|c| c.query_row(sql, [], |r| r.get::<_, i64>(0))).unwrap()
    }

    /// Populate one project with everything that can hang off it.
    fn populate_project(db: &Db) -> (Project, Context, Workflow) {
        let p = db.create_project("victim", "/tmp/victim", "").unwrap();
        let ctx = db.create_context(&p.id, "ctx").unwrap();
        db.add_entry_committed(&ctx.id, "todo", "A").unwrap();
        db.set_binding_session(&ctx.id, "codex", Some("sess"), None, None).unwrap();
        db.chat_store_set(&format!("{}:codex", ctx.id), "[]").unwrap();
        db.sc_commit_with(&ctx.id, None, &ScCommitMeta::ui("seed".into()), |_| Ok(())).unwrap();
        let w = db
            .create_workflow(&p.id, "cron", "", true, "schedule", Some(&ScheduleConfig::Interval { every_minutes: 1 }), &[], &Default::default(), &[])
            .unwrap();
        db.create_run(&w.id, "manual").unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO remote_grants (id,project_id,project_name,project_root,context_id,context_name,context_enabled,fs_write,exec_allowed,token_hash,created_at)
                 VALUES ('g1',?1,'victim','/tmp/victim',?2,'ctx',1,1,1,'hash1','now')",
                params![p.id, ctx.id],
            )?;
            c.execute(
                "INSERT INTO remote_oauth_tokens (id,client_id,grant_id,access_hash,refresh_hash,scopes,created_at,access_expires_epoch,refresh_expires_epoch)
                 VALUES ('t-legacy','cli','g1','ah1','rh1','fs:read','now',9999999999,9999999999),
                        ('t-shared','cli','','ah2','rh2','fs:read','now',9999999999,9999999999)",
                [],
            )
        })
        .unwrap();
        (p, ctx, w)
    }

    #[test]
    fn deleting_a_project_removes_everything_it_owns_and_revokes_its_remote_access() {
        let db = fixture_db();
        let (p, ctx, w) = populate_project(&db);
        // an unrelated project must be untouched
        let other = db.create_project("other", "", "").unwrap();
        let other_ctx = db.create_context(&other.id, "keep").unwrap();
        db.add_entry_committed(&other_ctx.id, "todo", "keep").unwrap();
        db.create_workflow(&other.id, "keep", "", true, "manual", None, &[], &Default::default(), &[]).unwrap();

        let cleanup = db.delete_project(&p.id).unwrap();
        assert_eq!((cleanup.contexts, cleanup.workflows, cleanup.grants, cleanup.tokens_revoked), (1, 1, 1, 1));

        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM projects WHERE id='{}'", p.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM contexts WHERE id='{}'", ctx.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM context_entries WHERE context_id='{}'", ctx.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM agent_bindings WHERE context_id='{}'", ctx.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM sc_commits WHERE context_id='{}'", ctx.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM chat_store WHERE key LIKE '{}:%'", ctx.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM workflows WHERE id='{}'", w.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM workflow_runs WHERE workflow_id='{}'", w.id)), 0);
        // nothing left for the scheduler, no open authorization, legacy token revoked, shared token intact
        assert!(db.scheduled_workflows().unwrap().iter().all(|x| x.project_id != p.id));
        assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_grants"), 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_oauth_tokens WHERE id='t-legacy' AND revoked_at IS NOT NULL"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_oauth_tokens WHERE id='t-shared' AND revoked_at IS NULL"), 1);
        // the other project is intact
        assert_eq!(db.list_contexts(&other.id).unwrap().len(), 1);
        assert_eq!(db.list_entries(&other_ctx.id).unwrap().len(), 1);
        assert_eq!(db.list_workflows(&other.id).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_context_removes_commits_chat_snapshots_and_grant_context_link() {
        let db = fixture_db();
        let (_p, ctx, _w) = populate_project(&db);
        db.delete_context(&ctx.id).unwrap();
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM sc_commits WHERE context_id='{}'", ctx.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM chat_store WHERE key LIKE '{}:%'", ctx.id)), 0);
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM context_entries WHERE context_id='{}'", ctx.id)), 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_grants WHERE id='g1' AND context_id IS NULL AND context_enabled=0"), 1);
    }

    #[test]
    fn foreign_keys_reject_children_of_missing_parents() {
        // Closes the tick/delete race: a run can no longer be recorded for a
        // workflow (or a context under a project) that was just deleted.
        let db = fixture_db();
        assert!(db.create_run("no-such-workflow", "schedule").is_err());
        assert!(db.create_context("no-such-project", "x").is_err());
        assert!(db.add_entry_committed("no-such-context", "todo", "x").is_err());
    }

    #[test]
    fn legacy_database_is_upgraded_with_foreign_keys_and_orphans_removed() {
        // Build a pre-FK database by hand (the historical schema), including the
        // orphan rows the old non-cascading deletes left behind, then open it.
        let dir = std::env::temp_dir().join(format!("shidrive-legacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, root_path TEXT NOT NULL DEFAULT '', description TEXT NOT NULL DEFAULT '', sort INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE contexts (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, overview TEXT NOT NULL DEFAULT '', constraints TEXT NOT NULL DEFAULT '', sort INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE context_entries (id TEXT PRIMARY KEY, context_id TEXT NOT NULL, kind TEXT NOT NULL, content TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'open', sort INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE agent_bindings (id TEXT PRIMARY KEY, context_id TEXT NOT NULL, agent_type TEXT NOT NULL, session_id TEXT, title TEXT, workspace TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, UNIQUE(context_id, agent_type));
                 CREATE TABLE chat_messages (id TEXT PRIMARY KEY, context_id TEXT NOT NULL, agent_type TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL);
                 CREATE TABLE workflows (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', enabled INTEGER NOT NULL DEFAULT 1, trigger_type TEXT NOT NULL DEFAULT 'manual', schedule TEXT, steps TEXT NOT NULL DEFAULT '[]', last_run_at TEXT, next_run_at TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE workflow_runs (id TEXT PRIMARY KEY, workflow_id TEXT NOT NULL, trigger TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'running', log TEXT NOT NULL DEFAULT '', started_at TEXT NOT NULL, finished_at TEXT);
                 CREATE TABLE sc_commits (id TEXT PRIMARY KEY, context_id TEXT NOT NULL, seq INTEGER NOT NULL, agent_type TEXT NOT NULL DEFAULT '', session_id TEXT, summary TEXT NOT NULL DEFAULT '', files TEXT NOT NULL DEFAULT '[]', snapshot TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL);
                 CREATE TABLE remote_grants (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, project_name TEXT NOT NULL DEFAULT '', project_root TEXT NOT NULL, context_id TEXT, context_name TEXT NOT NULL DEFAULT '', context_enabled INTEGER NOT NULL DEFAULT 0, fs_write INTEGER NOT NULL DEFAULT 0, exec_allowed INTEGER NOT NULL DEFAULT 0, token_hash TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL, revoked_at TEXT, last_used_at TEXT);
                 CREATE TABLE chat_store (key TEXT PRIMARY KEY, items_json TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO projects VALUES ('p-live','live','','',0,'t','t');
                 INSERT INTO contexts VALUES ('c-live','p-live','c','','',0,'t','t'), ('c-orphan','p-gone','c','','',0,'t','t');
                 INSERT INTO context_entries VALUES ('e-live','c-live','todo','x','open',0,'t','t'), ('e-orphan','c-orphan','todo','x','open',0,'t','t');
                 INSERT INTO agent_bindings VALUES ('b-orphan','c-orphan','codex',NULL,NULL,NULL,'t','t');
                 INSERT INTO sc_commits VALUES ('s-live','c-live',1,'a',NULL,'','[]','{}','t'), ('s-orphan','c-orphan',1,'a',NULL,'','[]','{}','t');
                 INSERT INTO workflows VALUES ('w-live','p-live','w','',1,'schedule','{\"kind\":\"interval\",\"every_minutes\":5}','[]',NULL,'2020-01-01 00:00:00','t','t'),
                                              ('w-orphan','p-gone','w','',1,'schedule','{\"kind\":\"interval\",\"every_minutes\":5}','[]',NULL,'2020-01-01 00:00:00','t','t');
                 INSERT INTO workflow_runs VALUES ('r-live','w-live','manual','success','','t','t'), ('r-orphan','w-orphan','manual','success','','t','t');
                 INSERT INTO remote_grants (id,project_id,project_name,project_root,context_id,context_name,context_enabled,fs_write,exec_allowed,token_hash,created_at)
                     VALUES ('g-live','p-live','live','/x','c-orphan','gone',1,1,1,'h1','t'), ('g-orphan','p-gone','gone','/y',NULL,'',0,1,1,'h2','t');
                 INSERT INTO chat_store VALUES ('c-live:codex','[]','t'), ('c-orphan:codex','[]','t');",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        // live data survived, orphans are gone
        assert_eq!(count(&db, "SELECT COUNT(*) FROM contexts"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM context_entries"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM agent_bindings"), 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM sc_commits"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM workflows"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM workflow_runs"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_grants"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_grants WHERE id='g-live' AND context_id IS NULL AND context_enabled=0"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM chat_store"), 1);
        // the orphan workflow can no longer be scheduled
        assert_eq!(db.scheduled_workflows().unwrap().iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), vec!["w-live"]);
        // foreign keys are now in place (with the added columns) and enforced
        for (table, _) in CHILD_TABLES {
            assert!(count(&db, &format!("SELECT COUNT(*) FROM pragma_foreign_key_list('{table}')")) > 0, "{table}");
        }
        assert!(db.get_binding("c-live", "codex").is_ok());
        assert!(db.create_run("w-orphan", "schedule").is_err());
        // reopening is a no-op
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM workflows"), 1);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn editing_a_workflow_never_writes_back_stale_runtime_fields_and_rearms_only_on_trigger_change() {
        let db = fixture_db();
        let p = db.create_project("p", "", "").unwrap();
        let w = db
            .create_workflow(&p.id, "w", "", true, "schedule", Some(&ScheduleConfig::Interval { every_minutes: 60 }), &[], &Default::default(), &[])
            .unwrap();
        let armed = w.next_run_at.clone().expect("creating an enabled schedule arms it");
        // the scheduler moved the runtime fields on
        db.update_workflow_times(&w.id, Some("2026-01-01 10:00:00"), Some("2026-01-01 11:00:00")).unwrap();
        // an editor holding the stale copy renames the workflow
        let mut stale = w.clone();
        stale.name = "renamed".into();
        let stored = db.update_workflow(&stale).unwrap();
        assert_eq!(stored.name, "renamed");
        assert_eq!(stored.last_run_at.as_deref(), Some("2026-01-01 10:00:00"), "stale last_run_at must not win");
        assert_eq!(stored.next_run_at.as_deref(), Some("2026-01-01 11:00:00"), "a rename keeps the armed slot");
        assert_ne!(stored.next_run_at.as_deref(), Some(armed.as_str()));
        // changing the schedule re-arms immediately (no waiting for the next tick)
        let mut edited = stored.clone();
        edited.schedule = Some(ScheduleConfig::Once { at: "2099-09-27T09:30".into() });
        let stored = db.update_workflow(&edited).unwrap();
        assert_eq!(stored.schedule, Some(ScheduleConfig::Once { at: "2099-09-27 09:30".into() }), "datetime-local input is normalised");
        assert_eq!(stored.next_run_at.as_deref(), Some("2099-09-27 09:30:00"));
        // switching to manual / disabling clears the slot
        let mut off = stored.clone();
        off.enabled = false;
        assert_eq!(db.update_workflow(&off).unwrap().next_run_at, None);
        // invalid / past one-shot times are rejected at save time instead of "in one minute"
        let mut bad = stored.clone();
        bad.schedule = Some(ScheduleConfig::Once { at: "".into() });
        assert!(db.update_workflow(&bad).unwrap_err().contains("单次执行时间"));
        bad.schedule = Some(ScheduleConfig::Once { at: "2000-01-01 00:00".into() });
        assert!(db.update_workflow(&bad).unwrap_err().contains("已过去"));
        bad.schedule = None;
        assert!(db.update_workflow(&bad).is_err());
        assert!(db
            .create_workflow(&p.id, "x", "", true, "schedule", Some(&ScheduleConfig::Once { at: "not a time".into() }), &[], &Default::default(), &[])
            .is_err());
    }

    #[test]
    fn a_due_one_shot_is_claimed_once_and_retired_regardless_of_run_outcome() {
        let db = fixture_db();
        let p = db.create_project("p", "", "").unwrap();
        let w = db
            .create_workflow(&p.id, "once", "", true, "schedule", Some(&ScheduleConfig::Once { at: "2099-01-01 00:00".into() }), &[], &Default::default(), &[])
            .unwrap();
        let expected = w.next_run_at.clone();
        // first tick claims the slot and retires the workflow (before the run even starts)
        assert!(db.claim_scheduled_run(&w.id, expected.as_deref(), "2099-01-01 00:00:05", None, true).unwrap());
        let after = db.get_workflow(&w.id).unwrap().unwrap();
        assert!(!after.enabled);
        assert_eq!(after.next_run_at, None);
        assert_eq!(after.last_run_at.as_deref(), Some("2099-01-01 00:00:05"));
        // a second tick (or a failed/stopped run) cannot claim it again
        assert!(!db.claim_scheduled_run(&w.id, expected.as_deref(), "2099-01-01 00:00:20", None, true).unwrap());
        assert!(db.scheduled_workflows().unwrap().iter().all(|x| x.id != w.id));
        // a recurring schedule whose row was edited between read and claim is skipped
        let w2 = db
            .create_workflow(&p.id, "cron", "", true, "schedule", Some(&ScheduleConfig::Interval { every_minutes: 1 }), &[], &Default::default(), &[])
            .unwrap();
        assert!(!db.claim_scheduled_run(&w2.id, Some("1999-01-01 00:00:00"), "now", Some("later"), false).unwrap());
        assert!(db.claim_scheduled_run(&w2.id, w2.next_run_at.as_deref(), "now", Some("later"), false).unwrap());
        let after = db.get_workflow(&w2.id).unwrap().unwrap();
        assert!(after.enabled);
        assert_eq!(after.next_run_at.as_deref(), Some("later"));
    }

    #[test]
    fn shared_context_commit_is_atomic_and_ui_edits_advance_the_version() {
        let db = fixture_db();
        let p = db.create_project("p", "", "").unwrap();
        let ctx = db.create_context(&p.id, "ctx").unwrap();
        let entry = |c: &str| ScEntry { content: c.into(), status: "open".into() };
        let meta = |s: &str| ScCommitMeta { agent_type: "codex".into(), summary: s.into(), ..Default::default() };

        // agent reads v0 and sees todo A
        let c1 = db.sc_commit_patch(&ctx.id, 0, &ScPatch { todos: Some(vec![entry("A")]), ..Default::default() }, &meta("add A")).unwrap();
        assert_eq!(c1.seq, 1);
        let (head, snap) = db.sc_snapshot(&ctx.id).unwrap();
        assert_eq!((head, snap.todos.len()), (1, 1));

        // user adds B in the UI → this is a version too
        let (_b, ui_commit) = db.add_entry_committed(&ctx.id, "todo", "B").unwrap();
        assert_eq!((ui_commit.seq, ui_commit.agent_type.as_str()), (2, "user"));

        // agent still on base_version=1 replaces the todo list → conflict, B survives
        let err = db.sc_commit_patch(&ctx.id, 1, &ScPatch { todos: Some(vec![entry("A"), entry("C")]), ..Default::default() }, &meta("lost update")).unwrap_err();
        assert_eq!(err, "__conflict__:2");
        let todos: Vec<String> = db.list_entries(&ctx.id).unwrap().into_iter().map(|e| e.content).collect();
        assert_eq!(todos, vec!["A", "B"]);

        // after re-reading, the merged commit goes through
        let c3 = db.sc_commit_patch(&ctx.id, 2, &ScPatch { todos: Some(vec![entry("A"), entry("B"), entry("C")]), overview: Some("ov".into()), ..Default::default() }, &meta("merged")).unwrap();
        assert_eq!(c3.seq, 3);
        assert_eq!(db.get_context_row(&ctx.id).unwrap().overview, "ov");
        let snap: ScSnapshot = serde_json::from_str(&c3.snapshot).unwrap();
        assert_eq!(snap.todos.len(), 3);

        // UI edits of overview / entry status / deletion are versions; no-ops are not
        assert!(db.update_context_committed(&ctx.id, "ctx", "ov", "").unwrap().is_none());
        assert_eq!(db.update_context_committed(&ctx.id, "ctx", "ov2", "").unwrap().unwrap().seq, 4);
        let first = db.list_entries(&ctx.id).unwrap().remove(0);
        assert!(db.update_entry_committed(&first.id, &first.content, &first.status).unwrap().is_none());
        assert_eq!(db.update_entry_committed(&first.id, &first.content, "done").unwrap().unwrap().seq, 5);
        assert_eq!(db.delete_entry_committed(&first.id).unwrap().unwrap().seq, 6);
        assert!(db.delete_entry_committed(&first.id).unwrap().is_none());

        // failure anywhere in the commit rolls back overview + entries + version
        let before = db.sc_snapshot(&ctx.id).unwrap();
        db.with(|c| c.execute_batch("CREATE TRIGGER reject_commit BEFORE INSERT ON sc_commits BEGIN SELECT RAISE(ABORT,'fixture failure'); END;")).unwrap();
        let err = db.sc_commit_patch(&ctx.id, 6, &ScPatch { overview: Some("never".into()), todos: Some(vec![]), ..Default::default() }, &meta("boom")).unwrap_err();
        assert!(err.contains("fixture failure"), "{err}");
        let after = db.sc_snapshot(&ctx.id).unwrap();
        assert_eq!(after.0, before.0);
        assert_eq!(after.1.overview, before.1.overview);
        assert_eq!(after.1.todos.len(), before.1.todos.len());
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

