// Integration-style regression tests for the OAuth handshake and v0.3.6 token compatibility.
use super::*;
use crate::remote_mcp::{active_grants, grant_create, grant_pause, RemoteGrantInput};
use std::io::Read;
use std::net::{TcpListener, TcpStream};

fn test_db() -> (Arc<Db>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("shidrive-oauth-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    (Arc::new(Db::open(&dir.join("test.db")).unwrap()), dir)
}

fn add_grant(db: &Arc<Db>, root: &std::path::Path, name: &str) -> RemoteGrant {
    let project_root = root.join(name);
    std::fs::create_dir_all(&project_root).unwrap();
    grant_create(
        db,
        &RemoteGrantInput {
            project_id: name.into(),
            project_name: name.into(),
            project_root: project_root.to_string_lossy().into_owned(),
            context_id: Some(format!("context-{name}")),
            context_name: format!("context {name}"),
            context_enabled: true,
            fs_write: true,
            exec_allowed: false,
        },
    )
    .unwrap()
    .0
}

fn pending(client_id: &str, redirect_uri: &str, challenge: &str) -> PendingTxn {
    PendingTxn {
        client_id: client_id.into(),
        client_name: "test app".into(),
        redirect_uri: redirect_uri.into(),
        scope: "fs:read fs:write context".into(),
        state: "csrf-state".into(),
        code_challenge: challenge.into(),
        created: Instant::now(),
        decision: None,
    }
}

// /token writes a real HTTP response; test both the status line and JSON payload.
fn token_call(db: &Arc<Db>, oauth: &OAuthState, form: &str) -> (u16, Value) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    token_endpoint(db, oauth, form, &mut server).unwrap();
    drop(server);
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    let code: u16 = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = response.split_once("\r\n\r\n").unwrap().1;
    (code, serde_json::from_str(body).unwrap())
}

