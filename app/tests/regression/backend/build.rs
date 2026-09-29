// Type-check the changed IPC function bodies without Tauri's desktop runtime.
// This does NOT validate Tauri command macros or Windows/WebView integration.
use std::{env,fs,path::PathBuf};
fn main() {
    let source_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../../src-tauri/src/commands.rs");
    println!("cargo:rerun-if-changed={}", source_path.display());
    let source = fs::read_to_string(source_path).unwrap();
    let mut generated = String::from("use serde_json::Value; use crate::models::*; type AgentsState<'a> = &'a std::sync::Arc<crate::manager::AgentManager>;\n");
    for name in ["binding_unbind", "acp_session_new", "acp_prompt", "acp_set_config_option"] {
        let signature = format!("pub async fn {name}(");
        let body = source.split("#[tauri::command]").find(|part| part.trim_start().starts_with(&signature)).expect("changed command missing");
        generated.push_str(body);
    }
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("commands.rs"), generated).unwrap();
}
