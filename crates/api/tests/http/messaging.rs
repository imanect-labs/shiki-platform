//! 職員間メッセージの HTTP 結合テスト（実 Postgres ＋ 実 OpenFGA・Task 14.4 / 14.16）。
//!
//! 購読中に剥奪された接続へ以後の発言が届かないこと（PIT-64）、組織を越えて読めないこと、
//! 監査担当者の経路が通常の経路と分かれていること（PIT-65）を HTTP で確かめる。

use super::*;
use authz::{OpenFgaClient, OpenFgaConfig};
use futures::StreamExt;
use serde_json::{json, Value};

async fn call(
    app: &axum::Router,
    sid: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(
                    COOKIE,
                    format!("shiki_session={sid}.default; shiki_csrf=csrf"),
                )
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(if body.is_null() {
                    String::new()
                } else {
                    body.to_string()
                }))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// SSE を開き、届いたイベント名を順に送るチャネルを返す。
async fn open_stream(
    app: &axum::Router,
    sid: &str,
    path: &str,
) -> (StatusCode, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(
                    COOKIE,
                    format!("shiki_session={sid}.default; shiki_csrf=csrf"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut body = response.into_body().into_data_stream();
    tokio::spawn(async move {
        let mut buf = String::new();
        while let Some(Ok(chunk)) = body.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = buf.find("\n\n") {
                let frame: String = buf.drain(..end + 2).collect();
                let event = frame
                    .lines()
                    .find_map(|l| l.strip_prefix("event:"))
                    .map(|s| s.trim().to_string());
                let data = frame
                    .lines()
                    .find_map(|l| l.strip_prefix("data:"))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if let Some(e) = event {
                    let _ = tx.send(format!("{e} {data}"));
                }
            }
        }
        let _ = tx.send("closed".into());
    });
    (status, rx)
}

async fn next_event(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>, want: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(e)) if e.starts_with(want) => return e,
            Ok(Some(e)) if e.starts_with("keepalive") || e.starts_with("ready") => {}
            Ok(Some(e)) if want != "closed" && e == "closed" => panic!("{want} の前に閉じた"),
            Ok(Some(_)) => {}
            _ => panic!("{want} が 5 秒以内に届かない"),
        }
    }
}

#[tokio::test]
async fn messaging_http_stream_closes_on_revocation_and_stays_in_org() {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return;
    };
    let Ok(fga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let fga: Arc<OpenFgaClient> = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: fga_url,
                store_name: format!("messaging-http-{}", uuid::Uuid::new_v4()),
            },
            &serde_json::from_str(include_str!(
                "../../../authz/model/authorization-model.json"
            ))
            .unwrap(),
        )
        .await
        .unwrap(),
    );
    // alice・bob は同じ org、carol は別の org（テナントは同じ）。
    let org = format!("msg-{}", uuid::Uuid::new_v4().simple());
    let other_org = format!("msg-{}", uuid::Uuid::new_v4().simple());
    let sessions = Arc::new(MemorySessionStore::new());
    for (sid, user, group) in [
        ("sid-alice", "alice", &org),
        ("sid-bob", "bob", &org),
        ("sid-carol", "carol", &other_org),
    ] {
        let mut record = session_record(now() + 3600, None, "csrf");
        record.principal.id = user.into();
        record.principal.groups = vec![format!("/{group}")];
        sessions
            .put("default", sid, &record, Duration::from_secs(3600))
            .await
            .unwrap();
        let ctx = authz::AuthContext::new(
            record.principal.clone(),
            group.to_string(),
            authz::TenantId::parse("default").unwrap(),
        );
        fga.write_tuple(
            &ctx.subject(),
            Relation::Member,
            &ctx.ns().organization(ctx.org()),
        )
        .await
        .unwrap();
    }
    let mut state = state_with_authz(sessions, fga.clone());
    let svc = Arc::new(messaging::MessagingService::new(
        pool,
        fga.clone(),
        messaging::pubsub::Hub::local(),
        Arc::new(messaging::files::NoFiles),
    ));
    state.messaging = Some(svc);
    let app = build_router(state);

    let (status, ch) = call(
        &app,
        "sid-alice",
        "POST",
        "/messaging/channels",
        json!({"name": "企画", "kind": "private", "members": ["bob"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ch}");
    let id = ch["id"].as_str().unwrap().to_string();

    // 別 org の carol には存在ごと見えない。
    let (status, _) = call(
        &app,
        "sid-carol",
        "GET",
        &format!("/messaging/channels/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = open_stream(
        &app,
        "sid-carol",
        &format!("/messaging/channels/{id}/stream"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // bob が購読し、alice の発言が届く。
    let (status, mut rx) =
        open_stream(&app, "sid-bob", &format!("/messaging/channels/{id}/stream")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/channels/{id}/messages"),
        json!({"blocks": [{"type": "text", "text": "届く発言"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let e = next_event(&mut rx, "message").await;
    assert!(e.contains("届く発言"), "{e}");

    // 購読中に bob を外すと leave が届いて接続が閉じ、以後の発言は届かない。
    let (status, _) = call(
        &app,
        "sid-alice",
        "DELETE",
        &format!("/messaging/channels/{id}/members/bob"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    next_event(&mut rx, "leave").await;
    call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/channels/{id}/messages"),
        json!({"blocks": [{"type": "text", "text": "外した後の発言"}]}),
    )
    .await;
    next_event(&mut rx, "closed").await;
    let (status, _) = call(
        &app,
        "sid-bob",
        "GET",
        &format!("/messaging/channels/{id}/messages"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 監査担当者の経路は /org-audit に分かれ、監査担当者でなければ middleware で拒否される。
    let (status, _) = call(
        &app,
        "sid-alice",
        "GET",
        "/org-audit/messaging/channels",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
