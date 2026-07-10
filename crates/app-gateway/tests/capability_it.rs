//! 能力アダプタの結合テスト（Task 9.8 受け入れ条件）。
//!
//! 能力ごとの行列を検証する:
//! - **scope なし** → 403（二重ゲートのスコープ突合）
//! - **scope のみ**（per-call FGA 否認）→ 403/404（第4ゲート＝ストア内 ReBAC）
//! - **scope ＋ FGA 可・ただし非所有テーブル** → 403（data.\* のアプリ所有束縛）
//! - **両方可** → 200 ＋ `app_capability_usage` 計上 ＋ `audit_log` 記録
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）・authz/JWKS/ObjectStore/Rag はスタブ。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use app_gateway::{build_gateway_router, fetch_usage, AppInstallationStore, NewAppInstallation};
use axum::http::StatusCode;
use common::{ctx, get, request_json, setup, state, state_with, token, token_as, StubAuthz};
use data::{FieldDef, FieldType, NewDataTable, TableSchema};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const ALL_SCOPES: &str = "data.read data.write data.schema storage.read storage.write \
                          rag.query identity.read events.subscribe notify.send";

fn field(name: &str, ty: FieldType) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type: ty,
        required: false,
        unique: false,
        indexed: false,
        options: vec![],
        ref_table: None,
        lookup: None,
        computed: None,
    }
}

fn simple_schema() -> TableSchema {
    let mut title = field("title", FieldType::Text);
    title.required = true;
    let amount = field("amount", FieldType::Number);
    TableSchema {
        fields: vec![title, amount],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
    }
}

/// インストール（全能力 granted）＋アプリ所有テーブル＋非所有テーブルを用意する。
async fn fixture(pool: &PgPool, tenant: &str) -> (Uuid, String, Uuid, Uuid) {
    let app_id = Uuid::new_v4();
    let client = format!("app-{}", Uuid::new_v4());
    AppInstallationStore::new(pool.clone())
        .upsert(
            &ctx(tenant),
            NewAppInstallation {
                app_id,
                app_name: "経費",
                installed_version: "1.0.0",
                granted_scopes: &ALL_SCOPES
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                client_id_b1: Some(&client),
                client_id_b2: None,
            },
        )
        .await
        .expect("install");

    // テーブル 2 本（所有/非所有）。app_id の束縛は 9.13b プロビジョンの領分のため、
    // ここではテスト fixture として直接設定する。
    let store = data::DataStore::new(
        pool.clone(),
        Arc::new(StubAuthz::allow_all()),
        Arc::new(common::FixedResolver),
    );
    let owned = store
        .create_table(
            &ctx(tenant),
            NewDataTable {
                name: "owned".into(),
                schema: simple_schema(),
            },
            None,
        )
        .await
        .expect("owned table");
    let foreign = store
        .create_table(
            &ctx(tenant),
            NewDataTable {
                name: "foreign".into(),
                schema: simple_schema(),
            },
            None,
        )
        .await
        .expect("foreign table");
    sqlx::query("UPDATE data_table SET app_id = $1 WHERE id = $2")
        .bind(app_id)
        .bind(owned.id)
        .execute(pool)
        .await
        .expect("bind app");
    (app_id, client, owned.id, foreign.id)
}

