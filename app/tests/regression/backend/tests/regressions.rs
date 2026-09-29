use shidrive_audit_harness::{acp::AcpConnection, agents, db::Db, manager::{AgentManager,SessionTarget}, models::{AgentLaunch,Context,NO_PROJECT_ID},AppHandle};
use std::{sync::Arc,time::Duration,path::Path};
use serde_json::{json,Value};
fn launch() -> AgentLaunch { AgentLaunch {command:std::env::var("PYTHON").unwrap_or_else(|_| if cfg!(windows) {"python"} else {"python3"}.into()),args:vec!["-u".into(),concat!(env!("CARGO_MANIFEST_DIR"),"/mock_adapter.py").into()],env:Default::default()} }
fn db() -> Arc<Db> { Arc::new(Db::open(Path::new(":memory:")).unwrap()) }
fn contexts(db: &Db) -> (Context,Context) { (db.create_context(NO_PROJECT_ID,"A").unwrap(),db.create_context(NO_PROJECT_ID,"B").unwrap()) }
async fn manager() -> (Arc<AgentManager>,AppHandle,Context,Context) {
    let db=db(); let (a,b)=contexts(&db); let app=AppHandle::default();
    let m=Arc::new(AgentManager::new(app.clone(),db)); m.set_override("codex",Some(launch())).await;
    (m,app,a,b)
}
fn texts(app: &AppHandle) -> Vec<String> {
    app.events.lock().unwrap().iter().filter(|(ev,_)|ev=="acp://update")
    .filter_map(|(_,v)|v["update"]["content"]["text"].as_str().map(str::to_owned)).collect()
}
async fn started(app: &AppHandle, text: &str) {
    tokio::time::timeout(Duration::from_secs(3),async {
        while !texts(app).contains(&format!("START:{text}")) { tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.expect("mock prompt did not start");
}
async fn rpc(conn:&AcpConnection, method:&str, p:Value)->Value {conn.extension_request(&format!("_{method}"),p,Some(Duration::from_secs(3))).await.unwrap()}
fn ready(app:&AppHandle, context_id:&str)->Value { app.events.lock().unwrap().iter().rev().find(|(ev,v)|ev=="acp://session-ready" && v["contextId"]==context_id).unwrap().1.clone() }
fn snapshot(sid:&str)->String {json!({"version":2,"sessionId":sid,"items":[{"kind":"user","text":"keep"}]}).to_string()}

#[tokio::test]
async fn manual_binary_inherits_defaults_but_nonempty_args_are_exact() {
    let m=AgentManager::new(AppHandle::default(),db());
    for agent in ["opencode","cursor","cline","codebuddy","qoder","minimax","grok"] {
        m.set_override(agent,Some(AgentLaunch{command:format!("{agent}.exe"),args:vec![],env:Default::default()})).await;
        assert_eq!(m.launch_for(agent).await.unwrap().args,agents::spec(agent).unwrap().args);
        m.set_override(agent,Some(AgentLaunch{command:format!("{agent}.exe"),args:vec!["custom-protocol".into()],env:Default::default()})).await;
        assert_eq!(m.launch_for(agent).await.unwrap().args,vec!["custom-protocol"]);
    }
}
#[tokio::test]
async fn automatic_launch_appends_user_args() {
    let m=AgentManager::new(AppHandle::default(),db());
    let package=m.tools.acp_node_modules().join("cline");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::write(package.join("package.json"),r#"{"name":"cline","bin":{"cline":"bin/index.js"}}"#).unwrap();
    std::fs::write(package.join("bin/index.js"),"").unwrap();
    m.set_override("cline",Some(AgentLaunch{command:"".into(),args:vec!["--verbose".into()],env:Default::default()})).await;
    let launch=m.launch_for("cline").await.unwrap();
    assert!(launch.args.ends_with(&["--acp".into(),"--verbose".into()]));
    std::fs::remove_dir_all(package).unwrap();
}
#[test]
fn npm_recipes_match_pinned_official_registry() {
    let upstream:Value=serde_json::from_str(include_str!("../../registry-recipes.json")).unwrap();
    for agent in ["cline","codebuddy","qoder","minimax","grok"] {
        assert_eq!(serde_json::to_value(&agents::spec(agent).unwrap().args).unwrap(),upstream["recipes"][agent]["args"]);
    }
}
#[tokio::test]
async fn capabilities_are_scoped_to_session_including_config_and_mode_changes() {
    let (m,app,a,b)=manager().await;
    let (_,sid_a)=m.ensure_session(&a,"codex").await.unwrap();
    let (_,sid_b)=m.ensure_session(&b,"codex").await.unwrap(); assert_ne!(sid_a,sid_b);
    m.ensure_session(&a,"codex").await.unwrap();
    assert_eq!(ready(&app,&a.id)["response"]["models"]["currentModelId"],"model-1");
    m.set_config_option(&a.id,"codex","model",json!("custom-A"),Some(&sid_a)).await.unwrap();
    m.set_mode(&a.id,"codex","custom-mode").await.unwrap();
    m.ensure_session(&b,"codex").await.unwrap();
    assert_eq!(ready(&app,&b.id)["response"]["models"]["currentModelId"],"model-2");
    m.ensure_session(&a,"codex").await.unwrap();
    assert_eq!(ready(&app,&a.id)["response"]["models"]["currentModelId"],"custom-A");
    assert_eq!(ready(&app,&a.id)["response"]["modes"]["currentModeId"],"custom-mode");
    assert!(m.set_config_option(&a.id,"codex","model",json!("wrong"),Some(&sid_b)).await.is_err());
    m.disconnect("codex").await;
}
#[tokio::test]
async fn load_response_populates_capabilities() {
    let (m,app,a,_)=manager().await;
    let (old,_)=m.ensure_session(&a,"codex").await.unwrap(); m.disconnect("codex").await;
    let (new,_)=m.ensure_session(&a,"codex").await.unwrap();
    assert_ne!(old.connection_id,new.connection_id);
    assert_eq!(ready(&app,&a.id)["response"]["models"]["currentModelId"],"history-model");
    assert_eq!(ready(&app,&a.id)["response"]["configOptions"][0]["currentValue"],"history-model");
    m.disconnect("codex").await;
}
#[tokio::test]
async fn two_contexts_sharing_a_sid_serialize_without_losing_chunks() {
    let (m,app,a,b)=manager().await;
    let (_,sid)=m.ensure_session(&a,"codex").await.unwrap();
    m.db.set_binding_session(&b.id,"codex",Some(&sid),None,None).unwrap();
    let m1=m.clone(); let a1=a.clone();
    let first=tokio::spawn(async move {m1.prompt(&a1,"codex","A",&[]).await}); started(&app,"A").await;
    m.prompt(&b,"codex","B",&[]).await.unwrap(); first.await.unwrap().unwrap();
    assert_eq!(texts(&app),vec!["START:A","TAIL:A","START:B","TAIL:B"]);
    m.disconnect("codex").await;
}
#[tokio::test]
async fn explicit_workflow_and_ui_share_sid_lock_but_not_events() {
    let (m,app,a,_)=manager().await; let (_,sid)=m.ensure_session(&a,"codex").await.unwrap();
    let m1=m.clone(); let a1=a.clone();
    let first=tokio::spawn(async move {m1.prompt(&a1,"codex","A",&[]).await}); started(&app,"A").await;
    let second=m.prompt_with(&a,"codex",SessionTarget::Explicit(sid),"workflow",&[]).await.unwrap(); first.await.unwrap().unwrap();
    assert_eq!(texts(&app),vec!["START:A","TAIL:A"]);
    assert_eq!(second["_shidriveOutput"],"START:workflowTAIL:workflow");
    m.disconnect("codex").await;
}
#[tokio::test]
async fn distinct_sids_can_run_concurrently() {
    let (m,app,a,b)=manager().await;
    m.ensure_session(&a,"codex").await.unwrap(); m.ensure_session(&b,"codex").await.unwrap();
    let m1=m.clone(); let a1=a.clone();
    let first=tokio::spawn(async move {m1.prompt(&a1,"codex","LONG",&[]).await}); started(&app,"LONG").await;
    m.prompt(&b,"codex","B",&[]).await.unwrap(); assert!(!first.is_finished()); first.await.unwrap().unwrap();
    assert!(texts(&app).contains(&"TAIL:LONG".into())); m.disconnect("codex").await;
}
#[tokio::test]
async fn empty_history_never_restarts_other_sessions() {
    let (m,app,a,b)=manager().await; let (conn,_)=m.ensure_session(&a,"codex").await.unwrap();
    m.db.set_binding_session(&b.id,"codex",Some("historical-empty"),None,None).unwrap();
    let m1=m.clone(); let a1=a.clone();
    let first=tokio::spawn(async move {m1.prompt(&a1,"codex","LONG",&[]).await}); started(&app,"LONG").await;
    let rows=m.bind_session(&b,"codex","historical-empty",None,true).await.unwrap(); assert!(rows.is_empty());
    assert!(conn.is_alive()); first.await.unwrap().unwrap();
    assert!(texts(&app).contains(&"TAIL:LONG".into()));
    assert_eq!(conn.connection_id,m.ensure_connected("codex").await.unwrap().connection_id); m.disconnect("codex").await;
}
#[tokio::test]
async fn history_load_waits_for_same_sid_prompt() {
    let (m,app,a,b)=manager().await; let (_,sid)=m.ensure_session(&a,"codex").await.unwrap();
    let m1=m.clone(); let a1=a.clone();
    let first=tokio::spawn(async move {m1.prompt(&a1,"codex","A",&[]).await}); started(&app,"A").await;
    m.bind_session(&b,"codex",&sid,None,false).await.unwrap(); first.await.unwrap().unwrap();
    assert_eq!(texts(&app),vec!["START:A","TAIL:A"]); m.disconnect("codex").await;
}
#[tokio::test]
async fn aborted_prompt_retains_lock_until_cancel_ack() {
    let (m,app,a,b)=manager().await; let (_,sid)=m.ensure_session(&a,"codex").await.unwrap();
    m.db.set_binding_session(&b.id,"codex",Some(&sid),None,None).unwrap();
    let m1=m.clone(); let a1=a.clone();
    let first=tokio::spawn(async move {m1.prompt(&a1,"codex","LONG",&[]).await}); started(&app,"LONG").await;
    first.abort(); let _=first.await;
    m.prompt(&b,"codex","B",&[]).await.unwrap();
    assert_eq!(texts(&app),vec!["START:LONG","CANCELLED:LONG","START:B","TAIL:B"]);
    m.disconnect("codex").await;
}
#[tokio::test]
async fn capture_is_cleaned_up_if_load_future_is_aborted() {
    let (m,app,a,_)=manager().await; let conn=m.ensure_connected("codex").await.unwrap();
    rpc(&conn,"audit/control",json!({"loadDelay":0.15})).await;
    let m1=m.clone(); let a1=a.clone();
    let load=tokio::spawn(async move {m1.bind_session(&a1,"codex","slow-history",None,false).await});
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {if rpc(&conn,"audit/state",json!({})).await["loads"]["slow-history"].as_u64().unwrap_or(0)>0 {break} tokio::time::sleep(Duration::from_millis(5)).await;}
    }).await.unwrap();
    load.abort(); let _=load.await; tokio::time::sleep(Duration::from_millis(180)).await;
    assert!(!conn.is_loaded("slow-history"));
    m.db.set_binding_session(&a.id,"codex",Some("slow-history"),None,None).unwrap();
    m.prompt(&a,"codex","AFTER",&[]).await.unwrap(); assert_eq!(texts(&app),vec!["START:AFTER","TAIL:AFTER"]);
    m.disconnect("codex").await;
}
#[tokio::test]
async fn unbind_and_fresh_session_reject_active_binding_without_data_loss() {
    let (m,app,a,_)=manager().await; let (_,sid)=m.ensure_session(&a,"codex").await.unwrap();
    let key=format!("{}:codex",a.id); let saved=snapshot(&sid); m.db.chat_store_set(&key,&saved).unwrap();
    let m1=m.clone(); let a1=a.clone();
    let first=tokio::spawn(async move {m1.prompt(&a1,"codex","A",&[]).await}); started(&app,"A").await;
    assert!(m.unbind(&a.id,"codex").await.is_err()); assert!(m.create_fresh_session(&a,"codex").await.is_err());
    assert_eq!(m.db.chat_store_get(&key).unwrap(),Some(saved)); first.await.unwrap().unwrap(); m.disconnect("codex").await;
}
#[tokio::test]
async fn failed_new_or_load_preserves_binding_and_snapshot() {
    let (m,_,a,_)=manager().await; let (conn,sid)=m.ensure_session(&a,"codex").await.unwrap();
    let key=format!("{}:codex",a.id); let saved=snapshot(&sid); m.db.chat_store_set(&key,&saved).unwrap();
    rpc(&conn,"audit/control",json!({"failNew":true})).await;
    assert!(m.create_fresh_session(&a,"codex").await.is_err());
    assert_eq!(m.db.get_binding(&a.id,"codex").unwrap().unwrap().session_id,Some(sid.clone()));
    assert_eq!(m.db.chat_store_get(&key).unwrap(),Some(saved.clone()));
    m.disconnect("codex").await; let conn=m.ensure_connected("codex").await.unwrap();
    rpc(&conn,"audit/control",json!({"failLoad":true})).await; assert!(m.ensure_session(&a,"codex").await.is_err());
    assert_eq!(m.db.get_binding(&a.id,"codex").unwrap().unwrap().session_id,Some(sid));
    assert_eq!(m.db.chat_store_get(&key).unwrap(),Some(saved)); m.disconnect("codex").await;
}
#[tokio::test]
async fn stale_silent_history_refresh_cannot_restore_old_binding() {
    let (m,_,a,_)=manager().await; let (_,old)=m.ensure_session(&a,"codex").await.unwrap();
    let new=m.create_fresh_session(&a,"codex").await.unwrap();
    assert!(m.bind_session(&a,"codex",&old,None,true).await.is_err());
    assert_eq!(m.db.get_binding(&a.id,"codex").unwrap().unwrap().session_id,Some(new)); m.disconnect("codex").await;
}
#[test]
fn rebinding_unbinding_and_late_snapshots_obey_sid_identity() {
    let db=db(); let (a,_)=contexts(&db); let key=format!("{}:codex",a.id);
    db.set_binding_session(&a.id,"codex",Some("A"),None,None).unwrap(); db.chat_store_set(&key,&snapshot("A")).unwrap();
    db.set_binding_session(&a.id,"codex",Some("A"),None,None).unwrap(); assert!(db.chat_store_get(&key).unwrap().is_some());
    db.set_binding_session(&a.id,"codex",Some("B"),None,None).unwrap(); assert!(db.chat_store_get(&key).unwrap().is_none());
    db.chat_store_set(&key,&snapshot("A")).unwrap(); assert!(db.chat_store_get(&key).unwrap().is_none());
    db.chat_store_set(&key,&snapshot("B")).unwrap(); db.unbind(&a.id,"codex").unwrap();
    db.chat_store_set(&key,&snapshot("B")).unwrap(); assert!(db.chat_store_get(&key).unwrap().is_none());
}
#[tokio::test]
async fn both_request_types_are_drained_and_closed_connection_is_freed() {
    for method in ["audit/permission","audit/elicitation"] {
        let app=AppHandle::default(); let conn=AcpConnection::spawn(app.clone(),"codex",&launch()).await.unwrap();
        let weak=Arc::downgrade(&conn); rpc(&conn,method,json!({})).await;
        conn.shutdown(); drop(conn); tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(weak.strong_count(),0,"{method} must release all waiters");
        let events=app.events.lock().unwrap(); let request=events.iter().find(|(e,_)|e=="acp://permission" || e=="acp://elicitation").unwrap();
        let cancelled=events.iter().find(|(e,_)|e=="acp://requests-cancelled").unwrap();
        assert!(cancelled.1["requestIds"].as_array().unwrap().contains(&request.1["requestId"]));
    }
}
#[tokio::test]
async fn session_request_cancellation_does_not_cancel_other_sessions() {
    let app=AppHandle::default(); let conn=AcpConnection::spawn(app.clone(),"codex",&launch()).await.unwrap();
    rpc(&conn,"audit/permission",json!({"sessionId":"A"})).await; rpc(&conn,"audit/elicitation",json!({"sessionId":"B"})).await;
    let ids:Vec<_>=app.events.lock().unwrap().iter().filter(|(e,_)|e=="acp://permission" || e=="acp://elicitation").map(|(_,v)|v["requestId"].as_str().unwrap().to_string()).collect();
    conn.cancel_user_requests(Some("A")); assert!(conn.resolve_permission(&ids[0],"allow").is_err());
    conn.resolve_elicitation(&ids[1],json!({"action":"decline"})).unwrap(); conn.shutdown();
}
#[tokio::test]
async fn temporary_and_bound_workflows_never_emit_chat_turns_or_touch_ui_status() {
    let (m,app,a,_)=manager().await; let (_,bound)=m.ensure_session(&a,"codex").await.unwrap();
    m.db.set_binding_status(&a.id,"codex","before").unwrap(); app.events.lock().unwrap().clear();
    for target in [SessionTarget::Temp,SessionTarget::Binding] {
        let result=m.prompt_with(&a,"codex",target,"TEMP",&[]).await.unwrap();
        assert_eq!(result["_shidriveOutput"],"START:TEMPTAIL:TEMP");
    }
    let events=app.events.lock().unwrap().clone();
    assert!(!events.iter().any(|(e,_)|e=="acp://update" || e=="acp://binding-status"));
    assert_eq!(m.db.get_binding(&a.id,"codex").unwrap().unwrap().status,"before");
    assert_eq!(m.db.get_binding(&a.id,"codex").unwrap().unwrap().session_id,Some(bound));
    m.disconnect("codex").await;
}
#[tokio::test]
async fn rejected_buffer_owner_cannot_erase_current_turn() {
    let conn=AcpConnection::spawn(AppHandle::default(),"codex",&launch()).await.unwrap();
    conn.begin_turn("s1","A","owner","chat").unwrap(); assert!(conn.begin_turn("s1","B","other","chat").is_err());
    conn.end_turn("s1","other"); assert!(conn.has_active_turn("s1")); conn.end_turn("s1","owner");
    assert!(!conn.has_active_turn("s1")); conn.shutdown();
}

#[tokio::test]
async fn workflow_output_is_persisted_in_run_log_not_chat() {
    use shidrive_audit_harness::{engine::Engine,models::WorkflowStep};
    let (m,app,a,_)=manager().await;
    let step:WorkflowStep=serde_json::from_value(json!({"type":"agent","context_id":a.id,"agent_type":"codex","prompt":"WORKFLOW"})).unwrap();
    let workflow=m.db.create_workflow(NO_PROJECT_ID,"regression","",true,"manual",None,&[step],&Default::default(),&[]).unwrap();
    let engine=Arc::new(Engine::new(app.clone(),m.db.clone(),m.clone())); engine.run_now(&workflow.id).unwrap();
    let run=tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            if let Some(run)=m.db.list_runs(&workflow.id,1).unwrap().into_iter().next() {
                if run.status!="running" {break run}
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_eq!(run.status,"success"); assert!(run.log.contains("START:WORKFLOWTAIL:WORKFLOW"));
    assert!(texts(&app).is_empty()); assert!(m.db.get_binding(&a.id,"codex").unwrap().is_none());
    m.disconnect("codex").await;
}