#[test]
fn pending_survives_unmounted_settings_and_consent_does_not_create_grant() {
    let (db, dir) = test_db();
    let oauth = OAuthState::new();
    let txn_id = "request-1".to_string();
    oauth.txns.lock().unwrap().insert(
        txn_id.clone(),
        pending("client-1", "http://localhost/callback", "pkce-challenge"),
    );
    assert_eq!(pending_list(&oauth)[0].txn_id, txn_id);
    assert!(
        oauth_decide(&db, &oauth, &txn_id, true).is_err(),
        "不能批准空的开放列表"
    );
    let _grant = add_grant(&db, &dir, "one");
    assert!(oauth_decide(&db, &oauth, &txn_id, true).is_ok());
    assert_eq!(
        active_grants(&db).unwrap().len(),
        1,
        "审批不得新增静态 Token 或授权行"
    );
    assert!(pending_list(&oauth).is_empty());
    assert!(
        oauth_decide(&db, &oauth, &txn_id, true).is_err(),
        "重复点击不能签发两次"
    );
    let codes = oauth.codes.lock().unwrap();
    let code = codes.values().next().unwrap();
    assert_eq!(code.grant_id, "");
    assert_eq!(code.scope, "fs:read fs:write context");
    assert_eq!(code.redirect_uri, "http://localhost/callback");
    drop(codes);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn exchange_refresh_revoke_and_legacy_binding() {
    let (db, dir) = test_db();
    let grant = add_grant(&db, &dir, "one");
    let oauth = OAuthState::new();
    let reg = client_register(
        &db,
        "test client",
        &["http://localhost:3000/callback".into()],
    )
    .unwrap();
    let client_id = reg["client_id"].as_str().unwrap();
    let verifier = "a".repeat(50);
    let txn_id = "request-2";
    oauth.txns.lock().unwrap().insert(
        txn_id.into(),
        pending(
            client_id,
            "http://localhost:3000/callback",
            &sha256_b64url(&verifier),
        ),
    );
    oauth_decide(&db, &oauth, txn_id, true).unwrap();
    let code = oauth.codes.lock().unwrap().keys().next().unwrap().clone();
    let form = format!("grant_type=authorization_code&client_id={client_id}&code={code}&redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fcallback&code_verifier={verifier}");
    let (status, issued) = token_call(&db, &oauth, &form);
    assert_eq!(status, 200);
    let access = issued["access_token"].as_str().unwrap();
    let refresh = issued["refresh_token"].as_str().unwrap();
    assert!(matches!(
        resolve_access_token(&db, access).unwrap(),
        Some(OAuthAccess::Shared { .. })
    ));
    assert_eq!(active_grants(&db).unwrap().len(), 1);
    assert_eq!(token_call(&db, &oauth, &form).0, 400, "授权码不可重复兑换");

    // 新增 / 暂停现有授权会改变同一 OAuth 客户端的开放范围；无需再审批项目。
    let second = add_grant(&db, &dir, "two");
    assert_eq!(active_grants(&db).unwrap().len(), 2);
    grant_pause(&db, &second.id).unwrap();
    assert_eq!(active_grants(&db).unwrap().len(), 1);
    let (status, rotated) = token_call(
        &db,
        &oauth,
        &format!("grant_type=refresh_token&client_id={client_id}&refresh_token={refresh}"),
    );
    assert_eq!(status, 200);
    assert!(resolve_access_token(&db, access).unwrap().is_none());
    let rotated_access = rotated["access_token"].as_str().unwrap();
    assert!(matches!(
        resolve_access_token(&db, rotated_access).unwrap(),
        Some(OAuthAccess::Shared { .. })
    ));
    let rows = tokens_list(&db).unwrap();
    assert_eq!(rows[0].client_name, "test client");
    token_revoke(&db, &rows[0].id).unwrap();
    assert!(resolve_access_token(&db, rotated_access).unwrap().is_none());
    assert_eq!(
        token_call(
            &db,
            &oauth,
            &format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={}",
                rotated["refresh_token"].as_str().unwrap()
            )
        )
        .0,
        400
    );

    // v0.3.6 的旧令牌仍指向一条授权，不因升级变成跨项目凭据。
    let old = issue_tokens(&db, client_id, &grant.id, "fs:read").unwrap();
    match resolve_access_token(&db, old["access_token"].as_str().unwrap()).unwrap() {
        Some(OAuthAccess::Legacy(g)) => assert_eq!(g.id, grant.id),
        _ => panic!("旧版 OAuth 授权意外扩大"),
    }
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn expired_pending_is_not_returned_or_approvable() {
    let (db, dir) = test_db();
    let oauth = OAuthState::new();
    let mut txn = pending("old-client", "http://localhost/cb", "pkce");
    txn.created = Instant::now() - TXN_TTL - Duration::from_secs(1);
    oauth.txns.lock().unwrap().insert("expired".into(), txn);
    assert!(pending_list(&oauth).is_empty());
    assert!(oauth_decide(&db, &oauth, "expired", true).is_err());
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn explicit_scope_is_never_broadened() {
    assert_eq!(
        normalize_scope(""),
        Some("fs:read fs:write exec context".into())
    );
    assert_eq!(
        normalize_scope("context fs:read context unknown"),
        Some("context fs:read".into())
    );
    assert_eq!(normalize_scope("unknown"), None);
}

/// v0.3.10 的 cleanup_revoked 在 tokens_list（设置页每 4 秒轮询）里删除所有「没有令牌」的
/// 客户端注册：刚 /register 完、等用户点同意的客户端会在 /authorize 前消失；吊销后想重新
/// 授权的客户端也因注册行被删而失败。注册行必须在授权流程窗口与吊销保留期内存活，
/// 列表则只展示有效授权。
#[test]
fn listing_tokens_never_deletes_live_or_recently_revoked_client_registrations() {
    let (db, dir) = test_db();
    let _grant = add_grant(&db, &dir, "one");
    let oauth = OAuthState::new();
    // ① 刚注册、尚未授权的客户端：设置页轮询多次后 /authorize 仍必须认识它
    let fresh = client_register(&db, "fresh client", &["http://localhost:4000/cb".into()]).unwrap();
    let fresh_id = fresh["client_id"].as_str().unwrap().to_string();
    for _ in 0..3 {
        let _ = tokens_list(&db).unwrap();
    }
    assert!(client_get(&db, &fresh_id).is_some(), "in-flight registration must survive the settings-page poll");

    // ② 授权过再吊销：列表里不再显示，但注册行仍在（客户端缓存的 client_id 可重新授权）
    let reg = client_register(&db, "revoked client", &["http://localhost:3000/callback".into()]).unwrap();
    let client_id = reg["client_id"].as_str().unwrap().to_string();
    let verifier = "b".repeat(50);
    oauth.txns.lock().unwrap().insert(
        "request-9".into(),
        pending(&client_id, "http://localhost:3000/callback", &sha256_b64url(&verifier)),
    );
    oauth_decide(&db, &oauth, "request-9", true).unwrap();
    let code = oauth.codes.lock().unwrap().keys().next().unwrap().clone();
    let form = format!("grant_type=authorization_code&client_id={client_id}&code={code}&redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fcallback&code_verifier={verifier}");
    assert_eq!(token_call(&db, &oauth, &form).0, 200);
    let rows = tokens_list(&db).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].client_name, "revoked client");
    token_revoke(&db, &rows[0].id).unwrap();
    assert!(tokens_list(&db).unwrap().is_empty(), "revoked tokens are hidden from the list");
    assert!(client_get(&db, &client_id).is_some(), "registration survives revocation");
    assert!(client_get(&db, &fresh_id).is_some());

    // ③ 真正的垃圾才回收：吊销超过 90 天的令牌行；注册超过 24h 且已无任何令牌的客户端
    db.with(|c| {
        c.execute("UPDATE remote_oauth_tokens SET revoked_at='2000-01-01T00:00:00'", [])?;
        Ok(())
    })
    .unwrap();
    let _ = tokens_list(&db).unwrap();
    let token_rows: i64 = db.with(|c| c.query_row("SELECT COUNT(*) FROM remote_oauth_tokens", [], |r| r.get(0))).unwrap();
    assert_eq!(token_rows, 0, "revoked token past retention is purged");
    assert!(client_get(&db, &client_id).is_some(), "a recently registered client is kept even with no tokens left");
    db.with(|c| {
        c.execute("UPDATE remote_oauth_clients SET created_at='2000-01-01T00:00:00'", [])?;
        Ok(())
    })
    .unwrap();
    let _ = tokens_list(&db).unwrap();
    assert!(client_get(&db, &fresh_id).is_none(), "stale never-authorized registration is garbage-collected");
    assert!(client_get(&db, &client_id).is_none(), "stale registration without tokens is garbage-collected");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
