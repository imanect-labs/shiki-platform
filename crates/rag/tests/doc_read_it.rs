//! 索引済み文書の本文読み出し（`SearchService::read_document`・#425）。
//!
//! 実 Postgres のみ必要（`STORAGE_TEST_DATABASE_URL`）。認可は呼び出し側（StorageService の
//! `get_metadata`）が済ませた `Node` を受け取る形なので、ここでは読み出しの中身・窓・境界を見る。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use authz::AuthContext;
use common::{test_ctx, test_ctx_org, FakeEmbedder, FakeReranker, FakeVectorStore, ScriptedAuthz};
use rag::{DocRead, RagConfig, ReadFrom, ReadSource, SearchService, TantivyFulltext};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::audit::AuditRecorder;
use uuid::Uuid;

struct Env {
    pool: PgPool,
    ctx: AuthContext,
    search: SearchService,
    /// 可読集合（pre-filter）に入っているフォルダの構造タグ。積むチャンクは既定でこれを持つ。
    readable_tag: String,
    _tmp: tempfile::TempDir,
}

async fn setup() -> Option<Env> {
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
    let ctx = test_ctx(&format!("t-{}", Uuid::new_v4().simple()), "alice");
    let readable_tag = ctx
        .ns()
        .folder(&Uuid::new_v4().to_string())
        .as_str()
        .to_string();
    let tmp = tempfile::tempdir().unwrap();
    let search = SearchService::new(
        pool.clone(),
        RagConfig::default(),
        Arc::new(FakeEmbedder),
        Arc::new(FakeReranker),
        Arc::new(FakeVectorStore::default()),
        Arc::new(TantivyFulltext::new(tmp.path())),
        Arc::new(ScriptedAuthz {
            readable_folders: vec![readable_tag.clone()],
            readable_files: vec![],
            denied_files: HashSet::new(),
        }),
        AuditRecorder::new(pool.clone()),
    );
    Some(Env {
        pool,
        ctx,
        search,
        readable_tag,
        _tmp: tmp,
    })
}

