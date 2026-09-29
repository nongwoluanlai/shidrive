//! Positive regressions for the SDK review findings. Real production sources,
//! official SDK and subprocess stdio; only native GUI/install discovery are stubbed.
use serde_json::{json, Value};
use shidrive_audit_harness::{
    acp::AcpConnection,
    db::Db,
    manager::{AgentManager, SessionTarget},
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
async fn send(conn: &AcpConnection, id: &str, method: &str, params: Value) {
    rpc(
        conn,
        "server_request",
        json!({"id":id,"method":method,"params":params}),
    )
    .await;
}
async fn reply(conn: &AcpConnection, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let records = rpc(conn, "records", json!({})).await;
            if let Some(value) = records["replies"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
            {
                return value.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("peer did not receive SDK response")
}
fn permission(sid: &str) -> Value {
    json!({"sessionId":sid,"toolCall":{"toolCallId":"t"},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]})
}
fn elicitation(sid: Option<&str>) -> Value {
    let mut value = json!({"message":"question","requestedSchema":{"type":"object","properties":{"answer":{"type":"string"}}}});
    if let Some(sid) = sid {
        value["sessionId"] = sid.into()
    }
    value
}
fn event_id(app: &AppHandle, kind: &str, sid: &str) -> String {
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
        .into()
}
fn dialogs(app: &AppHandle, sid: &str) -> usize {
    app.events
        .lock()
        .unwrap()
        .iter()
        .filter(|(e, v)| {
            (e == "acp://permission" || e == "acp://elicitation") && v["sessionId"] == sid
        })
        .count()
}
async fn wait_start(app: &AppHandle, text: &str) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !app.events.lock().unwrap().iter().any(|(e, v)| {
            (e == "acp://update" || e == "acp://workflow-update")
                && v["update"]["content"]["text"] == format!("START:{text}")
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("shidrive-r2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn single_blocking_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap()
}
struct HoldPool {
    release: Option<std::sync::mpsc::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl HoldPool {
    async fn new() -> Self {
        let (release, rx) = std::sync::mpsc::channel();
        let (start, ready) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let _ = start.send(());
            let _ = rx.recv_timeout(Duration::from_secs(12));
        });
        ready.await.unwrap();
        Self {
            release: Some(release),
            task: Some(task),
        }
    }
    async fn release(mut self) {
        let _ = self.release.take().unwrap().send(());
        self.task.take().unwrap().await.unwrap();
        // Let the blocked pool process already queued closures, including ones
        // whose async waiter was dropped. This is not just an early file check.
        tokio::task::spawn_blocking(|| {}).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}
impl Drop for HoldPool {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(());
        }
    }
}
async fn write(conn: &AcpConnection, id: &str, sid: &str, path: &Path, content: &str) {
    send(
        conn,
        id,
        "fs/write_text_file",
        json!({"sessionId":sid,"path":path,"content":content}),
    )
    .await;
}

#[test]
fn disconnect_prevents_queued_write_from_overwriting_post_disconnect_edit() {
    single_blocking_runtime().block_on(async {
        let tmp = Temp::new();
        let path = tmp.0.join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (conn, app) = connection().await;
        let hold = HoldPool::new().await;
        write(&conn, "old", "s", &path, "STALE").await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        conn.shutdown();
        assert!(!conn.is_alive());
        std::fs::write(&path, "USER_EDIT_AFTER_DISCONNECT").unwrap();
        hold.release().await;
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "USER_EDIT_AFTER_DISCONNECT"
        );
        assert!(!app
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|(_, v)| v.get("fileChanged").is_some()));
    });
}

#[test]
fn dropping_last_owner_also_cancels_queued_file_job_without_a_connection_cycle() {
    single_blocking_runtime().block_on(async {
        let tmp = Temp::new();
        let path = tmp.0.join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (conn, _) = connection().await;
        let hold = HoldPool::new().await;
        write(&conn, "old", "s", &path, "STALE").await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        let weak = Arc::downgrade(&conn);
        drop(conn);
        tokio::time::timeout(Duration::from_secs(2), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        std::fs::write(&path, "USER_EDIT_AFTER_DROP").unwrap();
        hold.release().await;
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "USER_EDIT_AFTER_DROP"
        );
    });
}

