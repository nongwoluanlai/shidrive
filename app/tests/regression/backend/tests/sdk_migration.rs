//! Exercise the actual production SDK integration over subprocess stdio.
use agent_client_protocol::schema::v1::{ContentBlock, ImageContent, TextContent};
use serde_json::{json, Value};
use shidrive_audit_harness::{
    acp::AcpConnection,
    db::Db,
    manager::AgentManager,
    models::{AgentLaunch, NO_PROJECT_ID},
    AppHandle,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn launch() -> AgentLaunch {
    AgentLaunch {
        command: std::env::var("PYTHON")
            .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into()),
        args: vec![
            "-u".into(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/mock_adapter.py").into(),
        ],
        env: Default::default(),
    }
}
async fn connection() -> (Arc<AcpConnection>, AppHandle) {
    let app = AppHandle::default();
    (
        AcpConnection::spawn(app.clone(), "codex", &launch())
            .await
            .unwrap(),
        app,
    )
}
async fn rpc(conn: &AcpConnection, method: &str, params: Value) -> Value {
    conn.extension_request(
        &format!("_audit/{method}"),
        params,
        Some(Duration::from_secs(4)),
    )
    .await
    .unwrap()
}
async fn remote_reply(conn: &AcpConnection, id: Value) -> Value {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let records = rpc(conn, "records", json!({})).await;
            if let Some(reply) = records["replies"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
            {
                return reply.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("SDK did not return the server-request response")
}
fn ui_id(app: &AppHandle, kind: &str, sid: &str) -> String {
    app.events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(e, v)| e == &format!("acp://{kind}") && v["sessionId"] == sid)
        .unwrap()
        .1["requestId"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn permission(sid: &str) -> Value {
    json!({"sessionId": sid, "toolCall": {"toolCallId":"t"},
        "options":[{"optionId":"allow", "name":"Allow", "kind":"allow_once"}]})
}
fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text(TextContent::new(value))]
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("shidrive-sdk-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn typed_handshake_prompt_images_and_close_use_stable_v1() {
    let (conn, _) = connection().await;
    let created = conn.new_session("/workspace", vec![]).await.unwrap();
    let sid = created["sessionId"].as_str().unwrap();
    assert_eq!(
        created["models"]["currentModelId"], "model-1",
        "legacy models must not disappear in SDK decoding"
    );
    let mut blocks = text("SDK");
    blocks.push(ContentBlock::Image(ImageContent::new(
        "aGVsbG8=",
        "image/png",
    )));
    assert_eq!(
        conn.prompt_session(sid, blocks).await.unwrap()["stopReason"],
        "end_turn"
    );
    conn.close_session(sid).await.unwrap();
    let records = rpc(&conn, "records", json!({})).await;
    let wire = records["wire"].as_array().unwrap();
    let init = &wire[0];
    assert_eq!(init["method"], "initialize");
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["params"]["protocolVersion"], 1);
    assert_eq!(
        init["params"]["clientCapabilities"]["fs"]["readTextFile"],
        true
    );
    assert_eq!(
        init["params"]["clientCapabilities"]["fs"]["writeTextFile"],
        true
    );
    assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
    assert_eq!(init["params"]["clientInfo"]["name"], "shidrive");
    let prompt = wire
        .iter()
        .find(|r| r["method"] == "session/prompt")
        .unwrap();
    assert_eq!(
        prompt["params"]["prompt"][1],
        json!({"type":"image", "data":"aGVsbG8=", "mimeType":"image/png"})
    );
    conn.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_correlates_concurrent_out_of_order_replies() {
    let (conn, _) = connection().await;
    let results = futures::future::join_all((0..32).map(|i| {
        let conn = conn.clone();
        async move {
            let answer = rpc(
                &conn,
                "echo",
                json!({"value":i,"delay":(31-i) as f64 / 1000.0}),
            )
            .await;
            assert_eq!(answer, i);
        }
    }))
    .await;
    assert_eq!(results.len(), 32);
    let r = rpc(&conn, "records", json!({})).await;
    let ids: std::collections::HashSet<_> = r["wire"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r.get("id"))
        .collect();
    assert_eq!(ids.len(), r["wire"].as_array().unwrap().len());
    conn.shutdown();
}

#[tokio::test]
async fn timed_out_and_dropped_sdk_requests_do_not_poison_later_replies() {
    let (conn, _) = connection().await;
    let error = conn
        .extension_request(
            "_audit/echo",
            json!({"value":"timeout", "delay":0.25}),
            Some(Duration::from_millis(20)),
        )
        .await
        .unwrap_err();
    assert!(error.contains("超时"));
    let task_conn = conn.clone();
    let dropped = tokio::spawn(async move {
        task_conn
            .extension_request(
                "_audit/echo",
                json!({"value":"dropped", "delay":0.25}),
                None,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let r = rpc(&conn, "records", json!({})).await;
            if r["wire"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["params"]["value"] == "dropped")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    dropped.abort();
    let _ = dropped.await;
    assert_eq!(rpc(&conn, "echo", json!({"value":"fresh"})).await, "fresh");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        rpc(&conn, "echo", json!({"value":"after late replies"})).await,
        "after late replies"
    );
    let records = rpc(&conn, "records", json!({})).await;
    assert_eq!(
        records["rpcCancels"].as_array().unwrap().len(),
        2,
        "SDK owns drop-time RPC cancellation"
    );
    assert!(conn.is_alive());
    conn.shutdown();
}

#[tokio::test]
async fn typed_filesystem_handlers_read_ranges_and_accept_standard_and_legacy_write() {
    let temp = Temp::new();
    let path = temp.0.join("text.txt");
    let (conn, app) = connection().await;
    for (id, field, content) in [
        (1, "content", "one\ntwo\nthree\n"),
        (2, "contents", "legacy\ntext\n"),
    ] {
        let mut params = json!({"sessionId":"fs", "path":path});
        params[field] = json!(content);
        rpc(
            &conn,
            "server_request",
            json!({"id":id, "method":"fs/write_text_file", "params":params}),
        )
        .await;
        assert!(remote_reply(&conn, json!(id)).await.get("result").is_some());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        rpc(
            &conn,
            "server_request",
            json!({"id":format!("read{id}"), "method":"fs/read_text_file",
            "params":{"sessionId":"fs", "path":path, "line":2, "limit":1}}),
        )
        .await;
        assert_eq!(
            remote_reply(&conn, json!(format!("read{id}"))).await["result"]["content"],
            if id == 1 { "two\n" } else { "text\n" }
        );
    }
    assert_eq!(
        app.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(e, v)| e == "acp://update" && v.get("fileChanged").is_some())
            .count(),
        2
    );
    conn.shutdown();
}

#[tokio::test]
async fn waiting_dialogs_do_not_block_dispatch_and_string_numeric_ids_remain_distinct() {
    let (conn, app) = connection().await;
    for (id, sid) in [(json!(42), "numeric"), (json!("42"), "string")] {
        rpc(
            &conn,
            "server_request",
            json!({"id":id, "method":"session/request_permission", "params":permission(sid)}),
        )
        .await;
    }
    let numeric = ui_id(&app, "permission", "numeric");
    let string = ui_id(&app, "permission", "string");
    assert_ne!(numeric, string);
    assert_eq!(
        conn.prompt_session("unrelated", text("WHILE_WAITING"))
            .await
            .unwrap()["stopReason"],
        "end_turn"
    );
    conn.resolve_permission(&string, "allow").unwrap();
    assert_eq!(
        remote_reply(&conn, json!("42")).await["result"],
        json!({"outcome":{"outcome":"selected","optionId":"allow"}})
    );
    conn.cancel_user_requests(Some("numeric"));
    assert_eq!(
        remote_reply(&conn, json!(42)).await["result"],
        json!({"outcome":{"outcome":"cancelled"}})
    );
    conn.shutdown();
}

#[tokio::test]
async fn peer_rpc_cancellation_removes_only_its_dialog() {
    let (conn, app) = connection().await;
    for sid in ["cancel", "keep"] {
        rpc(
            &conn,
            "server_request",
            json!({"id":sid, "method":"session/request_permission", "params":permission(sid)}),
        )
        .await;
    }
    let cancel = ui_id(&app, "permission", "cancel");
    let keep = ui_id(&app, "permission", "keep");
    rpc(
        &conn,
        "notify",
        json!({"method":"$/cancel_request", "params":{"requestId":"cancel"}}),
    )
    .await;
    assert_eq!(
        remote_reply(&conn, json!("cancel")).await["error"]["code"],
        -32800
    );
    assert!(conn.resolve_permission(&cancel, "allow").is_err());
    conn.resolve_permission(&keep, "allow").unwrap();
    assert!(remote_reply(&conn, json!("keep"))
        .await
        .get("result")
        .is_some());
    assert!(app
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|(e, v)| e == "acp://requests-cancelled"
            && v["requestIds"].as_array().unwrap().contains(&json!(cancel))));
    conn.shutdown();
}

#[tokio::test]
async fn all_existing_elicitation_aliases_round_trip_through_sdk_responders() {
    let (conn, app) = connection().await;
    for (n, method) in [
        "elicitation/create",
        "session/elicitation/create",
        "elicitation/request",
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("e{n}");
        rpc(
            &conn,
            "server_request",
            json!({"id":id, "method":method,
            "params":{"sessionId":id, "message":"Question", "requestedSchema":{"type":"object"}}}),
        )
        .await;
        conn.resolve_elicitation(
            &ui_id(&app, "elicitation", &id),
            json!({"action":"accept", "content":{"answer":"yes"}}),
        )
        .unwrap();
        assert_eq!(
            remote_reply(&conn, json!(id)).await["result"]["content"]["answer"],
            "yes"
        );
    }
    conn.shutdown();
}

#[tokio::test]
async fn sdk_returns_protocol_errors_for_unknown_methods_and_malformed_standard_requests() {
    let (conn, app) = connection().await;
    for (id, method, params, code) in [
        ("unknown", "_not_implemented", json!({}), -32601),
        (
            "bad-write",
            "fs/write_text_file",
            json!({"sessionId":"s", "path":"unused"}),
            -32602,
        ),
        (
            "bad-permission",
            "session/request_permission",
            json!({"sessionId":"s", "options":"wrong type"}),
            -32602,
        ),
    ] {
        rpc(
            &conn,
            "server_request",
            json!({"id":id, "method":method, "params":params}),
        )
        .await;
        assert_eq!(remote_reply(&conn, json!(id)).await["error"]["code"], code);
    }
    assert!(!app
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|(e, _)| e == "acp://permission"));
    assert!(conn.is_alive());
    conn.shutdown();
}

#[tokio::test]
async fn typed_load_falls_back_to_resume_and_preserves_legacy_caps() {
    let (conn, _) = connection().await;
    rpc(&conn, "control", json!({"loadUnsupported":true})).await;
    let response = conn
        .load_session("history", "/workspace", vec![])
        .await
        .unwrap();
    assert_eq!(response["models"]["currentModelId"], "history-model");
    assert!(conn.is_loaded("history"));
    let r = rpc(&conn, "records", json!({})).await;
    let methods: Vec<_> = r["wire"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["method"].as_str())
        .filter(|s| s.starts_with("session/"))
        .collect();
    assert_eq!(methods, ["session/load", "session/load", "session/resume"]);
    conn.shutdown();
}

#[tokio::test]
async fn typed_session_list_supports_pages_array_form_and_existing_aliases() {
    let (conn, _) = connection().await;
    rpc(&conn, "control", json!({"listPages":[
        {"sessions":[{"sessionId":"canonical", "cwd":"/work", "title":"A"}], "nextCursor":"second"},
        {"sessions":[{"session_id":"legacy", "updated_at":"2026-09-29T00:00:00Z", "workingDirectory":"/old"}, {"session_id":"unknown-cwd"}]}
    ]})).await;
    let sessions = conn.session_list().await.unwrap();
    assert_eq!(
        sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        ["canonical", "legacy", "unknown-cwd"]
    );
    assert_eq!(sessions[1].cwd.as_deref(), Some("/old"));
    assert_eq!(sessions[2].cwd, None);
    rpc(
        &conn,
        "control",
        json!({"listPages":[[{"session_id":"root-array"}]]}),
    )
    .await;
    assert_eq!(
        conn.session_list().await.unwrap()[0].session_id,
        "root-array"
    );
    conn.shutdown();
}

#[tokio::test]
async fn legacy_empty_config_ack_keeps_session_menu_and_typed_value() {
    let db = Arc::new(Db::open(Path::new(":memory:")).unwrap());
    let context = db.create_context(NO_PROJECT_ID, "sdk").unwrap();
    let manager = AgentManager::new(AppHandle::default(), db);
    manager.set_override("codex", Some(launch())).await;
    let (conn, sid) = manager.ensure_session(&context, "codex").await.unwrap();
    for ack in [Value::Null, json!({})] {
        rpc(&conn, "control", json!({"configAck":ack})).await;
        manager
            .set_config_option(&context.id, "codex", "model", json!("selected"), Some(&sid))
            .await
            .unwrap();
        let caps = conn.cached_caps(&sid);
        assert_eq!(caps["configOptions"][0]["currentValue"], "selected");
        assert_eq!(
            caps["configOptions"][0]["options"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    conn.set_session_config(&sid, "flag", &json!(true))
        .await
        .unwrap();
    assert!(conn
        .set_session_config(&sid, "flag", &json!(["invalid"]))
        .await
        .is_err());
    let r = rpc(&conn, "records", json!({})).await;
    let request = r["wire"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|r| r["method"] == "session/set_config_option")
        .unwrap();
    assert_eq!(request["params"]["configId"], "flag");
    assert_eq!(request["params"]["configOptionId"], "flag");
    assert_eq!(request["params"]["type"], "boolean");
    assert_eq!(request["params"]["value"], true);
    manager.disconnect("codex").await;
}

#[tokio::test]
async fn malformed_required_response_fields_fail_without_losing_connection() {
    let (conn, _) = connection().await;
    rpc(
        &conn,
        "control",
        json!({"malformedNew":true, "malformedPrompt":true}),
    )
    .await;
    assert!(conn
        .new_session("/work", vec![])
        .await
        .unwrap_err()
        .contains("sessionId"));
    assert!(conn
        .prompt_session("s", text("malformed"))
        .await
        .unwrap_err()
        .contains("stopReason"));
    assert!(conn.is_alive());
    assert_eq!(
        rpc(&conn, "echo", json!({"value":"usable"})).await,
        "usable"
    );
    conn.shutdown();
}

#[tokio::test]
async fn sdk_batch_dispatch_handles_async_filesystem_and_unknown_request() {
    let temp = Temp::new();
    let file = temp.0.join("batch.txt");
    std::fs::write(&file, "batch read").unwrap();
    let (conn, _) = connection().await;
    rpc(&conn, "batch", json!({"messages":[
        {"jsonrpc":"2.0", "id":"batch-read", "method":"fs/read_text_file", "params":{"sessionId":"s", "path":file}},
        {"jsonrpc":"2.0", "id":"batch-unknown", "method":"_unknown", "params":{}}
    ]})).await;
    assert_eq!(
        remote_reply(&conn, json!("batch-read")).await["result"]["content"],
        "batch read"
    );
    assert_eq!(
        remote_reply(&conn, json!("batch-unknown")).await["error"]["code"],
        -32601
    );
    conn.shutdown();
}

#[tokio::test]
async fn unsupported_protocol_version_is_rejected_and_closed() {
    let app = AppHandle::default();
    let mut config = launch();
    config
        .env
        .insert("MOCK_PROTOCOL_VERSION".into(), "2".into());
    let error = AcpConnection::spawn(app.clone(), "codex", &config)
        .await
        .err()
        .expect("v2 must not be silently accepted");
    assert!(error.contains("协议版本"));
    let events = app.events.lock().unwrap();
    assert!(events.iter().any(|(_, v)| v["state"] == "disconnected"));
    assert!(!events.iter().any(|(_, v)| v["state"] == "connected"));
}

#[tokio::test]
async fn eof_and_codec_failures_release_pending_requests_and_both_dialog_queues() {
    for failure in ["eof", "utf8", "oversize"] {
        let (conn, app) = connection().await;
        let weak = Arc::downgrade(&conn);
        rpc(&conn, "permission", json!({})).await;
        rpc(&conn, "elicitation", json!({})).await;
        #[cfg(target_os = "linux")]
        let pid = rpc(&conn, "state", json!({})).await["pid"]
            .as_u64()
            .unwrap();
        let ids: Vec<_> = app
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(e, _)| e == "acp://permission" || e == "acp://elicitation")
            .map(|(_, v)| v["requestId"].clone())
            .collect();
        let method = if failure == "eof" {
            "_audit/exit"
        } else {
            "_audit/break_transport"
        };
        assert!(tokio::time::timeout(
            Duration::from_secs(8),
            conn.extension_request(method, json!({"kind":failure}), None)
        )
        .await
        .unwrap()
        .is_err());
        assert!(!conn.is_alive());
        for id in ids {
            assert!(app
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|(e, v)| e == "acp://requests-cancelled"
                    && v["requestIds"].as_array().unwrap().contains(&id)));
        }
        #[cfg(target_os = "linux")]
        tokio::time::timeout(Duration::from_secs(3), async {
            while Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("closed connection retained an unreaped process");
        drop(conn);
        tokio::task::yield_now().await;
        assert_eq!(weak.strong_count(), 0, "{failure} left a connection cycle");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_unterminated_response_and_chunks_survive_clean_eof() {
    for _ in 0..8 {
        let (conn, _) = connection().await;
        conn.begin_turn("s", "context", "turn", "chat").unwrap();
        let response = conn.prompt_session("s", text("SDK_EOF")).await.unwrap();
        assert_eq!(response["stopReason"], "end_turn");
        assert_eq!(conn.end_turn("s", "turn").0, "START:SDK_EOFTAIL:SDK_EOF");
        conn.shutdown();
    }
}

#[tokio::test]
async fn dropping_last_host_owner_closes_pending_dialog_without_a_reference_cycle() {
    let (conn, app) = connection().await;
    let weak = Arc::downgrade(&conn);
    rpc(&conn, "elicitation", json!({})).await;
    drop(conn);
    tokio::time::timeout(Duration::from_secs(3), async {
        while weak.strong_count() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(app
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|(e, _)| e == "acp://requests-cancelled"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancelled_initialize_kills_spawned_process_and_sdk_driver() {
    let temp = Temp::new();
    let pid_file = temp.0.join("pid");
    let app = AppHandle::default();
    let task_app = app.clone();
    let mut config = launch();
    config.env.insert(
        "MOCK_PID_FILE".into(),
        pid_file.to_string_lossy().to_string(),
    );
    config.env.insert("MOCK_INIT_DELAY".into(), "10".into());
    let task = tokio::spawn(async move { AcpConnection::spawn(task_app, "codex", &config).await });
    let pid = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file) {
                if pid.parse::<u32>().is_ok() {
                    return pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while Path::new(&format!("/proc/{pid}")).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled initialize leaked its process");
    assert!(app
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|(_, v)| v["state"] == "disconnected"));
}

async fn sdk_to_sdk(select: bool) {
    let temp = Temp::new();
    let path = temp.0.join("sdk.txt");
    let app = AppHandle::default();
    let config = AgentLaunch {
        command: env!("CARGO_BIN_EXE_sdk_peer").into(),
        args: vec![],
        env: Default::default(),
    };
    let conn = AcpConnection::spawn(app.clone(), "sdk-peer", &config)
        .await
        .unwrap();
    assert_eq!(
        conn.agent_info.lock().unwrap()["agentInfo"]["name"],
        "official-sdk-test-peer"
    );
    let created = conn
        .new_session(temp.0.to_str().unwrap(), vec![])
        .await
        .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_owned();
    conn.begin_turn(&sid, "ctx", "sdk-turn", "chat").unwrap();
    let (task_conn, task_sid, task_path) = (conn.clone(), sid.clone(), path.clone());
    let task = tokio::spawn(async move {
        task_conn
            .prompt_session(&task_sid, text(task_path.to_str().unwrap()))
            .await
    });
    tokio::time::timeout(Duration::from_secs(4), async {
        while !app
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|(e, _)| e == "acp://permission")
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("official SDK peer did not request permission");
    if select {
        conn.resolve_permission(&ui_id(&app, "permission", &sid), "allow")
            .unwrap();
    } else {
        conn.cancel_user_requests(Some(&sid));
    }
    let response = tokio::time::timeout(Duration::from_secs(4), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        response["stopReason"],
        if select { "end_turn" } else { "cancelled" }
    );
    let transcript = conn.end_turn(&sid, "sdk-turn").0;
    if select {
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "SDK-to-SDK round trip\n"
        );
        assert_eq!(transcript, "SDK-to-SDK round trip");
    } else {
        assert!(!path.exists());
        assert!(transcript.is_empty());
    }
    conn.shutdown();
}
#[tokio::test]
async fn official_sdk_agent_and_client_complete_permission_filesystem_and_stream_round_trip() {
    sdk_to_sdk(true).await;
}
#[tokio::test]
async fn official_sdk_agent_observes_cancelled_permission_without_writing_file() {
    sdk_to_sdk(false).await;
}
