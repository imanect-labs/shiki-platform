//! Qdrant VectorStore の結合テスト（Task 2.4 受入条件・実 Qdrant が必要）。
//!
//! `RAG_TEST_QDRANT_URL` が設定されている時のみ実行し、未設定なら early-return で
//! スキップする（素の `cargo test` を壊さない）。CI の coverage ジョブで実走する。
//!
//! 検証: authz_tags フィルタ付き検索・**別テナント絶対遮断（タグ改竄でも tenant
//! フィルタ単独で遮断）**・削除でベクタが消える・move のタグ再書込・旧版掃除。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::sync::OnceLock;

use authz::{AuthContext, Principal};
use rag::vector_store::{ChunkPoint, PreFilter, VectorSearch, VectorStore};
use rag::QdrantVectorStore;
use uuid::Uuid;

const DIM: usize = 4;

/// alias（rag_chunks_active）は Qdrant インスタンス内で共有のため、テスト間で
/// 張り替え競合しないよう直列化する（tenant はテストごとに一意で汚染はしない）。
fn serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

async fn setup() -> Option<QdrantVectorStore> {
    let Ok(url) = std::env::var("RAG_TEST_QDRANT_URL") else {
        eprintln!("RAG_TEST_QDRANT_URL 未設定のためスキップ");
        return None;
    };
    // テストごとに独立した collection（モデル版に乱数を混ぜる）。alias は共有だが、
    // ensure_ready が毎回自 collection へ張り替えるため直近の store が有効になる。
    let store = QdrantVectorStore::new(
        reqwest::Client::new(),
        &url,
        &format!("test-model-{}", Uuid::new_v4()),
    );
    store.ensure_ready(DIM).await.unwrap();
    Some(store)
}

fn unit(v: [f32; DIM]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter().map(|x| x / norm).collect()
}

fn point(ctx: &AuthContext, node: Uuid, ord: u8, vector: [f32; DIM], folder: &str) -> ChunkPoint {
    ChunkPoint {
        chunk_id: Uuid::new_v5(&node, &[ord]),
        node_id: node,
        version: 1,
        vector: unit(vector),
        authz_tags: vec![
            ctx.ns().file(&node.to_string()).as_str().to_string(),
            ctx.ns().folder(folder).as_str().to_string(),
        ],
    }
}

