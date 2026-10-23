//! 内部サービス（Qdrant・ingestion-worker）への認証ヘッダの結線テスト（#812）。
//!
//! dev-dep の axum でスタブサーバを立て、全リクエストのヘッダを記録して検証する。
//! 外部依存なしで常時実行される。
//!
//! 検証: Qdrant の全経路に `api-key` が付く／未設定なら付かない／worker の全経路
//! （parse・embed・rerank）に `Authorization: Bearer` が付く／worker のトークンと
//! Qdrant のキーが互いのサービスへ漏れない／ヘッダに載らない値は起動時に落ちる。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use std::sync::{Arc, Mutex};

use authz::{AuthContext, Principal};
use axum::extract::Request;
use axum::response::IntoResponse;
use axum::Json;
use axum::Router;
use rag::embedding::EmbeddingProvider;
use rag::parser::{DocumentParser, ParseRequest, ParseSource};
use rag::rerank::{RerankPassage, Reranker};
use rag::vector_store::{ChunkPoint, PreFilter, VectorSearch, VectorStore};
use rag::{
    worker_auth_headers, EmbedInput, HttpDocumentParser, HttpEmbeddingProvider, HttpReranker,
    QdrantVectorStore, RagError,
};

fn ctx() -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("a-corp".into()),
        },
        "acme".into(),
        "a-corp".into(),
    )
}

/// 1 リクエストの記録: (メソッド, パス, api-key, authorization)。
type Seen = Arc<Mutex<Vec<(String, String, Option<String>, Option<String>)>>>;

