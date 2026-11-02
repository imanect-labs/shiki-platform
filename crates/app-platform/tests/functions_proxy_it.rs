//! ミニアプリの関数がゲートウェイへ委譲する HTTP（ループバック）は、環境のプロキシを使わない（BUG-209）。
//!
//! `HTTP_PROXY` を設定して `NO_PROXY` にループバックを書いていない環境でも、委譲はゲートウェイへ直に届き、利用者の
//! Bearer と本文がプロキシへ渡らない。実 QuickJS-in-wasmtime の関数から `Shiki.data.listTables()` を呼んで確かめる。
//!
//! 環境変数を書き換えるので、ほかのテストと同じ実行ファイルに入れない（この 1 本だけの実行ファイル）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use app_platform::{FunctionActor, FunctionInvocation, FunctionRunner};
use axum::{extract::State, routing::get, Json, Router};
use script_runtime::ScriptEngine;
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

#[path = "support/mem_store.rs"]
mod mem_store;
use mem_store::MemStore;

const SCRIPT: &str = "
function main(input) {
  return { tables: Shiki.data.listTables() };
}
";

/// 届いた Authorization を記録するゲートウェイ。
async fn tables(
    State(seen): State<Arc<Mutex<Vec<String>>>>,
    headers: axum::http::HeaderMap,
) -> Json<serde_json::Value> {
    seen.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    Json(serde_json::json!([{ "id": "t-1", "name": "expense" }]))
}

/// 受けた接続を数えて 502 を返すプロキシ。
async fn proxy(count: Arc<AtomicUsize>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await;
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_calls_do_not_go_through_the_environment_proxy() {
    let proxied = Arc::new(AtomicUsize::new(0));
    let proxy_url = proxy(Arc::clone(&proxied)).await;
    // 環境のプロキシ（ループバックを除外していない）。クライアントを作る前に設定する。
    for key in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        std::env::set_var(key, &proxy_url);
    }
    for key in ["NO_PROXY", "no_proxy"] {
        std::env::remove_var(key);
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/gw/data/tables", get(tables))
        .with_state(Arc::clone(&seen));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tenant = format!("t-{}", Uuid::new_v4());
    let sha = sha256_hex(SCRIPT.as_bytes());
    let store = Arc::new(MemStore::default());
    store.0.lock().unwrap().insert(
        miniapp_bundle_key(&tenant, &sha),
        SCRIPT.as_bytes().to_vec(),
    );
    let engine = Arc::new(ScriptEngine::new().expect("engine"));
    let runner = FunctionRunner::new(engine, store, format!("http://{addr}")).unwrap();
    let outcome = runner
        .run(
            &sha,
            FunctionInvocation {
                tenant_id: tenant,
                app_id: Uuid::new_v4(),
                function: "list".into(),
                payload: serde_json::json!({}),
                bearer: "user-bearer".into(),
                actor: FunctionActor::User,
                egress_allowlist: vec![],
            },
        )
        .await
        .expect("run");

    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.value["tables"][0]["name"], "expense");
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec!["Bearer user-bearer".to_string()],
        "委譲はゲートウェイへ直に届く"
    );
    assert_eq!(
        proxied.load(Ordering::SeqCst),
        0,
        "利用者の Bearer と本文をプロキシへ渡さない"
    );
}
