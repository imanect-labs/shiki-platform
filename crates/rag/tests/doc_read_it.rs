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

/// このテストのテナントで積んだ行を片付ける（索引・取り込みの記録・内容の履歴・ファイル・監査）。
async fn cleanup(env: &Env) {
    let tenant = &env.ctx.tenant_id;
    for sql in [
        "delete from rag_chunk where tenant_id = $1",
        "delete from doc_block where tenant_id = $1",
        "delete from rag_ingest_job where tenant_id = $1",
        "delete from storage_event_outbox where tenant_id = $1",
        "delete from job_queue where tenant_id = $1",
        "delete from node_version where tenant_id = $1",
        "delete from node where tenant_id = $1",
        "delete from blob where tenant_id = $1",
        "delete from audit_log where tenant_id = $1",
    ] {
        sqlx::query(sql)
            .bind(tenant)
            .execute(&env.pool)
            .await
            .unwrap();
    }
}

/// 先頭から続けて読む（D-111・ワークフローの「ファイルを読む」）: 窓を 1 つにつなぐので、ページの印や
/// 「（前の続き）」が窓の境目で重ならず、1 回の SQL の件数の上限（400 件）を超える文書も順に欠けなく読める。
/// 上限の字数で切ったときは続きの位置が返る（切った印）。監査は 1 回だけ残る。
#[tokio::test]
async fn reads_whole_text_across_batches_and_marks_the_cut() {
    let Some(env) = setup().await else { return };
    let node = make_file(&env, "就業規則.pdf", 1).await;
    chunk(&env, &node, 1, 0, "leaf", Some(1), &[], "検索用").await;
    block(&env, &node, 1, 0, "heading", 1, "第1条 目的").await;
    for i in 1..=500 {
        // 2 ページ目から後ろは 100 段落ごとにページが変わる。
        block(
            &env,
            &node,
            1,
            i,
            "paragraph",
            1 + i / 100,
            &format!("段落{i:03}"),
        )
        .await;
    }
    let ctx = &env.ctx;
    let whole = page(
        env.search
            .read_document_text(ctx, &node, 200_000, Some("trace-whole"))
            .await
            .unwrap(),
    );
    assert_eq!(whole.version, 1);
    assert_eq!(whole.next, None, "文書の終わりまで読んだ");
    assert!(whole
        .text
        .starts_with("〔p.1〕\n## 第1条 目的\n\n段落001\n\n"));
    let order: Vec<usize> = (1..=500)
        .map(|i| {
            whole
                .text
                .find(&format!("段落{i:03}\n"))
                .expect("全段落を読む")
        })
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "順に並ぶ");
    assert_eq!(whole.text.matches("段落").count(), 500, "重複しない");
    // ページの印はページが変わる所に 1 回だけ（窓の境目で繰り返さない）。
    for p in 1..=6 {
        assert_eq!(
            whole.text.matches(&format!("〔p.{p}〕")).count(),
            1,
            "p.{p}"
        );
    }
    assert!(!whole.text.contains("前の続き"));
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from audit_log \
         where tenant_id = $1 and action = 'rag.doc_read' and trace_id = 'trace-whole'",
    )
    .bind(&ctx.tenant_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1, "続けて読んでも監査は 1 回");

    // 上限の字数で切ると、続きの位置が返る（切った印）。切った後ろの段落は入らない。
    let head = page(
        env.search
            .read_document_text(ctx, &node, 60, None)
            .await
            .unwrap(),
    );
    assert!(head.next.is_some(), "上限で切った");
    assert!(head.text.chars().count() <= 60, "{}", head.text);
    assert!(head.text.contains("段落001") && !head.text.contains("段落100"));

    // pre-filter（可読集合）で読めなければ本文を返さない（read_document と同じ契約）。
    sqlx::query("update rag_chunk set authz_tags = $1 where node_id = $2")
        .bind(vec![ctx
            .ns()
            .folder(&Uuid::new_v4().to_string())
            .as_str()
            .to_string()])
        .bind(node.id)
        .execute(&env.pool)
        .await
        .unwrap();
    let denied = env
        .search
        .read_document_text(ctx, &node, 200_000, None)
        .await
        .unwrap();
    cleanup(&env).await;
    assert_eq!(denied, DocRead::Denied);
}

