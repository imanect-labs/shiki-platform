//! `rag_chunk` の差し替え（`store::replace_chunks`）の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行し、未設定なら early-return で
//! スキップする（CI の rust ジョブは compose 非依存のため）。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::print_stderr
)]

use authz::{AuthContext, Principal};
use rag::anchor::{Anchor, BoxOrigin, PageBox};
use rag::store;
use rag::types::{BlockType, Chunk, ChunkKind, ParsedBlock};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

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

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    Some(pool)
}

fn chunk(node: Uuid, ordinal: i32) -> Chunk {
    Chunk {
        id: Uuid::new_v5(&node, &ordinal.to_le_bytes()),
        parent_id: None,
        kind: ChunkKind::Leaf,
        ordinal,
        page: None,
        heading_path: vec!["章".into(), format!("節{ordinal}")],
        content: format!("本文 {ordinal}"),
        anchor: None,
        quote_prefix: String::new(),
        quote_suffix: String::new(),
        boxes: Vec::new(),
    }
}

/// `INSERT_BATCH_ROWS` をまたぐ件数でも全行が入り、再実行しても冪等であること。
///
/// 1 行 1 クエリで回す実装では、チャンク数が数万になったときに往復ぶんだけ
/// トランザクションが伸び、delete で取った行ロックを握り続ける（PIT-67）。
/// 分割後に端数の取りこぼしが出ないことを実 DB で確かめる。
#[tokio::test]
async fn replace_chunks_writes_every_row_across_batch_boundaries() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let ctx = ctx(&tenant);
    let node = Uuid::new_v4();

    // 既定のバッチ幅 1000 を明確にまたぐ件数（2 バッチ＋端数）。
    const N: i32 = 2500;
    let chunks: Vec<Chunk> = (0..N).map(|i| chunk(node, i)).collect();
    let tags = vec![format!("file:{tenant}|{node}")];

    store::replace_chunks(&pool, &ctx, node, 1, &chunks, &[], &tags, "test-model")
        .await
        .unwrap();

    let count: i64 =
        sqlx::query_scalar("select count(*) from rag_chunk where tenant_id = $1 and node_id = $2")
            .bind(&tenant)
            .bind(node)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, i64::from(N), "書き込まれなかった行がある");

    // 配列カラム（heading_path / authz_tags）が行ごとに正しく載っていること。
    let (heading, stored_tags): (Vec<String>, Vec<String>) = sqlx::query_as(
        "select heading_path, authz_tags from rag_chunk \
         where tenant_id = $1 and node_id = $2 and ordinal = $3",
    )
    .bind(&tenant)
    .bind(node)
    .bind(N - 1)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(heading, vec!["章".to_string(), format!("節{}", N - 1)]);
    assert_eq!(stored_tags, tags);

    // 決定的 ID なので再実行しても増えない（at-least-once 配信の冪等性）。
    store::replace_chunks(&pool, &ctx, node, 1, &chunks, &[], &tags, "test-model")
        .await
        .unwrap();
    let again: i64 =
        sqlx::query_scalar("select count(*) from rag_chunk where tenant_id = $1 and node_id = $2")
            .bind(&tenant)
            .bind(node)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(again, i64::from(N), "再実行で行数が変わった");

    sqlx::query("delete from rag_chunk where tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}

fn block(block_type: BlockType, text: &str) -> ParsedBlock {
    ParsedBlock {
        block_type,
        level: (block_type == BlockType::Heading).then_some(2),
        text: text.into(),
        page: Some(1),
        prov: vec![PageBox {
            page: 1,
            bbox: [72.0, 700.0, 540.0, 680.0],
            origin: BoxOrigin::BottomLeft,
        }],
        list_marker: (block_type == BlockType::ListItem).then(|| "1.".to_string()),
    }
}

/// 版のブロック列（doc_block）とチャンクのアンカーが同じトランザクションで保存され、
/// 版ごとに残り、ノード削除で全版が消えること（#508）。
#[tokio::test]
async fn replace_chunks_keeps_blocks_per_version_and_anchor_columns() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let ctx = ctx(&tenant);
    let node = Uuid::new_v4();
    let tags = vec![format!("file:{tenant}|{node}")];
    let blocks = vec![
        block(BlockType::Heading, "提出期限"),
        block(BlockType::Paragraph, "申出書は1か月前までに提出する。"),
        block(BlockType::ListItem, "Workday を開く"),
    ];
    let mut leaf = chunk(node, 0);
    leaf.anchor = Some(Anchor {
        block_start: 1,
        off_start: 0,
        block_end: 2,
        off_end: 12,
    });
    leaf.quote_prefix = "提出期限\n".into();
    leaf.quote_suffix = String::new();
    leaf.boxes = blocks[1].prov.clone();

    store::replace_chunks(&pool, &ctx, node, 1, &[leaf.clone()], &blocks, &tags, "m")
        .await
        .unwrap();
    // 版 2 を書いても版 1 のブロック列は残る（rag_chunk は最新版だけ）。
    store::replace_chunks(&pool, &ctx, node, 2, &[leaf], &blocks[..2], &tags, "m")
        .await
        .unwrap();

    let per_version: Vec<(i64, i64)> = sqlx::query_as(
        "select version, count(*) from doc_block where tenant_id = $1 and node_id = $2 \
         group by version order by version",
    )
    .bind(&tenant)
    .bind(node)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(per_version, vec![(1, 3), (2, 2)]);

    let (kind, marker, prov): (String, Option<String>, serde_json::Value) = sqlx::query_as(
        "select type, list_marker, prov from doc_block \
         where tenant_id = $1 and node_id = $2 and version = 1 and ordinal = 2",
    )
    .bind(&tenant)
    .bind(node)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kind, "list_item");
    assert_eq!(marker.as_deref(), Some("1."));
    assert_eq!(prov[0]["page"], 1);
    assert_eq!(prov[0]["origin"], "bottom_left");

    let (bs, os, be, oe, prefix, boxes): (i32, i32, i32, i32, String, serde_json::Value) =
        sqlx::query_as(
            "select block_start, off_start, block_end, off_end, quote_prefix, boxes \
             from rag_chunk where tenant_id = $1 and node_id = $2",
        )
        .bind(&tenant)
        .bind(node)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((bs, os, be, oe), (1, 0, 2, 12));
    assert_eq!(prefix, "提出期限\n");
    assert_eq!(boxes[0]["bbox"][0], 72.0);

    // ノード削除で全版のブロック列が消える。
    store::delete_node(&pool, &ctx, node).await.unwrap();
    let left: i64 =
        sqlx::query_scalar("select count(*) from doc_block where tenant_id = $1 and node_id = $2")
            .bind(&tenant)
            .bind(node)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left, 0);
}