#[test]
fn eof_and_invalid_utf8_cancel_queued_writes_while_closed_host_is_cached() {
    single_blocking_runtime().block_on(async {
        for invalid_utf8 in [false, true] {
            let tmp = Temp::new();
            let path = tmp.0.join("file.txt");
            std::fs::write(&path, "original").unwrap();
            let (conn, _) = connection().await;
            let hold = HoldPool::new().await;
            write(&conn, "old", "s", &path, "STALE").await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let method = if invalid_utf8 {
                "_audit/break_transport"
            } else {
                "_audit/exit"
            };
            assert!(conn
                .extension_request(method, json!({"kind":"utf8"}), Some(Duration::from_secs(3)))
                .await
                .is_err());
            assert!(!conn.is_alive());
            std::fs::write(&path, "AFTER_EOF").unwrap();
            hold.release().await;
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "AFTER_EOF");
        }
    });
}

#[test]
fn peer_rpc_cancel_stops_queued_read_and_write_without_poisoning_later_requests() {
    single_blocking_runtime().block_on(async {
        let tmp = Temp::new();
        let path = tmp.0.join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (conn, _) = connection().await;
        let hold = HoldPool::new().await;
        write(&conn, "write", "s", &path, "STALE").await;
        send(
            &conn,
            "read",
            "fs/read_text_file",
            json!({"sessionId":"s","path":path}),
        )
        .await;
        for id in ["write", "read"] {
            rpc(
                &conn,
                "notify",
                json!({"method":"$/cancel_request","params":{"requestId":id}}),
            )
            .await;
            assert_eq!(reply(&conn, id).await["error"]["code"], -32800);
        }
        assert_eq!(
            rpc(&conn, "echo", json!({"value":"still responsive"})).await,
            "still responsive"
        );
        hold.release().await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        write(&conn, "fresh", "s", &path, "fresh").await;
        assert_eq!(reply(&conn, "fresh").await["result"], json!({}));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh");
        conn.shutdown();
    });
}

#[test]
fn cancelled_file_epoch_cannot_revive_when_same_sid_starts_new_turn() {
    single_blocking_runtime().block_on(async {
        let tmp = Temp::new();
        let old = tmp.0.join("old.txt");
        let other = tmp.0.join("other.txt");
        let fresh = tmp.0.join("fresh.txt");
        std::fs::write(&old, "original").unwrap();
        let (conn, _) = connection().await;
        conn.begin_turn("A", "a", "old-turn", "chat").unwrap();
        conn.begin_turn("B", "b", "other-turn", "chat").unwrap();
        let hold = HoldPool::new().await;
        write(&conn, "old", "A", &old, "STALE").await;
        write(&conn, "other", "B", &other, "B").await;
        conn.cancel_turn("A", "old-turn");
        assert_eq!(reply(&conn, "old").await["error"]["code"], -32800);
        conn.end_turn("A", "old-turn");
        conn.begin_turn("A", "a", "new-turn", "chat").unwrap();
        write(&conn, "fresh", "A", &fresh, "new").await;
        std::fs::write(&old, "AFTER_CANCEL").unwrap();
        hold.release().await;
        assert_eq!(reply(&conn, "other").await["result"], json!({}));
        assert_eq!(reply(&conn, "fresh").await["result"], json!({}));
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "AFTER_CANCEL");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "new");
        conn.shutdown();
    });
}

