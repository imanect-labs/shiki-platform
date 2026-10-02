//! HTTP クライアント実装（parser / embedding / rerank）の結線テスト。
//!
//! dev-dep の axum でローカルスタブサーバを立て、実 reqwest 経路（DTO 直列化・
//! エラーマッピング・版突合ガード）を検証する。外部依存なしで常時実行される。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use authz::{AuthContext, Principal};
use axum::extract::Json;
use axum::routing::post;
use axum::Router;
use rag::embedding::EmbeddingProvider;
use rag::parser::{DocumentParser, ParseRequest, ParseSource};
use rag::rerank::{RerankPassage, Reranker};
use rag::types::BlockType;
use rag::{EmbedInput, HttpDocumentParser, HttpEmbeddingProvider, HttpReranker, RagError};

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

/// スタブサーバを起動してベース URL を返す。
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn parser_maps_blocks_and_sends_tenant() {
    let router = Router::new().route(
        "/parse",
        post(|Json(body): Json<serde_json::Value>| async move {
            // tenant_id が必須フィールドとして届いていること（design §4.3）。
            assert_eq!(body["tenant_id"], "a-corp");
            assert_eq!(body["file_name"], "report.pdf");
            Json(serde_json::json!({
                "blocks": [
                    {"type": "heading", "level": 1, "text": "章", "page": 1},
                    {"type": "table", "text": "| a |", "page": 2}
                ],
                "used_ocr": true
            }))
        }),
    );
    let base = serve(router).await;
    let parser = HttpDocumentParser::new(reqwest::Client::new(), &base);
    let doc = parser
        .parse(
            &ctx(),
            ParseRequest {
                source: ParseSource::Url("http://minio:9000/blob"),
                content_type: "application/pdf",
                file_name: "report.pdf",
            },
        )
        .await
        .unwrap();
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(doc.blocks[0].block_type, BlockType::Heading);
    assert_eq!(doc.blocks[1].block_type, BlockType::Table);
    assert!(doc.used_ocr);
}

#[tokio::test]
async fn parser_maps_422_to_permanent_parse_error() {
    let router = Router::new().route(
        "/parse",
        post(|| async {
            (
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "detail": {"error": "parse_failed", "detail": "壊れた PDF"}
                })),
            )
        }),
    );
    let base = serve(router).await;
    let parser = HttpDocumentParser::new(reqwest::Client::new(), &base);
    let err = parser
        .parse(
            &ctx(),
            ParseRequest {
                source: ParseSource::Url("http://minio:9000/blob"),
                content_type: "application/pdf",
                file_name: "broken.pdf",
            },
        )
        .await
        .unwrap_err();
    match &err {
        RagError::Parse { code, detail } => {
            assert_eq!(code, "parse_failed");
            assert_eq!(detail, "壊れた PDF");
        }
        other => panic!("Parse エラーであるべき: {other:?}"),
    }
    assert!(!err.is_transient(), "パース失敗はリトライしない恒久エラー");
}

