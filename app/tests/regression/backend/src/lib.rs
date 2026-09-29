// Compile the patched ACP, manager, workflow engine, registry and DB directly.
// The Tauri GUI/event transport, installation discovery, MCP port and Windows
// job object integration are replaced by small test doubles. No audited
// function bodies are copied or modified.
extern crate self as tauri;
use std::sync::{Arc, Mutex};
use serde::Serialize;
use serde_json::Value;
#[derive(Clone, Default)]
pub struct AppHandle { pub events: Arc<Mutex<Vec<(String, Value)>>> }
pub trait Emitter { fn emit<T: Serialize>(&self, name: &str, payload: T) -> Result<(), String>; }
impl Emitter for AppHandle {
    fn emit<T: Serialize>(&self, name: &str, payload: T) -> Result<(), String> {
        self.events.lock().unwrap().push((name.into(), serde_json::to_value(payload).unwrap())); Ok(())
    }
}
pub struct Paths;
impl Paths { pub fn app_data_dir(&self) -> Result<std::path::PathBuf,String> { Ok(std::env::temp_dir()) } }
pub trait Manager { fn path(&self) -> Paths; }
impl Manager for AppHandle { fn path(&self) -> Paths { Paths } }
pub mod async_runtime {
    pub use tokio::spawn;
    pub fn block_on<T>(f: impl std::future::Future<Output=T>) -> T { tokio::runtime::Handle::current().block_on(f) }
}
#[path = "../../../../src-tauri/src/models.rs"] pub mod models;
#[path = "../../../../src-tauri/src/db.rs"] pub mod db;
#[path = "../../../../src-tauri/src/schedule.rs"] pub mod schedule;
#[path = "../../../../src-tauri/src/acp.rs"] pub mod acp;
#[path = "../../../../src-tauri/src/agents.rs"] pub mod agents;
#[path = "../../../../src-tauri/src/manager.rs"] pub mod manager;
#[path = "../../../../src-tauri/src/engine.rs"] pub mod engine;
pub mod child_job { pub fn attach(_: &tokio::process::Child) {} }
pub mod mcp { pub fn port_from_settings(_: &crate::db::Db) -> i64 { 8345 } }
pub mod node_rt {
    pub fn require_deepseek_node(_: &crate::setup::Tools) -> Result<std::path::PathBuf,String> { Ok("/usr/bin/node".into()) }
}
pub mod setup {
    use std::path::{Path,PathBuf};
    #[cfg(windows)]
    pub fn hide_console(command: &mut std::process::Command) -> &mut std::process::Command {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000)
    }
    pub struct Tools { pub tools_dir: PathBuf }
    impl Tools {
        pub fn discover(_: &crate::db::Db) -> Self { Self {tools_dir: std::env::temp_dir()} }
        pub fn node_exe(&self) -> PathBuf { "/usr/bin/node".into() }
        pub fn npm_cli(&self) -> PathBuf { "npm".into() }
        pub fn acp_dir(&self) -> PathBuf { self.tools_dir.join("audit-no-packages") }
        pub fn acp_node_modules(&self) -> PathBuf { self.acp_dir().join("node_modules") }
        pub fn codex_adapter(&self) -> PathBuf { self.acp_dir().join("codex.js") }
        pub fn zcode_adapter(&self) -> PathBuf { self.acp_dir().join("zcode.js") }
        pub fn codex_exe(&self) -> Option<PathBuf> { None }
        pub fn zcode_cli(&self) -> Option<PathBuf> { None }
        pub fn zcode_provider_config(&self, _: &Path) -> Option<PathBuf> { None }
        pub fn python_exe(&self) -> PathBuf { "/usr/bin/python3".into() }
        pub fn vscode_exe(&self) -> Option<PathBuf> { None }
    }
}

pub mod changed_commands { include!(concat!(env!("OUT_DIR"), "/commands.rs")); }