/// 全パスを受けてヘッダを記録するスタブ。応答はどの呼び出し口でも解釈できる形にする
/// （Qdrant の存在確認・worker の parse/embed/rerank の各 DTO を 1 つの JSON に同居させる）。
async fn serve_recording() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    let router = Router::new().fallback(move |req: Request| {
        let s = Arc::clone(&s);
        async move {
            let header = |name: &str| {
                req.headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            s.lock().unwrap().push((
                req.method().to_string(),
                req.uri().path().to_string(),
                header("api-key"),
                header("authorization"),
            ));
            Json(serde_json::json!({
                "status": "ok",
                "result": {"exists": true},
                "blocks": [],
                "used_ocr": false,
                "vectors": [[0.1, 0.2]],
                "model_version": "m",
                "dimension": 2,
                "scores": [{"id": "p1", "score": 0.5}],
            }))
            .into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

/// ensure_ready（GET/POST）・upsert（PUT）・検索・削除・タグ書換・テナント消去を一巡させる。
async fn exercise_qdrant(store: &QdrantVectorStore) {
    let ctx = ctx();
    let node = uuid::Uuid::new_v4();
    store.ensure_ready(2).await.unwrap();
    store
        .upsert(
            &ctx,
            &[ChunkPoint {
                chunk_id: uuid::Uuid::new_v4(),
                node_id: node,
                version: 1,
                vector: vec![0.1, 0.2],
                authz_tags: vec!["file:a-corp|x".into()],
            }],
        )
        .await
        .unwrap();
    store
        .search(
            &ctx,
            &VectorSearch {
                vector: &[0.1, 0.2],
                limit: 3,
                prefilter: &PreFilter::TenantOnly,
                scope_tags: &[],
                exclude: &[],
            },
        )
        .await
        .unwrap();
    store.delete_node(&ctx, node).await.unwrap();
    store.delete_stale_versions(&ctx, node, 1).await.unwrap();
    store
        .set_authz_tags(&ctx, node, &["file:a-corp|x".into()])
        .await
        .unwrap();
    store.purge_tenant("a-corp").await.unwrap();
}

#[tokio::test]
async fn qdrant_sends_api_key_on_every_request() {
    let (base, seen) = serve_recording().await;
    let store = QdrantVectorStore::new(reqwest::Client::new(), &base, "m")
        .with_api_key(Some("qdrant-key"))
        .unwrap();
    assert!(store.has_api_key());
    exercise_qdrant(&store).await;

    let seen = seen.lock().unwrap().clone();
    // GET（存在確認）・PUT（upsert）・POST（alias/検索/削除）を全て通っている。
    for method in ["GET", "PUT", "POST"] {
        assert!(
            seen.iter().any(|(m, ..)| m == method),
            "{method} を通っていない: {seen:?}"
        );
    }
    assert!(
        seen.iter()
            .all(|(_, _, key, _)| key.as_deref() == Some("qdrant-key")),
        "api-key の付いていない Qdrant リクエストがある: {seen:?}"
    );
    // Qdrant へ Authorization を送らない（worker のトークンの混入経路を持たない）。
    assert!(seen.iter().all(|(.., auth)| auth.is_none()), "{seen:?}");
}

#[tokio::test]
async fn qdrant_without_api_key_sends_no_key_header() {
    let (base, seen) = serve_recording().await;
    // 空文字は未設定扱い（compose が未設定の env を空文字で渡す形）。
    for key in [None, Some("")] {
        let store = QdrantVectorStore::new(reqwest::Client::new(), &base, "m")
            .with_api_key(key)
            .unwrap();
        assert!(!store.has_api_key());
        exercise_qdrant(&store).await;
    }
    let seen = seen.lock().unwrap().clone();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|(_, _, key, _)| key.is_none()), "{seen:?}");
}

#[test]
fn qdrant_rejects_key_that_cannot_be_a_header() {
    let err = QdrantVectorStore::new(reqwest::Client::new(), "http://q", "m")
        .with_api_key(Some("bad\nkey"))
        .err()
        .expect("改行を含むキーは起動時に落とす");
    assert!(matches!(err, RagError::Config(_)), "{err:?}");
    assert!(!err.is_transient());
}

fn worker_client(token: Option<&str>) -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(worker_auth_headers(token).unwrap())
        .build()
        .unwrap()
}

/// parse・embed・rerank を 1 回ずつ呼ぶ。
async fn exercise_worker(http: reqwest::Client, base: &str) {
    let ctx = ctx();
    HttpDocumentParser::new(http.clone(), base)
        .parse(
            &ctx,
            ParseRequest {
                source: ParseSource::Bytes(b"x"),
                content_type: "text/plain",
                file_name: "a.txt",
            },
        )
        .await
        .unwrap();
    HttpEmbeddingProvider::new(http.clone(), base, "m")
        .embed(&ctx, EmbedInput::Query, &["q".to_string()])
        .await
        .unwrap();
    HttpReranker::new(http, base)
        .rerank(
            &ctx,
            "q",
            &[RerankPassage {
                id: "p1".into(),
                text: "t".into(),
            }],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn worker_clients_send_bearer_token_on_every_endpoint() {
    let (base, seen) = serve_recording().await;
    exercise_worker(worker_client(Some("worker-token")), &base).await;

    let seen = seen.lock().unwrap().clone();
    let paths: Vec<&str> = seen.iter().map(|(_, p, ..)| p.as_str()).collect();
    assert_eq!(paths, ["/parse", "/embed", "/rerank"]);
    assert!(
        seen.iter()
            .all(|(.., auth)| auth.as_deref() == Some("Bearer worker-token")),
        "Bearer の付いていない worker リクエストがある: {seen:?}"
    );
    // worker へ Qdrant のキーを送らない。
    assert!(seen.iter().all(|(_, _, key, _)| key.is_none()), "{seen:?}");
}

#[tokio::test]
async fn worker_clients_without_token_send_no_authorization() {
    let (base, seen) = serve_recording().await;
    exercise_worker(worker_client(None), &base).await;
    exercise_worker(worker_client(Some("")), &base).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 6);
    assert!(seen.iter().all(|(.., auth)| auth.is_none()), "{seen:?}");
}

#[test]
fn worker_token_that_cannot_be_a_header_fails_at_startup() {
    let err = worker_auth_headers(Some("bad\rtoken")).unwrap_err();
    assert!(matches!(err, RagError::Config(_)), "{err:?}");
    // 値はログ・Debug に出ないよう sensitive 印を付ける。
    let headers = worker_auth_headers(Some("t")).unwrap();
    assert!(headers[reqwest::header::AUTHORIZATION].is_sensitive());
}

#[tokio::test]
async fn worker_rejection_surfaces_as_retryable_worker_error() {
    // トークン不一致（worker の 401）は握りつぶさず worker エラーとして上げる。
    let router = Router::new().fallback(|| async {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"detail": "unauthorized"})),
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let err =
        HttpEmbeddingProvider::new(worker_client(Some("wrong")), &format!("http://{addr}"), "m")
            .embed(&ctx(), EmbedInput::Query, &["q".to_string()])
            .await
            .unwrap_err();
    match err {
        RagError::Worker(msg) => assert!(msg.contains("401"), "{msg}"),
        other => panic!("Worker エラーであるべき: {other:?}"),
    }
}