#[test]
fn bounded_file_queue_rejects_excess_without_blocking_sdk_dispatch() {
    single_blocking_runtime().block_on(async {
        let tmp = Temp::new();
        let path = tmp.0.join("file.txt");
        let (conn, _) = connection().await;
        let hold = HoldPool::new().await;
        for i in 0..33 {
            write(&conn, &format!("w{i}"), "s", &path, "queued").await;
        }
        assert_eq!(reply(&conn, "w32").await["error"]["code"], -32603);
        assert_eq!(rpc(&conn, "echo", json!({"value":42})).await, 42);
        conn.shutdown();
        hold.release().await;
        assert!(!path.exists());
    });
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_started_io_keeps_its_serial_lane_until_os_operation_finishes() {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = Temp::new();
    let fifo = tmp.0.join("fifo");
    let target = tmp.0.join("file.txt");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    // O_RDWR|O_NONBLOCK keeps the FIFO open, so a real read can wait inside the
    // OS. Closing this guard always unblocks it, including during test failure.
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(0x800)
        .open(&fifo)
        .unwrap();
    let (conn, _) = connection().await;
    send(
        &conn,
        "read",
        "fs/read_text_file",
        json!({"sessionId":"s","path":fifo}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(90)).await;
    rpc(
        &conn,
        "notify",
        json!({"method":"$/cancel_request","params":{"requestId":"read"}}),
    )
    .await;
    assert_eq!(reply(&conn, "read").await["error"]["code"], -32800);
    write(&conn, "write", "s", &target, "after-read").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !target.exists(),
        "ongoing cancelled IO must not release the serial lane early"
    );
    assert_eq!(
        rpc(&conn, "echo", json!({"value":"responsive"})).await,
        "responsive"
    );
    drop(writer);
    assert_eq!(reply(&conn, "write").await["result"], json!({}));
    conn.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manager_cancel_rejects_late_dialogs_before_and_after_ack_then_new_turn_is_allowed() {
    let app = AppHandle::default();
    let db = Arc::new(Db::open(Path::new(":memory:")).unwrap());
    let context = db.create_context(NO_PROJECT_ID, "review").unwrap();
    let manager = Arc::new(AgentManager::new(app.clone(), db));
    manager.set_override("codex", Some(launch())).await;
    let (conn, sid) = manager.ensure_session(&context, "codex").await.unwrap();
    rpc(&conn, "control", json!({"cancelAckDelay":0.6})).await;
    let (m, c) = (manager.clone(), context.clone());
    let prompt = tokio::spawn(async move { m.prompt(&c, "codex", "LONG", &[]).await });
    wait_start(&app, "LONG").await;
    send(
        &conn,
        "existing",
        "session/request_permission",
        permission(&sid),
    )
    .await;
    let old_id = event_id(&app, "permission", &sid);
    let count = dialogs(&app, &sid);
    manager.cancel(&context.id, "codex").await.unwrap();
    assert_eq!(
        reply(&conn, "existing").await["result"]["outcome"]["outcome"],
        "cancelled"
    );
    assert!(conn.resolve_permission(&old_id, "allow").is_err());
    send(
        &conn,
        "late",
        "session/request_permission",
        permission(&sid),
    )
    .await;
    assert_eq!(
        reply(&conn, "late").await["result"]["outcome"]["outcome"],
        "cancelled"
    );
    for (i, method) in [
        "elicitation/create",
        "session/elicitation/create",
        "elicitation/request",
    ]
    .iter()
    .enumerate()
    {
        let id = format!("late-elic-{i}");
        send(&conn, &id, method, elicitation(Some(&sid))).await;
        assert_eq!(reply(&conn, &id).await["result"]["action"], "cancel");
    }
    assert!(!prompt.is_finished());
    assert_eq!(
        dialogs(&app, &sid),
        count,
        "cancelled turn must not republish a dialog"
    );
    assert_eq!(prompt.await.unwrap().unwrap()["stopReason"], "cancelled");
    send(
        &conn,
        "after-ack",
        "session/request_permission",
        permission(&sid),
    )
    .await;
    assert_eq!(
        reply(&conn, "after-ack").await["result"]["outcome"]["outcome"],
        "cancelled"
    );
    assert_eq!(dialogs(&app, &sid), count);
    // A subsequent real manager prompt on the SAME SID must reopen the gate.
    let (m, c) = (manager.clone(), context.clone());
    let next = tokio::spawn(async move { m.prompt(&c, "codex", "NEXT", &[]).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !conn.has_active_turn(&sid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    send(
        &conn,
        "next",
        "session/request_permission",
        permission(&sid),
    )
    .await;
    conn.resolve_permission(&event_id(&app, "permission", &sid), "allow")
        .unwrap();
    assert_eq!(
        reply(&conn, "next").await["result"]["outcome"]["outcome"],
        "selected"
    );
    assert_eq!(next.await.unwrap().unwrap()["stopReason"], "end_turn");
    manager.disconnect("codex").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborted_workflow_closes_admission_while_keeping_sid_lock_until_ack() {
    let app = AppHandle::default();
    let db = Arc::new(Db::open(Path::new(":memory:")).unwrap());
    let context = db.create_context(NO_PROJECT_ID, "wf").unwrap();
    let manager = Arc::new(AgentManager::new(app.clone(), db));
    manager.set_override("codex", Some(launch())).await;
    let conn = manager.ensure_connected("codex").await.unwrap();
    rpc(&conn, "control", json!({"cancelAckDelay":0.6})).await;
    let (m, c) = (manager.clone(), context.clone());
    let prompt = tokio::spawn(async move {
        m.prompt_with(&c, "codex", SessionTarget::Temp, "LONG", &[])
            .await
    });
    wait_start(&app, "LONG").await;
    let sid = rpc(&conn, "state", json!({})).await["active"][0]
        .as_str()
        .unwrap()
        .to_owned();
    prompt.abort();
    assert!(prompt.await.is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while rpc(&conn, "state", json!({})).await["cancels"]
            .as_u64()
            .unwrap()
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(conn.has_active_turn(&sid));
    send(
        &conn,
        "late-wf",
        "session/request_permission",
        permission(&sid),
    )
    .await;
    assert_eq!(
        reply(&conn, "late-wf").await["result"]["outcome"]["outcome"],
        "cancelled"
    );
    assert_eq!(dialogs(&app, &sid), 0);
    tokio::time::timeout(Duration::from_secs(3), async {
        while conn.has_active_turn(&sid) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    manager.disconnect("codex").await;
}

#[tokio::test]
async fn cancel_is_scoped_and_sidless_elicitation_is_explicitly_connection_scoped() {
    let (conn, app) = connection().await;
    conn.begin_turn("A", "a", "turn-a", "chat").unwrap();
    conn.begin_turn("B", "b", "turn-b", "chat").unwrap();
    send(&conn, "global", "elicitation/create", elicitation(None)).await;
    let global_id = event_id(&app, "elicitation", "");
    conn.cancel_session("A");
    send(&conn, "B", "session/request_permission", permission("B")).await;
    conn.resolve_permission(&event_id(&app, "permission", "B"), "allow")
        .unwrap();
    conn.resolve_elicitation(
        &global_id,
        json!({"action":"accept","content":{"answer":"global"}}),
    )
    .unwrap();
    assert_eq!(
        reply(&conn, "B").await["result"]["outcome"]["outcome"],
        "selected"
    );
    assert_eq!(reply(&conn, "global").await["result"]["action"], "accept");
    send(&conn, "global2", "elicitation/create", elicitation(None)).await;
    let second = event_id(&app, "elicitation", "");
    conn.shutdown();
    assert!(app
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|(e, v)| e == "acp://requests-cancelled"
            && v["requestIds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id == &second)));
}

#[tokio::test]
async fn rejected_begin_and_stale_owner_do_not_reopen_or_cancel_another_turn() {
    let (conn, app) = connection().await;
    conn.begin_turn("s", "ctx", "old", "chat").unwrap();
    conn.cancel_turn("s", "old");
    assert!(conn.begin_turn("s", "ctx", "imposter", "chat").is_err());
    conn.end_turn("s", "imposter");
    send(
        &conn,
        "blocked",
        "session/request_permission",
        permission("s"),
    )
    .await;
    assert_eq!(
        reply(&conn, "blocked").await["result"]["outcome"]["outcome"],
        "cancelled"
    );
    assert_eq!(dialogs(&app, "s"), 0);
    conn.end_turn("s", "old");
    conn.begin_turn("s", "ctx", "new", "chat").unwrap();
    conn.cancel_turn("s", "old");
    conn.end_turn("s", "old");
    assert!(conn.has_active_turn("s"));
    send(
        &conn,
        "allowed",
        "session/request_permission",
        permission("s"),
    )
    .await;
    conn.resolve_permission(&event_id(&app, "permission", "s"), "allow")
        .unwrap();
    assert_eq!(
        reply(&conn, "allowed").await["result"]["outcome"]["outcome"],
        "selected"
    );
    assert_eq!(rpc(&conn, "state", json!({})).await["cancels"], 1);
    conn.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_and_admitting_dialogs_race_without_leaving_resolvable_requests() {
    let (conn, app) = connection().await;
    for i in 0..20 {
        let sid = format!("s{i}");
        conn.begin_turn(&sid, "ctx", "turn", "chat").unwrap();
        let (task_conn, task_sid) = (conn.clone(), sid.clone());
        let cancel = tokio::spawn(async move {
            tokio::task::yield_now().await;
            task_conn.cancel_turn(&task_sid, "turn");
        });
        send(
            &conn,
            &format!("race-{i}"),
            "session/request_permission",
            permission(&sid),
        )
        .await;
        cancel.await.unwrap();
        assert_eq!(
            reply(&conn, &format!("race-{i}")).await["result"]["outcome"]["outcome"],
            "cancelled"
        );
        let events = app.events.lock().unwrap().clone();
        for (index, (name, value)) in events.iter().enumerate() {
            if name == "acp://permission" && value["sessionId"] == sid {
                let id = value["requestId"].as_str().unwrap();
                assert!(conn.resolve_permission(id, "allow").is_err());
                assert!(
                    events[index + 1..]
                        .iter()
                        .any(|(e, v)| e == "acp://requests-cancelled"
                            && v["requestIds"].as_array().unwrap().iter().any(|v| v == id)),
                    "expiry must follow admission, not precede it"
                );
            }
        }
        conn.end_turn(&sid, "turn");
    }
    conn.shutdown();
}
