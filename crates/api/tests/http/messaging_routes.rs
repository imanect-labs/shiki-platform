//! 職員間メッセージの HTTP ルートを一通り通す結合テスト（実 Postgres ＋ 実 OpenFGA・Task 14.16）。
//!
//! 権限の negative は crates/messaging の結合テストが主に担う。ここではルート表・抽出子・
//! エラーの写像（404/403/400/204）と、各ハンドラが service へ正しく渡すことを確かめる。

use super::*;
use authz::{AuthContext, OpenFgaClient, OpenFgaConfig};
use chrono::{SecondsFormat, Utc};
use messaging::{AgentAnswer, AgentRequest, AgentRuntime, PendingApprovalRef};
use serde_json::{json, Value};

async fn send(
    app: &axum::Router,
    sid: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
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
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn call(
    app: &axum::Router,
    sid: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let raw = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    let (status, bytes) = send(app, sid, method, path, "application/json", raw).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// 文脈の行数だけを答える LLM の代役。
struct EchoRuntime;

#[async_trait]
impl AgentRuntime for EchoRuntime {
    async fn answer(
        &self,
        _ctx: &AuthContext,
        req: &AgentRequest,
    ) -> Result<AgentAnswer, messaging::Error> {
        Ok(AgentAnswer {
            text: format!("文脈 {} 件", req.transcript().len()),
            sources: Vec::new(),
            create_chat: None,
        })
    }

    async fn start_chat(
        &self,
        _ctx: &AuthContext,
        _title: &str,
        _req: &AgentRequest,
    ) -> Result<uuid::Uuid, messaging::Error> {
        Ok(uuid::Uuid::new_v4())
    }

    async fn chat_status(
        &self,
        _ctx: &AuthContext,
        _thread: uuid::Uuid,
    ) -> Result<(Vec<PendingApprovalRef>, bool), messaging::Error> {
        Ok((Vec::new(), true))
    }
}

/// 1x1 の PNG（エージェントのアイコン）。
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

fn text(s: &str) -> Value {
    json!([{"type": "text", "text": s}])
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn messaging_http_routes_round_trip() {
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
                store_name: format!("messaging-routes-{}", uuid::Uuid::new_v4()),
            },
            &serde_json::from_str(include_str!(
                "../../../authz/model/authorization-model.json"
            ))
            .unwrap(),
        )
        .await
        .unwrap(),
    );
    // alice は組織の管理者、bob は一般、carol は監査担当者（同じ組織）。
    let org = format!("msgr-{}", uuid::Uuid::new_v4().simple());
    // 利用者 ID は実行ごとに変える（directory_user はテナント内で user_id が一意・他のテストと共有）。
    let sfx = uuid::Uuid::new_v4().simple().to_string();
    let (alice, bob, carol) = (
        format!("alice-{sfx}"),
        format!("bob-{sfx}"),
        format!("carol-{sfx}"),
    );
    let sessions = Arc::new(MemorySessionStore::new());
    for (sid, role, user) in [
        ("sid-alice", "admin", &alice),
        ("sid-bob", "member", &bob),
        ("sid-carol", "auditor", &carol),
    ] {
        let mut record = session_record(now() + 3600, None, "csrf");
        record.principal.id = user.clone();
        record.principal.groups = vec![format!("/{org}")];
        sessions
            .put("default", sid, &record, Duration::from_secs(3600))
            .await
            .unwrap();
        let ctx = AuthContext::new(
            record.principal.clone(),
            org.clone(),
            authz::TenantId::parse("default").unwrap(),
        );
        let org_obj = ctx.ns().organization(ctx.org());
        fga.write_tuple(&ctx.subject(), Relation::Member, &org_obj)
            .await
            .unwrap();
        match role {
            "admin" => fga
                .write_tuple(&ctx.subject(), Relation::Admin, &org_obj)
                .await
                .map(|_| ())
                .unwrap(),
            "auditor" => fga
                .write_tuple(&ctx.subject(), Relation::AuditViewer, &org_obj)
                .await
                .map(|_| ())
                .unwrap(),
            _ => {}
        }
        sqlx::query(
            "INSERT INTO directory_user (tenant_id, org, user_id, email, display_name) \
             VALUES ('default', $1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(&org)
        .bind(user)
        .bind(format!("{user}@example.com"))
        .bind(format!("{role} さん"))
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut state = state_with_authz(sessions, fga.clone());
    state.messaging = Some(Arc::new(messaging::MessagingService::new(
        pool,
        fga.clone(),
        messaging::pubsub::Hub::local(),
        Arc::new(messaging::files::NoFiles),
    )));
    state.messaging_agent = Some(Arc::new(EchoRuntime));
    let app = build_router(state);

    // --- チャンネル ---
    let (s, ch) = call(
        &app,
        "sid-alice",
        "POST",
        "/messaging/channels",
        json!({"name": "routes", "kind": "public", "topic": "話題"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ch}");
    let id = ch["id"].as_str().unwrap().to_string();
    let (s, dup) = call(
        &app,
        "sid-alice",
        "POST",
        "/messaging/channels",
        json!({"name": "ROUTES", "kind": "public"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{dup}");
    let (s, list) = call(&app, "sid-alice", "GET", "/messaging/channels", Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().any(|c| c["id"] == ch["id"]));
    let (s, browse) = call(
        &app,
        "sid-bob",
        "GET",
        "/messaging/channels/browse?q=rout",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(browse
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == ch["id"]));
    let (s, _) = call(
        &app,
        "sid-bob",
        "POST",
        &format!("/messaging/channels/{id}/join"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, upd) = call(
        &app,
        "sid-alice",
        "PATCH",
        &format!("/messaging/channels/{id}"),
        json!({"topic": "新しい話題"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{upd}");
    let (s, _) = call(
        &app,
        "sid-bob",
        "PATCH",
        &format!("/messaging/channels/{id}"),
        json!({"topic": "管理者でない"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, members) = call(
        &app,
        "sid-alice",
        "GET",
        &format!("/messaging/channels/{id}/members"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(members.as_array().unwrap().len(), 2);
    let (s, _) = call(
        &app,
        "sid-alice",
        "PATCH",
        &format!("/messaging/channels/{id}/members/{bob}"),
        json!({"admin": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "sid-alice",
        "PATCH",
        &format!("/messaging/channels/{id}/members/{bob}"),
        json!({"admin": false}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "sid-bob",
        "PUT",
        &format!("/messaging/channels/{id}/notify"),
        json!({"level": "mentions"}),
    )
    .await;
    assert!(s.is_success(), "{s}");

    // --- 発言・スレッド・リアクション・編集・削除・検索・メンション ---
    let (s, first) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/channels/{id}/messages"),
        json!({"blocks": [
            {"type": "text", "text": "hello routes "},
            {"type": "mention", "mention": {"kind": "user", "user_id": bob}}
        ]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{first}");
    let mid = first["id"].as_str().unwrap().to_string();
    let (s, bad) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/channels/{id}/messages"),
        json!({"blocks": []}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    let (s, reply) = call(
        &app,
        "sid-bob",
        "POST",
        &format!("/messaging/channels/{id}/messages"),
        json!({"parent_id": mid, "blocks": text("thread reply")}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{reply}");
    let (s, thread) = call(
        &app,
        "sid-alice",
        "GET",
        &format!("/messaging/channels/{id}/threads/{mid}"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(thread.as_array().unwrap().len(), 2);
    let (s, page) = call(
        &app,
        "sid-bob",
        "GET",
        &format!("/messaging/channels/{id}/messages?limit=10"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page.as_array().unwrap().len(), 1);
    let (s, one) = call(
        &app,
        "sid-bob",
        "GET",
        &format!("/messaging/messages/{mid}"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(one["reply_count"], 1);
    let (s, _) = call(
        &app,
        "sid-bob",
        "POST",
        &format!("/messaging/messages/{mid}/reactions"),
        json!({"emoji": "👍", "on": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "sid-bob",
        "POST",
        &format!("/messaging/messages/{mid}/reactions"),
        json!({"emoji": "not-an-emoji", "on": true}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, mentions) = call(&app, "sid-bob", "GET", "/messaging/mentions", Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(mentions.as_array().unwrap().len(), 1);
    let (s, _) = call(
        &app,
        "sid-bob",
        "POST",
        "/messaging/mentions/read",
        json!({"message_ids": [mid]}),
    )
    .await;
    assert!(s.is_success(), "{s}");
    let (s, edited) = call(
        &app,
        "sid-alice",
        "PATCH",
        &format!("/messaging/messages/{mid}"),
        json!({"blocks": text("hello routes edited")}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{edited}");
    let (s, _) = call(
        &app,
        "sid-bob",
        "PATCH",
        &format!("/messaging/messages/{mid}"),
        json!({"blocks": text("他人の発言")}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, hits) = call(
        &app,
        "sid-bob",
        "GET",
        "/messaging/search?q=routes&limit=5",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(!hits.as_array().unwrap().is_empty());
    let (s, _) = call(
        &app,
        "sid-bob",
        "POST",
        &format!("/messaging/channels/{id}/read"),
        json!({}),
    )
    .await;
    assert!(s.is_success(), "{s}");
    let reply_id = reply["id"].as_str().unwrap();
    let (s, _) = call(
        &app,
        "sid-bob",
        "DELETE",
        &format!("/messaging/messages/{reply_id}"),
        Value::Null,
    )
    .await;
    assert!(s.is_success(), "{s}");

    // --- エージェント（名前・アイコン・自分だけに表示の回答の公開と破棄） ---
    let (s, profile) = call(&app, "sid-bob", "GET", "/messaging/agent", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{profile}");
    let (s, profile) = call(
        &app,
        "sid-bob",
        "PUT",
        "/messaging/agent",
        json!({"name": "ボブの補佐"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{profile}");
    let (s, _) = call(
        &app,
        "sid-bob",
        "PUT",
        "/messaging/agent",
        json!({"name": "管理者"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "なりすましになる名前は拒否");
    let (s, _) = send(
        &app,
        "sid-bob",
        "PUT",
        "/messaging/agent/icon",
        "image/png",
        PNG.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(
        &app,
        "sid-bob",
        "PUT",
        "/messaging/agent/icon",
        "image/png",
        b"not an image".to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, icon) = send(
        &app,
        "sid-alice",
        "GET",
        &format!("/messaging/agents/{bob}/icon"),
        "application/json",
        Vec::new(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(icon, PNG);
    let (s, _) = call(
        &app,
        "sid-bob",
        "DELETE",
        "/messaging/agent/icon",
        Value::Null,
    )
    .await;
    assert!(s.is_success(), "{s}");
    let (s, _) = send(
        &app,
        "sid-alice",
        "GET",
        &format!("/messaging/agents/{bob}/icon"),
        "application/json",
        Vec::new(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // 自分だけに表示で 2 回呼ぶ（応答は非同期）。1 件は公開し、1 件は破棄する。
    for q in ["要約して", "もう一度"] {
        let (s, v) = call(
            &app,
            "sid-bob",
            "POST",
            &format!("/messaging/channels/{id}/messages"),
            json!({"agent_private": true, "blocks": [
                {"type": "mention", "mention": {"kind": "agent"}},
                {"type": "text", "text": q}
            ]}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let mut replies = Vec::new();
    for _ in 0..50 {
        let (s, r) = call(
            &app,
            "sid-bob",
            "GET",
            &format!("/messaging/channels/{id}/agent-replies"),
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        replies = r.as_array().unwrap().clone();
        if replies.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(replies.len(), 2, "自分だけに表示の回答が 2 件届く");
    let (a, b) = (
        replies[0]["id"].as_str().unwrap(),
        replies[1]["id"].as_str().unwrap(),
    );
    let (s, _) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/agent-replies/{a}/publish"),
        json!({"confirmed": true}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "他人の回答は公開できない");
    let (s, published) = call(
        &app,
        "sid-bob",
        "POST",
        &format!("/messaging/agent-replies/{a}/publish"),
        json!({"confirmed": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{published}");
    assert_eq!(published["posted"], true);
    let (s, _) = call(
        &app,
        "sid-bob",
        "DELETE",
        &format!("/messaging/agent-replies/{b}"),
        Value::Null,
    )
    .await;
    assert!(s.is_success(), "{s}");

    // --- DM・非表示・アーカイブ・退出 ---
    let (s, dm) = call(
        &app,
        "sid-alice",
        "POST",
        "/messaging/dms",
        json!({"user_id": bob}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{dm}");
    let dm_id = dm["id"].as_str().unwrap().to_string();
    let (s, _) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/channels/{dm_id}/messages"),
        json!({"blocks": text("dm secret")}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "sid-bob",
        "POST",
        &format!("/messaging/channels/{dm_id}/hide"),
        Value::Null,
    )
    .await;
    assert!(s.is_success(), "{s}");
    let (s, _) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/channels/{id}/members"),
        json!({"user_ids": [carol]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "sid-carol",
        "DELETE",
        &format!("/messaging/channels/{id}/members/{carol}"),
        Value::Null,
    )
    .await;
    assert!(s.is_success(), "自分で退出できる");
    let (s, _) = call(
        &app,
        "sid-alice",
        "PUT",
        &format!("/messaging/channels/{id}/archive"),
        json!({"archived": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/channels/{id}/messages"),
        json!({"blocks": text("アーカイブ後")}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        "sid-alice",
        "GET",
        "/messaging/channels/browse?archived=true",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // --- 組織の設定（組織管理者）と、監査担当者の閲覧申請・承認・閲覧・エクスポート ---
    let (s, _) = call(
        &app,
        "sid-bob",
        "GET",
        "/org-admin/messaging/settings",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, mut settings) = call(
        &app,
        "sid-alice",
        "GET",
        "/org-admin/messaging/settings",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{settings}");
    settings["access_approvers"] = json!([alice]);
    let (s, saved) = call(
        &app,
        "sid-alice",
        "PUT",
        "/org-admin/messaging/settings",
        settings.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    let (s, _) = call(
        &app,
        "sid-alice",
        "PUT",
        "/org-admin/messaging/settings",
        settings,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "古い rev では保存できない");

    let (s, channels) = call(
        &app,
        "sid-carol",
        "GET",
        "/org-audit/messaging/channels?q=",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{channels}");
    assert!(channels
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == dm["id"]));
    let from = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let until =
        (Utc::now() + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let range = format!("from={from}&until={until}");
    let (s, _) = call(
        &app,
        "sid-carol",
        "GET",
        &format!("/org-audit/messaging/channels/{dm_id}/messages?{range}"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "承認なしでは DM を読めない");
    let (s, grant) = call(
        &app,
        "sid-carol",
        "POST",
        "/org-audit/messaging/access-requests",
        json!({"channel_id": dm_id, "reason": "調査", "messages_from": from, "messages_until": until}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{grant}");
    let gid = grant["id"].as_str().unwrap().to_string();
    let (s, pending) = call(
        &app,
        "sid-alice",
        "GET",
        "/messaging/access-requests",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(pending
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["id"] == grant["id"]));
    let (s, _) = call(
        &app,
        "sid-bob",
        "POST",
        &format!("/messaging/access-requests/{gid}/decision"),
        json!({"approve": true}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "承認者でなければ決められない");
    let (s, decided) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/access-requests/{gid}/decision"),
        json!({"approve": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{decided}");
    let (s, read) = call(
        &app,
        "sid-carol",
        "GET",
        &format!("/org-audit/messaging/channels/{dm_id}/messages?{range}"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{read}");
    assert_eq!(read.as_array().unwrap().len(), 1);
    let (s, export) = send(
        &app,
        "sid-carol",
        "GET",
        &format!("/org-audit/messaging/channels/{dm_id}/export?{range}"),
        "application/json",
        Vec::new(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8_lossy(&export).contains("dm secret"));
    let (s, _) = call(
        &app,
        "sid-alice",
        "POST",
        &format!("/messaging/access-requests/{gid}/revoke"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "sid-carol",
        "GET",
        &format!("/org-audit/messaging/channels/{dm_id}/messages?{range}"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "取り消した後は読めない");
}