#[tokio::test]
async fn parser_maps_5xx_to_transient_worker_error() {
    let router = Router::new().route(
        "/parse",
        post(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
    );
    let base = serve(router).await;
    let parser = HttpDocumentParser::new(reqwest::Client::new(), &base);
    let err = parser
        .parse(
            &ctx(),
            ParseRequest {
                source: ParseSource::Url("http://minio:9000/blob"),
                content_type: "application/pdf",
                file_name: "a.pdf",
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, RagError::Worker(_)));
    assert!(err.is_transient(), "worker 5xx はリトライ対象");
}

#[tokio::test]
async fn embedding_batches_and_checks_model_version() {
    let router = Router::new().route(
        "/embed",
        post(|Json(body): Json<serde_json::Value>| async move {
            assert_eq!(body["tenant_id"], "a-corp");
            assert_eq!(body["input_type"], "document");
            let n = body["texts"].as_array().unwrap().len();
            Json(serde_json::json!({
                "vectors": vec![vec![0.6f32, 0.8f32]; n],
                "model_version": "cl-nagoya/ruri-v3-30m",
                "dimension": 2
            }))
        }),
    );
    let base = serve(router).await;
    let provider =
        HttpEmbeddingProvider::new(reqwest::Client::new(), &base, "cl-nagoya/ruri-v3-30m");
    // batch_size(64) を跨ぐ 100 件でバッチ分割経路も通す。
    let texts: Vec<String> = (0..100).map(|i| format!("文書{i}")).collect();
    let resp = provider
        .embed(&ctx(), EmbedInput::Document, &texts)
        .await
        .unwrap();
    assert_eq!(resp.vectors.len(), 100);
    assert_eq!(resp.dimension, 2);
    assert_eq!(provider.model_version(), "cl-nagoya/ruri-v3-30m");
}

#[tokio::test]
async fn embedding_rejects_model_version_mismatch() {
    let router = Router::new().route(
        "/embed",
        post(|| async {
            Json(serde_json::json!({
                "vectors": [[1.0f32]],
                "model_version": "other-model",
                "dimension": 1
            }))
        }),
    );
    let base = serve(router).await;
    let provider = HttpEmbeddingProvider::new(reqwest::Client::new(), &base, "expected-model");
    let err = provider
        .embed(&ctx(), EmbedInput::Query, &["q".into()])
        .await
        .unwrap_err();
    match &err {
        RagError::EmbeddingVersionMismatch { expected, actual } => {
            assert_eq!(expected, "expected-model");
            assert_eq!(actual, "other-model");
        }
        other => panic!("版不一致エラーであるべき: {other:?}"),
    }
    assert!(
        !err.is_transient(),
        "版不一致はリトライで直らない（設定修正か shadow index 移行が必要）"
    );
}

#[tokio::test]
async fn reranker_scores_passages_in_order() {
    let router = Router::new().route(
        "/rerank",
        post(|Json(body): Json<serde_json::Value>| async move {
            assert_eq!(body["tenant_id"], "a-corp");
            assert_eq!(body["query"], "経費");
            Json(serde_json::json!({
                "scores": [
                    {"id": "c1", "score": 0.9},
                    {"id": "c2", "score": 0.1}
                ],
                "model_version": "reranker-x"
            }))
        }),
    );
    let base = serve(router).await;
    let reranker = HttpReranker::new(reqwest::Client::new(), &base);
    let scores = reranker
        .rerank(
            &ctx(),
            "経費",
            &[
                RerankPassage {
                    id: "c1".into(),
                    text: "経費精算".into(),
                },
                RerankPassage {
                    id: "c2".into(),
                    text: "天気".into(),
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(scores.len(), 2);
    assert_eq!(scores[0].id, "c1");
    assert!(scores[0].score > scores[1].score);
}

#[tokio::test]
async fn reranker_short_circuits_on_empty_input() {
    // サーバ無しでも空入力は HTTP を呼ばず成功する。
    let reranker = HttpReranker::new(reqwest::Client::new(), "http://127.0.0.1:1");
    let scores = reranker.rerank(&ctx(), "q", &[]).await.unwrap();
    assert!(scores.is_empty());
}

/// Qdrant への upsert が点数で分割され、1 リクエストが上限を超えないこと。
///
/// 1 リクエストにまとめる実装では、1 文書のチャンク数が数万に達したときに
/// `serde_json::Value` の中間表現ごとボディが数百 MB になり、Qdrant の上限に
/// 当たるか、その前に OOM する（PIT-67）。実 Qdrant では再現に数万点が要るため、
/// ここではスタブサーバで**分割そのもの**を検証する。
#[tokio::test]
async fn qdrant_upsert_is_split_into_bounded_requests() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // 1 リクエストあたりの点数を記録する。
    let sizes: Arc<std::sync::Mutex<Vec<usize>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let (s, c) = (Arc::clone(&sizes), Arc::clone(&calls));
    let router = Router::new()
        .route(
            "/collections/rag_chunks_active/points",
            axum::routing::put(move |Json(body): Json<serde_json::Value>| {
                let (s, c) = (Arc::clone(&s), Arc::clone(&c));
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    s.lock()
                        .unwrap()
                        .push(body["points"].as_array().unwrap().len());
                    Json(serde_json::json!({"status": "ok"}))
                }
            }),
        )
        // ensure_ready が叩く経路（存在確認 → 作成 → alias）。
        .route(
            "/collections/{name}",
            axum::routing::get(|| async { Json(serde_json::json!({"status": "ok"})) }),
        );
    let base = serve(router).await;

    let store = rag::QdrantVectorStore::new(reqwest::Client::new(), &base, "test-model");
    let node = uuid::Uuid::new_v4();
    let points: Vec<rag::vector_store::ChunkPoint> = (0..600)
        .map(|i: usize| rag::vector_store::ChunkPoint {
            chunk_id: uuid::Uuid::new_v5(&node, &i.to_le_bytes()),
            node_id: node,
            version: 1,
            vector: vec![0.1, 0.2, 0.3, 0.4],
            authz_tags: vec!["file:a-corp|x".into()],
        })
        .collect();
    rag::vector_store::VectorStore::upsert(&store, &ctx(), &points)
        .await
        .unwrap();

    let sizes = sizes.lock().unwrap().clone();
    assert!(
        calls.load(Ordering::SeqCst) > 1,
        "分割されていない（1 リクエストで全点を送っている）: {sizes:?}"
    );
    assert_eq!(
        sizes.iter().sum::<usize>(),
        600,
        "点が欠けている: {sizes:?}"
    );
    // 実装の上限（vector_qdrant.rs の UPSERT_BATCH_POINTS）と同値にする。緩めると
    // 「分割はしているが 1 回が大きすぎる」実装を見逃す。
    const MAX_POINTS_PER_REQUEST: usize = 256;
    assert!(
        sizes.iter().all(|&n| n <= MAX_POINTS_PER_REQUEST),
        "1 リクエストの点数が上限を超えている: {sizes:?}"
    );
}