async fn search(
    store: &QdrantVectorStore,
    ctx: &AuthContext,
    vector: [f32; DIM],
    prefilter: &PreFilter,
) -> Vec<Uuid> {
    store
        .search(
            ctx,
            &VectorSearch {
                vector: &unit(vector),
                limit: 10,
                prefilter,
                scope_tags: &[],
                exclude: &[],
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect()
}

#[tokio::test]
async fn tags_prefilter_narrows_and_tenant_filter_blocks_forged_tags() {
    let _guard = serial_lock().lock().await;
    let Some(store) = setup().await else { return };
    let a = ctx(&format!("a-{}", Uuid::new_v4()));
    let b = ctx(&format!("b-{}", Uuid::new_v4()));
    let node_sales = Uuid::new_v4();
    let node_hr = Uuid::new_v4();
    let p_sales = point(&a, node_sales, 0, [1.0, 0.0, 0.0, 0.0], "folder-sales");
    let p_hr = point(&a, node_hr, 0, [0.9, 0.1, 0.0, 0.0], "folder-hr");
    let sales_tags = p_sales.authz_tags.clone();
    store.upsert(&a, &[p_sales, p_hr]).await.unwrap();

    // authz_tags フィルタ: folder-sales の可読タグのみ → sales だけヒット（受入条件）。
    let readable = vec![a.ns().folder("folder-sales").as_str().to_string()];
    let hits = search(&store, &a, [1.0, 0.0, 0.0, 0.0], &PreFilter::Tags(readable)).await;
    assert_eq!(hits, vec![Uuid::new_v5(&node_sales, &[0])]);

    // 可読タグ空 = ヒットゼロ（fail-closed）。
    let hits = search(&store, &a, [1.0, 0.0, 0.0, 0.0], &PreFilter::Tags(vec![])).await;
    assert!(hits.is_empty());

    // 【受入条件】別テナントのクエリでは、a-corp のタグを丸ごと「改竄」して載せても
    // 絶対に返らない（tenant_id 無条件 AND が authz_tags と独立に効く）。
    let hits = search(
        &store,
        &b,
        [1.0, 0.0, 0.0, 0.0],
        &PreFilter::Tags(sales_tags),
    )
    .await;
    assert!(hits.is_empty());
    let hits = search(&store, &b, [1.0, 0.0, 0.0, 0.0], &PreFilter::TenantOnly).await;
    assert!(hits.is_empty());
}

#[tokio::test]
async fn delete_node_removes_vectors() {
    let _guard = serial_lock().lock().await;
    let Some(store) = setup().await else { return };
    let a = ctx(&format!("a-{}", Uuid::new_v4()));
    let node = Uuid::new_v4();
    store
        .upsert(&a, &[point(&a, node, 0, [0.0, 1.0, 0.0, 0.0], "root")])
        .await
        .unwrap();
    assert_eq!(
        search(&store, &a, [0.0, 1.0, 0.0, 0.0], &PreFilter::TenantOnly)
            .await
            .len(),
        1
    );

    store.delete_node(&a, node).await.unwrap();
    assert!(
        search(&store, &a, [0.0, 1.0, 0.0, 0.0], &PreFilter::TenantOnly)
            .await
            .is_empty(),
        "doc 削除でベクタも消える（受入条件）"
    );
}

#[tokio::test]
async fn set_authz_tags_reevaluates_move_without_reembedding() {
    let _guard = serial_lock().lock().await;
    let Some(store) = setup().await else { return };
    let a = ctx(&format!("a-{}", Uuid::new_v4()));
    let node = Uuid::new_v4();
    store
        .upsert(
            &a,
            &[point(&a, node, 0, [0.0, 0.0, 1.0, 0.0], "folder-old")],
        )
        .await
        .unwrap();

    // move: タグ再書込のみ（ベクタ再計算なし）。
    let new_tags = vec![
        a.ns().file(&node.to_string()).as_str().to_string(),
        a.ns().folder("folder-new").as_str().to_string(),
    ];
    store.set_authz_tags(&a, node, &new_tags).await.unwrap();

    let old_readable = vec![a.ns().folder("folder-old").as_str().to_string()];
    let new_readable = vec![a.ns().folder("folder-new").as_str().to_string()];
    assert!(search(
        &store,
        &a,
        [0.0, 0.0, 1.0, 0.0],
        &PreFilter::Tags(old_readable)
    )
    .await
    .is_empty());
    assert_eq!(
        search(
            &store,
            &a,
            [0.0, 0.0, 1.0, 0.0],
            &PreFilter::Tags(new_readable)
        )
        .await
        .len(),
        1
    );
}

#[tokio::test]
async fn stale_versions_are_cleaned_and_purge_tenant_wipes_all() {
    let _guard = serial_lock().lock().await;
    let Some(store) = setup().await else { return };
    let a = ctx(&format!("a-{}", Uuid::new_v4()));
    let node = Uuid::new_v4();
    // v1 のベクタ（chunk_id は版込みで採番されるため v2 と衝突しない想定を模す）。
    let mut v1 = point(&a, node, 0, [0.0, 0.0, 0.0, 1.0], "root");
    v1.chunk_id = Uuid::new_v5(&node, b"v1");
    let mut v2 = point(&a, node, 1, [0.0, 0.0, 0.0, 1.0], "root");
    v2.chunk_id = Uuid::new_v5(&node, b"v2");
    v2.version = 2;
    store.upsert(&a, &[v1, v2]).await.unwrap();

    store.delete_stale_versions(&a, node, 2).await.unwrap();
    let hits = search(&store, &a, [0.0, 0.0, 0.0, 1.0], &PreFilter::TenantOnly).await;
    assert_eq!(hits, vec![Uuid::new_v5(&node, b"v2")], "旧版のみ消える");

    store.purge_tenant(&a.tenant_id).await.unwrap();
    assert!(
        search(&store, &a, [0.0, 0.0, 0.0, 1.0], &PreFilter::TenantOnly)
            .await
            .is_empty(),
        "テナント消去で全ベクタが消える"
    );
}