/// 索引に本文が無い理由（D-111）: 取り込めない種類・大きすぎる・取り込み待ち・取り込みに失敗・本文が無い・取り込まない
/// 場所にある・取り込みが届いていない。いまの内容は内容の履歴の最新で決めるので、名前の変更で node.version が進んでも
/// 索引の本文を読める。新しい内容の取り込みが終わるまでは、古い内容を「読める」と言わない。「待てば読める」は、
/// 取り込みへ向かう記録（outbox の未処理のイベント・取り込みの待ち行列・取り込みの記録）があるときだけ言う。
#[tokio::test]
async fn index_state_tells_why_the_text_is_not_readable_yet() {
    let Some(env) = setup().await else { return };
    let ctx = &env.ctx;
    let mut node = make_file(&env, "契約書.pdf", 1).await;
    sqlx::query("update node set content_type = 'application/pdf' where id = $1")
        .bind(node.id)
        .execute(&env.pool)
        .await
        .unwrap();
    node.content_type = Some("application/pdf".into());
    let record_version = |version: i64| {
        sqlx::query(
            "insert into node_version (node_id, version, org, tenant_id, blob_sha256, size_bytes, \
                                       content_type, author) \
             values ($1, $2, $3, $4, $5, 10, 'application/pdf', 'alice')",
        )
        .bind(node.id)
        .bind(version)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(node.blob_sha256.clone().unwrap())
    };
    let job = |version: i64, op: &'static str, status: &'static str| {
        sqlx::query(
            "insert into rag_ingest_job (tenant_id, org, node_id, version, op, status) \
             values ($1, $2, $3, $4, $5, $6) \
             on conflict (tenant_id, node_id, version, op) do update set status = excluded.status",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(node.id)
        .bind(version)
        .bind(op)
        .bind(status)
    };
    let set_node_version = |version: i64| {
        sqlx::query("update node set version = $2 where id = $1")
            .bind(node.id)
            .bind(version)
    };
    record_version(1).execute(&env.pool).await.unwrap();
    let mut states = Vec::new();
    let mut state = |label: &'static str, s: rag::IndexState| states.push((label, s));

    // 取り込みへ向かう記録がどこにも無い（社内文書の検索を有効にする前に置いた・outbox の掃除で消えた）。
    state(
        "記録が無い",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    // 書き込みと同じ Tx で outbox に積まれ、relay がまだ取り込みへ積んでいない。
    sqlx::query(
        "insert into storage_event_outbox (org, tenant_id, node_id, version, op, actor) \
         values ($1, $2, $3, 1, 'create', 'alice')",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(node.id)
    .execute(&env.pool)
    .await
    .unwrap();
    state(
        "outbox にある",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    // relay が取り込みの待ち行列へ積んだ（outbox は処理済み）。
    sqlx::query("update storage_event_outbox set processed_at = now() where tenant_id = $1")
        .bind(&ctx.tenant_id)
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query("insert into job_queue (queue, tenant_id, payload) values ('rag_ingest', $1, $2)")
        .bind(&ctx.tenant_id)
        .bind(
            serde_json::json!({ "tenant_id": ctx.tenant_id, "org": ctx.org,
            "node_id": node.id, "version": 1, "op": "create", "actor": "alice" }),
        )
        .execute(&env.pool)
        .await
        .unwrap();
    state(
        "待ち行列にある",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    sqlx::query("delete from job_queue where tenant_id = $1")
        .bind(&ctx.tenant_id)
        .execute(&env.pool)
        .await
        .unwrap();
    // 取り込み中・一時的な失敗のやり直し待ちは、待てば読める。
    job(1, "create", "failed").execute(&env.pool).await.unwrap();
    state(
        "やり直し待ち",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    // 取り込みを飛ばした（前の版で、取り込みより先に名前を変えたファイル）: 待っても取り込まれない。
    job(1, "create", "skipped")
        .execute(&env.pool)
        .await
        .unwrap();
    state(
        "取り込みを飛ばした",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    // 取り込みが終わったのに本文が無い（文字の無い文書）。
    job(1, "create", "succeeded")
        .execute(&env.pool)
        .await
        .unwrap();
    state(
        "本文が無い",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    chunk(&env, &node, 1, 0, "leaf", Some(1), &[], "第1条").await;
    state(
        "取り込み済み",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    // 名前の変更は node.version を進めるが、内容は同じなので読める。
    set_node_version(2).execute(&env.pool).await.unwrap();
    node.version = 2;
    state(
        "名前の変更の後",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    // ごみ箱に入れて索引を消し（v3）、戻した（v4）。戻した取り込みが記録に入る前でも、本文が無いとは言わない。
    job(3, "delete", "succeeded")
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query("delete from rag_chunk where node_id = $1")
        .bind(node.id)
        .execute(&env.pool)
        .await
        .unwrap();
    set_node_version(4).execute(&env.pool).await.unwrap();
    node.version = 4;
    state(
        "ごみ箱から戻した直後",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    job(4, "restore", "succeeded")
        .execute(&env.pool)
        .await
        .unwrap();
    chunk(&env, &node, 4, 0, "leaf", Some(1), &[], "第1条").await;
    state(
        "ごみ箱から戻して取り込み直した",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    // 新しい内容（v5）の取り込みが終わるまでは、古い内容（v1）を読めると言わない。
    record_version(5).execute(&env.pool).await.unwrap();
    set_node_version(5).execute(&env.pool).await.unwrap();
    node.version = 5;
    job(5, "update", "running")
        .execute(&env.pool)
        .await
        .unwrap();
    state(
        "新しい内容の取り込み待ち",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    job(5, "update", "dead").execute(&env.pool).await.unwrap();
    state(
        "新しい内容の取り込みに失敗",
        env.search.index_state(ctx, &node).await.unwrap(),
    );
    // 別の org の文脈ではノードも索引も見えない（本文は返さない）。
    let other = test_ctx_org(&ctx.tenant_id, "alice", "other-org");
    state(
        "別の org",
        env.search.index_state(&other, &node).await.unwrap(),
    );
    // システム領域（AI エージェントの作業用のフォルダー）のファイルは取り込まない。
    sqlx::query("update node set system = true where id = $1")
        .bind(node.id)
        .execute(&env.pool)
        .await
        .unwrap();
    state(
        "システム領域",
        env.search.index_state(ctx, &node).await.unwrap(),
    );

    // 種類と大きさは、取り込みの記録より先に決まる。
    let mut image = node.clone();
    image.content_type = Some("image/png".into());
    state("画像", env.search.index_state(ctx, &image).await.unwrap());
    let mut unknown = node.clone();
    unknown.content_type = None;
    state(
        "種類が分からない",
        env.search.index_state(ctx, &unknown).await.unwrap(),
    );
    let mut big = node.clone();
    let limit_bytes = RagConfig::default().max_parse_bytes;
    big.size_bytes = Some(limit_bytes + 1);
    state(
        "大きすぎる",
        env.search.index_state(ctx, &big).await.unwrap(),
    );
    cleanup(&env).await;

    use rag::IndexState as S;
    assert_eq!(
        states,
        vec![
            ("記録が無い", S::NotIngested),
            ("outbox にある", S::Pending),
            ("待ち行列にある", S::Pending),
            ("やり直し待ち", S::Pending),
            ("取り込みを飛ばした", S::NotIngested),
            ("本文が無い", S::Empty),
            ("取り込み済み", S::Ready),
            ("名前の変更の後", S::Ready),
            ("ごみ箱から戻した直後", S::Pending),
            ("ごみ箱から戻して取り込み直した", S::Ready),
            ("新しい内容の取り込み待ち", S::Pending),
            ("新しい内容の取り込みに失敗", S::Failed),
            ("別の org", S::Pending),
            ("システム領域", S::Excluded),
            ("画像", S::Unsupported),
            ("種類が分からない", S::Unsupported),
            ("大きすぎる", S::TooLarge { limit_bytes }),
        ]
    );
}
