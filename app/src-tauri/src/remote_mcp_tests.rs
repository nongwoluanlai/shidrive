use super::*;
use serde_json::json;

fn fixture() -> (Arc<Db>, PathBuf, RemoteGrant, RemoteGrant) {
    let dir = std::env::temp_dir().join(format!("shidrive-shared-test-{}", uuid::Uuid::new_v4()));
    let path_a = dir.join("project");
    let path_b = dir.join("project-next"); // 目录名有前缀关系，不能当作 project 的子目录
    std::fs::create_dir_all(&path_a).unwrap();
    std::fs::create_dir_all(&path_b).unwrap();
    let db = Arc::new(Db::open(&dir.join("test.db")).unwrap());
    let add = |id: &str, root: &Path, write: bool| {
        grant_create(
            &db,
            &RemoteGrantInput {
                project_id: id.into(),
                project_name: id.into(),
                project_root: root.to_string_lossy().into_owned(),
                context_id: Some(format!("ctx-{id}")),
                context_name: format!("Context {id}"),
                context_enabled: true,
                fs_write: write,
                exec_allowed: write,
            },
        )
        .unwrap()
        .0
    };
    let a = add("a", &path_a, false);
    let b = add("b", &path_b, true);
    (db, dir, a, b)
}

fn tool_names(tools: &serde_json::Value) -> Vec<&str> {
    tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect()
}

#[test]
fn shared_list_and_schema_follow_scope_and_current_grants() {
    let (db, dir, a, b) = fixture();
    let grants = active_grants(&db).unwrap();
    let tools = shared_tools(&grants, "fs:read context", false);
    let names = tool_names(&tools);
    assert!(names.contains(&"pc.access.list"));
    assert!(names.contains(&"pc.fs.read"));
    assert!(names.contains(&"context_update"));
    assert!(!names.contains(&"pc.fs.write"));
    assert!(!names.contains(&"pc.process.exec"));
    let ctx_tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "context_get")
        .unwrap();
    assert!(ctx_tool["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .contains(&json!("context_id")));
    let path_tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "pc.fs.read")
        .unwrap();
    assert_eq!(
        path_tool["inputSchema"]["properties"]["grant_id"]["type"],
        "string"
    );
    let access = shared_access_list(&grants, "context", false);
    assert_eq!(access["grants"].as_array().unwrap().len(), 2);
    assert!(access["grants"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["root"].is_null()));

    // 已暂停的开放目录立即从共享 OAuth 工具和列表消失；恢复后回来。
    grant_pause(&db, &b.id).unwrap();
    let live = active_grants(&db).unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, a.id);
    assert!(
        !tool_names(&shared_tools(&live, "fs:read fs:write exec context", true))
            .contains(&"pc.fs.write")
    );
    assert!(select_shared_grant(
        &live,
        "fs:read",
        "pc.fs.read",
        &json!({ "grant_id": b.id }),
        false
    )
    .is_err());
    grant_resume(&db, &b.id).unwrap();
    assert_eq!(active_grants(&db).unwrap().len(), 2);
    grant_delete(&db, &b.id).unwrap();
    assert_eq!(active_grants(&db).unwrap().len(), 1);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn routing_requires_open_grant_and_never_treats_similar_prefix_as_child() {
    let (db, dir, a, b) = fixture();
    let grants = active_grants(&db).unwrap();
    let file_a = PathBuf::from(&a.project_root).join("file.txt");
    let file_b = PathBuf::from(&b.project_root).join("file.txt");
    std::fs::write(&file_a, "a").unwrap();
    std::fs::write(&file_b, "b").unwrap();
    let a_args = json!({ "path": file_a.to_string_lossy() });
    let b_args = json!({ "path": file_b.to_string_lossy() });
    assert_eq!(
        select_shared_grant(&grants, "fs:read", "pc.fs.read", &a_args, false)
            .unwrap()
            .id,
        a.id
    );
    assert_eq!(
        select_shared_grant(&grants, "fs:read", "pc.fs.read", &b_args, false)
            .unwrap()
            .id,
        b.id
    );
    assert!(select_shared_grant(
        &grants,
        "fs:read",
        "pc.fs.read",
        &json!({ "path": "file.txt" }),
        false
    )
    .is_err());
    assert!(select_shared_grant(
        &grants,
        "fs:read",
        "pc.fs.read",
        &json!({ "grant_id": "fake", "path": "file.txt" }),
        false
    )
    .is_err());
    assert!(select_shared_grant(
        &grants,
        "fs:read",
        "pc.fs.write",
        &json!({ "grant_id": a.id, "path": "x" }),
        false
    )
    .is_err());
    assert!(select_shared_grant(
        &grants,
        "exec",
        "pc.process.exec",
        &json!({ "grant_id": b.id }),
        false
    )
    .is_err());
    assert_eq!(
        select_shared_grant(
            &grants,
            "exec",
            "pc.process.exec",
            &json!({ "grant_id": b.id }),
            true
        )
        .unwrap()
        .id,
        b.id
    );
    // 显式错误的 grant_id 不会绕过 coding_mcp 的根目录检查。
    let cfg = crate::coding_mcp::Cfg {
        root: PathBuf::from(&a.project_root),
        token: String::new(),
        auth_user: String::new(),
        auth_pass: String::new(),
        exec_enabled: false,
    };
    assert!(crate::coding_mcp::call_tool(&cfg, "pc.fs.read", &b_args).is_err());
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_context_must_be_explicit_and_opened_but_passcode_remains_pinned() {
    let (db, dir, a, b) = fixture();
    let grants = active_grants(&db).unwrap();
    let shared = RemoteAccess::Shared {
        scopes: "context".into(),
    };
    assert!(authorize_tool(&shared, &grants, &db, "context_get", &mut json!({})).is_err());
    assert!(authorize_tool(
        &shared,
        &grants,
        &db,
        "context_get",
        &mut json!({ "context_id": "not-open" })
    )
    .is_err());
    assert!(authorize_tool(
        &shared,
        &grants,
        &db,
        "context_get",
        &mut json!({ "context_id": "ctx-b" })
    )
    .is_ok());
    let read_only = RemoteAccess::Shared {
        scopes: "fs:read".into(),
    };
    assert!(authorize_tool(
        &read_only,
        &grants,
        &db,
        "context_get",
        &mut json!({ "context_id": "ctx-b" })
    )
    .is_err());
    let passcode = RemoteAccess::Grant(a.clone());
    let mut forged = json!({ "context_id": "ctx-b" });
    authorize_tool(&passcode, &[], &db, "context_get", &mut forged).unwrap();
    assert_eq!(forged["context_id"], "ctx-a");
    grant_pause(&db, &b.id).unwrap();
    assert!(authorize_tool(
        &shared,
        &active_grants(&db).unwrap(),
        &db,
        "context_get",
        &mut json!({ "context_id": "ctx-b" })
    )
    .is_err());
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