#[tokio::test]
async fn data_capability_matrix() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (app_id, client, owned, foreign) = fixture(&pool, &tenant).await;
    let app = build_gateway_router(state(pool.clone(), &tenant));

    // scope なし → 403（granted にあってもトークンに無い）。
    let no_scope = token(&client, "openid", &tenant);
    let (s, _) = get(
        &app,
        &format!("/gw/data/tables/{owned}/records"),
        Some(&no_scope),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let tok = token(&client, ALL_SCOPES, &tenant);

    // 非所有テーブル → scope があっても 403（アプリ所有束縛）。
    let (s, body) = request_json(
        &app,
        "POST",
        &format!("/gw/data/tables/{foreign}/records"),
        Some(&tok),
        &json!({ "data": { "title": "x" } }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");

    // 所有テーブル → create/get/list/query/update/delete の一気通貫。
    let (s, created) = request_json(
        &app,
        "POST",
        &format!("/gw/data/tables/{owned}/records"),
        Some(&tok),
        &json!({ "data": { "title": "経費A", "amount": 1200 } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{created}");
    let rec_id = created["id"].as_str().expect("id");

    let (s, got) = get(
        &app,
        &format!("/gw/data/tables/{owned}/records/{rec_id}"),
        Some(&tok),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{got}");
    assert_eq!(got["data"]["title"], "経費A");

    let (s, listed) = get(
        &app,
        &format!("/gw/data/tables/{owned}/records?limit=10"),
        Some(&tok),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(listed["items"].as_array().map(Vec::len), Some(1));

    let (s, q) = request_json(
        &app,
        "POST",
        &format!("/gw/data/tables/{owned}/query"),
        Some(&tok),
        &json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{q}");

    let (s, updated) = request_json(
        &app,
        "PATCH",
        &format!("/gw/data/tables/{owned}/records/{rec_id}"),
        Some(&tok),
        &json!({ "patch": { "amount": 1500 }, "expected_rev": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{updated}");
    assert_eq!(updated["rev"], 2);

    // 楽観ロック不一致 → 409。
    let (s, _) = request_json(
        &app,
        "PATCH",
        &format!("/gw/data/tables/{owned}/records/{rec_id}"),
        Some(&tok),
        &json!({ "patch": { "amount": 1 }, "expected_rev": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // テーブル一覧はアプリ所有分のみ…だが FGA スタブの list_objects は空 → 空配列で 200。
    let (s, tables) = get(&app, "/gw/data/tables", Some(&tok)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(tables.as_array().is_some());

    // schema 参照（data.schema スコープ）。
    let (s, schema) = get(&app, &format!("/gw/data/tables/{owned}/schema"), Some(&tok)).await;
    assert_eq!(s, StatusCode::OK, "{schema}");
    assert_eq!(schema["id"], owned.to_string());

    // 利用量: data.read / data.write / data.schema が (alice×app) で計上されている。
    let usage = fetch_usage(&pool, &tenant, app_id, 100)
        .await
        .expect("usage");
    let calls = |cap: &str| {
        usage
            .iter()
            .filter(|u| u.capability == cap && u.user_sub == "alice")
            .map(|u| u.calls)
            .sum::<i64>()
    };
    assert!(calls("data.write") >= 2, "usage: {usage:?}");
    assert!(calls("data.read") >= 3, "usage: {usage:?}");
    assert!(calls("data.schema") >= 1, "usage: {usage:?}");

    // 監査: gateway.call の Allow が残っている。
    let (audits,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'gateway.call' AND object_id = $2",
    )
    .bind(&tenant)
    .bind(app_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("audit");
    assert!(audits >= 5, "audit rows: {audits}");

    // FGA 否認（deny_all）→ scope があっても第4ゲートで拒否（403/404 のどちらも権限系）。
    let deny = build_gateway_router(state_with(
        pool.clone(),
        &tenant,
        Arc::new(StubAuthz::deny_all()),
    ));
    let (s, _) = get(
        &deny,
        &format!("/gw/data/tables/{owned}/records/{rec_id}"),
        Some(&tok),
    )
    .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "FGA 否認は権限エラー: {s}"
    );

    // delete（楽観ロック一致）→ 200。
    let (s, _) = request_json(
        &app,
        "DELETE",
        &format!("/gw/data/tables/{owned}/records/{rec_id}?expected_rev=2"),
        Some(&tok),
        &json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn storage_rag_identity_notify_matrix() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (app_id, client, _owned, _foreign) = fixture(&pool, &tenant).await;

    // identity.read: 直接ロールが最小 DTO で返る（authz スタブが 2 ロールを返す）。
    let authz = Arc::new(StubAuthz {
        roles: vec![
            format!("role:{tenant}|sales"),
            format!("role:{tenant}|dev"),
            "role:other-tenant|leak".into(), // 他テナントは strip 失敗で除外される
        ],
        ..StubAuthz::default()
    });
    let app = build_gateway_router(state_with(pool.clone(), &tenant, authz));
    let tok = token(&client, ALL_SCOPES, &tenant);

    let (s, me) = get(&app, "/gw/identity/me", Some(&tok)).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    assert_eq!(me["user_sub"], "alice");
    assert_eq!(me["roles"], json!(["dev", "sales"]));

    // storage.write: フォルダ作成（ルート直下・org メンバー扱い）→ 200。
    let (s, folder) = request_json(
        &app,
        "POST",
        "/gw/storage/folders",
        Some(&tok),
        &json!({ "parent_id": null, "name": "アプリ出力" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{folder}");
    let folder_id = folder["id"].as_str().expect("folder id");

    // storage.read: メタデータ＋子一覧。
    let (s, meta) = get(&app, &format!("/gw/storage/nodes/{folder_id}"), Some(&tok)).await;
    assert_eq!(s, StatusCode::OK, "{meta}");
    assert_eq!(meta["kind"], "folder");
    let (s, children) = get(
        &app,
        &format!("/gw/storage/nodes/{folder_id}/children"),
        Some(&tok),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{children}");

    // storage スコープ無しトークン → 403。
    let narrow = token(&client, "data.read", &tenant);
    let (s, _) = get(
        &app,
        &format!("/gw/storage/nodes/{folder_id}"),
        Some(&narrow),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // rag.query: スタブ port が 1 ヒットを返す（実 post-filter は SearchService 側の IT）。
    let (s, hits) = request_json(
        &app,
        "POST",
        "/gw/rag/query",
        Some(&tok),
        &json!({ "query": "経費 規程" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{hits}");
    assert_eq!(hits["hits"].as_array().map(Vec::len), Some(1));
    // 空クエリは 400。
    let (s, _) = request_json(
        &app,
        "POST",
        "/gw/rag/query",
        Some(&tok),
        &json!({ "query": " " }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // notify.send: 台帳へ記録される（bob 宛）。
    let (s, sent) = request_json(
        &app,
        "POST",
        "/gw/notify/send",
        Some(&tok),
        &json!({ "recipient": "bob", "title": "承認依頼", "body": "経費Aを確認してください" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sent}");
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM app_notification \
         WHERE tenant_id = $1 AND app_id = $2 AND recipient = 'bob' AND created_by = 'alice'",
    )
    .bind(&tenant)
    .bind(app_id)
    .fetch_one(&pool)
    .await
    .expect("notification");
    assert_eq!(count, 1);
    // 不正入力（title 空）→ 400。
    let (s, _) = request_json(
        &app,
        "POST",
        "/gw/notify/send",
        Some(&tok),
        &json!({ "recipient": "bob", "title": "  " }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // 利用量: user_sub 別に計上される（bob のトークンで identity.me → bob の行が増える）。
    let bob = token_as(&client, ALL_SCOPES, &tenant, "bob");
    let (s, _) = get(&app, "/gw/identity/me", Some(&bob)).await;
    assert_eq!(s, StatusCode::OK);
    let usage = fetch_usage(&pool, &tenant, app_id, 100)
        .await
        .expect("usage");
    assert!(
        usage
            .iter()
            .any(|u| u.capability == "identity.read" && u.user_sub == "alice"),
        "{usage:?}"
    );
    assert!(
        usage
            .iter()
            .any(|u| u.capability == "identity.read" && u.user_sub == "bob"),
        "{usage:?}"
    );
    assert!(
        usage
            .iter()
            .any(|u| u.capability == "notify.send" && u.user_sub == "alice"),
        "{usage:?}"
    );
}

#[tokio::test]
async fn events_subscribe_streams_owned_table_events() {
    use tower::ServiceExt;

    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (_app_id, client, owned, foreign) = fixture(&pool, &tenant).await;
    // 購読束縛（アプリ所有∩ユーザー可視）は list_objects 由来のため、
    // スタブに所有/非所有テーブル両方を可視として持たせる（所有束縛の効きを見る）。
    let authz = Arc::new(StubAuthz {
        objects: vec![
            format!("data_table:{tenant}|{owned}"),
            format!("data_table:{tenant}|{foreign}"),
        ],
        ..StubAuthz::default()
    });
    let app = build_gateway_router(state_with(pool.clone(), &tenant, authz));
    let tok = token(&client, ALL_SCOPES, &tenant);

    // scope 無し → 403。
    let narrow = token(&client, "data.read", &tenant);
    let req = axum::http::Request::builder()
        .uri("/gw/events/subscribe")
        .header("authorization", format!("Bearer {narrow}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 接続 → SSE 応答ヘッダ。購読開始後に所有テーブルのドメインイベントを outbox へ発行し、
    // 最初のフレームに届くことを検証する（ライブテール）。
    let req = axum::http::Request::builder()
        .uri("/gw/events/subscribe")
        .header("authorization", format!("Bearer {tok}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );

    // 除外系を**先に**発行してから所有イベントを発行する（id 順に配信されるため、もし除外が
    // 漏れていれば所有イベントより先に届く＝最初のフレーム検証で検出できる）。
    // ①非所有・ユーザー可視の foreign（アプリ所有束縛で除外されるべき）
    // ②存在しないテーブル（ReBAC 不可視相当・除外されるべき）
    // ③所有テーブル owned（これだけが届く）
    let c = ctx(&tenant);
    let mut tx = pool.begin().await.unwrap();
    for table_id in [foreign, Uuid::new_v4(), owned] {
        storage::event::emit_on(
            &mut tx,
            &c,
            storage::event::WriteEvent {
                node_id: Uuid::new_v4(),
                version: 1,
                op: storage::event::WriteOp::Update,
                payload: json!({
                    "event_type": "data.record.transitioned",
                    "table_id": table_id,
                    "to": "submitted",
                }),
            },
            None,
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    // 最初のデータフレームを最大 10 秒待つ（ポーリング間隔 1s）。
    let mut body = resp.into_body().into_data_stream();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        use futures::StreamExt;
        loop {
            match body.next().await {
                Some(Ok(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes).to_string();
                    if text.contains("data.record.transitioned") {
                        return text;
                    }
                    // keep-alive コメント等はスキップ。
                }
                _ => panic!("SSE ストリームが終了した"),
            }
        }
    })
    .await
    .expect("SSE イベントがタイムアウト内に届く");
    // 最初に届いた遷移イベントが owned（先に発行した foreign/不可視が漏れていない）。
    assert!(frame.contains(&owned.to_string()), "{frame}");
    assert!(!frame.contains(&foreign.to_string()), "{frame}");
}