/// file node を作り、認可済みの `Node`（実運用では get_metadata の戻り）を返す。
async fn make_file(env: &Env, name: &str, version: i64) -> storage::Node {
    let ctx = &env.ctx;
    let sha = format!("{:0>64}", Uuid::new_v4().simple().to_string());
    sqlx::query(
        "insert into blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount) \
         values ($1, $2, $3, 10, 'text/plain', $4, 1)",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(&sha)
    .bind(format!("{}/{}/{}", ctx.tenant_id, ctx.org, sha))
    .execute(&env.pool)
    .await
    .unwrap();
    let id: Uuid = sqlx::query_scalar(
        "insert into node (org, tenant_id, kind, name, blob_sha256, size_bytes, content_type, \
                           version, created_by, updated_by) \
         values ($1, $2, 'file', $3, $4, 10, 'text/plain', $5, $6, $6) returning id",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(name)
    .bind(&sha)
    .bind(version)
    .bind(&ctx.principal.id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    let now = sqlx::types::chrono::Utc::now();
    storage::Node {
        id,
        org: ctx.org.clone(),
        tenant_id: ctx.tenant_id.clone(),
        kind: storage::NodeKind::File,
        name: name.into(),
        parent_id: None,
        blob_sha256: Some(sha),
        size_bytes: Some(10),
        content_type: Some("text/plain".into()),
        version,
        deleted_at: None,
        created_by: ctx.principal.id.clone(),
        updated_by: ctx.principal.id.clone(),
        created_at: now,
        updated_at: now,
    }
}

/// rag_chunk を 1 行積む。
#[allow(clippy::too_many_arguments)] // テストの行データをそのまま並べる。
async fn chunk(
    env: &Env,
    node: &storage::Node,
    version: i64,
    ordinal: i32,
    kind: &str,
    page: Option<i32>,
    path: &[&str],
    content: &str,
) {
    let ctx = &env.ctx;
    sqlx::query(
        "insert into rag_chunk \
             (id, tenant_id, org, node_id, version, parent_id, kind, ordinal, page, heading_path, \
              content, char_count, authz_tags, embedding_model_version) \
         values ($1, $2, $3, $4, $5, null, $6, $7, $8, $9, $10, 1, $11, 'fake-model')",
    )
    .bind(Uuid::new_v4())
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(node.id)
    .bind(version)
    .bind(kind)
    .bind(ordinal)
    .bind(page)
    .bind(path.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    .bind(content)
    .bind(vec![env.readable_tag.clone()])
    .execute(&env.pool)
    .await
    .unwrap();
}

/// doc_block を 1 行積む。
async fn block(
    env: &Env,
    node: &storage::Node,
    version: i64,
    ordinal: i32,
    kind: &str,
    page: i32,
    text: &str,
) {
    let ctx = &env.ctx;
    sqlx::query(
        "insert into doc_block (tenant_id, org, node_id, version, ordinal, type, level, text, page) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(node.id)
    .bind(version)
    .bind(ordinal)
    .bind(kind)
    .bind((kind == "heading").then_some(2))
    .bind(text)
    .bind(page)
    .execute(&env.pool)
    .await
    .unwrap();
}

fn page(read: DocRead) -> rag::DocReadPage {
    match read {
        DocRead::Page(p) => p,
        other => panic!("本文が読めること: {other:?}"),
    }
}

#[tokio::test]
async fn reads_blocks_by_position_and_records_audit() {
    let Some(env) = setup().await else { return };
    let node = make_file(&env, "規程.pdf", 2).await;
    // 旧版の索引は読まない（最新版のみ）。
    chunk(&env, &node, 1, 0, "leaf", Some(1), &[], "旧版の本文").await;
    chunk(&env, &node, 2, 0, "leaf", Some(1), &[], "検索用").await;
    block(&env, &node, 2, 0, "heading", 1, "第1条 目的").await;
    block(&env, &node, 2, 1, "paragraph", 1, "この規程は…").await;
    block(&env, &node, 2, 2, "heading", 3, "第5条 申請").await;
    block(&env, &node, 2, 3, "paragraph", 3, "申請は人事部へ").await;

    let ctx = &env.ctx;
    let all = page(
        env.search
            .read_document(ctx, &node, &ReadFrom::Start, 8_000, Some("trace-read"))
            .await
            .unwrap(),
    );
    assert_eq!(all.version, 2);
    assert_eq!(all.source, ReadSource::Blocks);
    assert!(all.text.starts_with("〔p.1〕\n## 第1条 目的"));
    assert!(all
        .text
        .contains("〔p.3〕\n## 第5条 申請\n\n申請は人事部へ"));
    assert!(!all.text.contains("旧版"));
    assert_eq!(all.next, None);

    // 見出し・ページで読み始める。
    for from in [
        ReadFrom::Heading("5条".into()),
        ReadFrom::Page(2),
        ReadFrom::At(rag::Cursor { unit: 2, offset: 0 }),
    ] {
        let p = page(
            env.search
                .read_document(ctx, &node, &from, 8_000, None)
                .await
                .unwrap(),
        );
        assert!(p.text.contains("## 第5条 申請"), "{from:?}");
        assert!(!p.text.contains("第1条"), "{from:?}");
    }

    // 窓の上限で切れ、続きの位置が返る。
    let first = page(
        env.search
            .read_document(ctx, &node, &ReadFrom::Start, 20, None)
            .await
            .unwrap(),
    );
    assert_eq!(first.next, Some(rag::Cursor { unit: 1, offset: 0 }));

    // 見つからない見出しは目次を返す。LIKE のワイルドカードは効かない。
    match env
        .search
        .read_document(ctx, &node, &ReadFrom::Heading("%".into()), 8_000, None)
        .await
        .unwrap()
    {
        DocRead::PositionNotFound { version, outline } => {
            assert_eq!(version, 2);
            assert_eq!(outline, vec!["第1条 目的", "第5条 申請"]);
        }
        other => panic!("見出しが見つからないこと: {other:?}"),
    }

    // 本文の読み出しは rag.doc_read として版・範囲つきで残る。
    let metadata: serde_json::Value = sqlx::query_scalar(
        "select metadata from audit_log \
         where tenant_id = $1 and object_id = $2 and action = 'rag.doc_read' \
           and trace_id = 'trace-read'",
    )
    .bind(&ctx.tenant_id)
    .bind(node.id.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(metadata["version"], 2);
    assert_eq!(metadata["source"], "blocks");
    assert_eq!(metadata["from"], serde_json::json!([0, 0]));
}

/// ブロック列の無い旧索引は、検索用チャンク（leaf / table）を連結して読む。
#[tokio::test]
async fn falls_back_to_chunks_for_old_indexes() {
    let Some(env) = setup().await else { return };
    let node = make_file(&env, "議事録.docx", 1).await;
    chunk(&env, &node, 1, 0, "leaf", None, &["第1章"], "冒頭").await;
    chunk(&env, &node, 1, 1, "parent", None, &["第1章"], "親（写し）").await;
    chunk(
        &env,
        &node,
        1,
        2,
        "table",
        None,
        &["第2章", "予算"],
        "| 費目 |",
    )
    .await;
    chunk(&env, &node, 1, 3, "parent", None, &["第2章", "予算"], "親").await;

    let ctx = &env.ctx;
    let p = page(
        env.search
            .read_document(ctx, &node, &ReadFrom::Start, 8_000, None)
            .await
            .unwrap(),
    );
    assert_eq!(p.source, ReadSource::Chunks);
    assert_eq!(p.text, "## 第1章\n冒頭\n\n## 第2章 > 予算\n| 費目 |\n\n");

    let p = page(
        env.search
            .read_document(ctx, &node, &ReadFrom::Heading("予算".into()), 8_000, None)
            .await
            .unwrap(),
    );
    assert!(p.text.starts_with("## 第2章 > 予算"));
    match env
        .search
        .read_document(ctx, &node, &ReadFrom::Heading("付録".into()), 8_000, None)
        .await
        .unwrap()
    {
        DocRead::PositionNotFound { outline, .. } => {
            assert_eq!(outline, vec!["第1章", "第2章 > 予算"]);
        }
        other => panic!("見出しが見つからないこと: {other:?}"),
    }
}

/// 索引が無ければ「未索引」。別 org・別テナントの文脈では何も読めない。
#[tokio::test]
async fn unindexed_and_foreign_contexts_read_nothing() {
    let Some(env) = setup().await else { return };
    let node = make_file(&env, "未索引.pdf", 1).await;
    let got = env
        .search
        .read_document(&env.ctx, &node, &ReadFrom::Start, 8_000, None)
        .await
        .unwrap();
    assert_eq!(got, DocRead::NotIndexed);

    chunk(&env, &node, 1, 0, "leaf", None, &[], "社外秘").await;
    for other in [
        test_ctx(&format!("t-{}", Uuid::new_v4().simple()), "alice"),
        test_ctx_org(&env.ctx.tenant_id, "alice", "other-org"),
    ] {
        let got = env
            .search
            .read_document(&other, &node, &ReadFrom::Start, 8_000, None)
            .await
            .unwrap();
        assert_eq!(got, DocRead::NotIndexed, "tenant と org で必ず絞る");
    }
}

/// 窓より大きい単位（巨大な表）も、同じ単位の途中から続きを読める。見出しが見つからずに
/// 見出し一覧を返す場合も、本文由来の内容を返すので監査に残す。
#[tokio::test]
async fn huge_unit_continues_and_outline_is_audited() {
    let Some(env) = setup().await else { return };
    let node = make_file(&env, "大きな表.xlsx", 1).await;
    let table = format!("{}{}", "行".repeat(30), "末尾");
    chunk(&env, &node, 1, 0, "leaf", Some(1), &[], "検索用").await;
    block(&env, &node, 1, 0, "table", 1, &table).await;
    block(&env, &node, 1, 1, "paragraph", 1, "表の後").await;
    let ctx = &env.ctx;

    let first = page(
        env.search
            .read_document(ctx, &node, &ReadFrom::Start, 20, None)
            .await
            .unwrap(),
    );
    let cursor = first.next.expect("続きがある");
    assert_eq!(cursor.unit, 0, "同じ単位の途中から続く");
    let mut read = first.text.clone();
    let mut at = cursor;
    for _ in 0..10 {
        let p = page(
            env.search
                .read_document(ctx, &node, &ReadFrom::At(at), 20, None)
                .await
                .unwrap(),
        );
        read.push_str(&p.text);
        match p.next {
            Some(c) => at = c,
            None => break,
        }
    }
    let rows = read.matches('行').count();
    assert_eq!(rows, 30, "表の全行を欠けも重複もなく読める: {read}");
    assert!(read.contains("末尾") && read.contains("表の後"));

    env.search
        .read_document(
            ctx,
            &node,
            &ReadFrom::Heading("付録".into()),
            8_000,
            Some("trace-outline"),
        )
        .await
        .unwrap();
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from audit_log \
         where tenant_id = $1 and action = 'rag.doc_read' and trace_id = 'trace-outline'",
    )
    .bind(&ctx.tenant_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
}

/// 二段 authz の pre-filter: viewer 判定（呼び出し側）を通っても、可読集合にチャンクの
/// authz_tags が入っていなければ本文は返さない（片方が誤って許可しても漏れない）。
#[tokio::test]
async fn prefilter_denies_when_chunk_tags_are_not_readable() {
    let Some(env) = setup().await else { return };
    let node = make_file(&env, "他部署.pdf", 1).await;
    chunk(&env, &node, 1, 0, "leaf", None, &[], "社外秘").await;
    sqlx::query("update rag_chunk set authz_tags = $1 where node_id = $2")
        .bind(vec![env
            .ctx
            .ns()
            .folder(&Uuid::new_v4().to_string())
            .as_str()
            .to_string()])
        .bind(node.id)
        .execute(&env.pool)
        .await
        .unwrap();
    let got = env
        .search
        .read_document(&env.ctx, &node, &ReadFrom::Start, 8_000, None)
        .await
        .unwrap();
    assert_eq!(got, DocRead::Denied);
}

/// 取得の総量を窓で打ち切っても（SQL の累積字数）、続きを辿れば全ブロックを順に欠けなく読める。
#[tokio::test]
async fn many_blocks_are_read_in_order_across_windows() {
    let Some(env) = setup().await else { return };
    let node = make_file(&env, "多数.md", 1).await;
    chunk(&env, &node, 1, 0, "leaf", None, &[], "検索用").await;
    for i in 0..50 {
        block(
            &env,
            &node,
            1,
            i,
            "paragraph",
            1,
            &format!("段落{i:03}です"),
        )
        .await;
    }
    let ctx = &env.ctx;
    let mut from = ReadFrom::Start;
    let mut read = String::new();
    for _ in 0..100 {
        let p = page(
            env.search
                .read_document(ctx, &node, &from, 25, None)
                .await
                .unwrap(),
        );
        read.push_str(&p.text);
        match p.next {
            Some(c) => from = ReadFrom::At(c),
            None => break,
        }
    }
    let seen: Vec<String> = (0..50)
        .filter(|i| read.contains(&format!("段落{i:03}です")))
        .map(|i| format!("{i:03}"))
        .collect();
    assert_eq!(seen.len(), 50, "全ブロックを読む: {read}");
    assert_eq!(read.matches("段落").count(), 50, "重複しない");
}
