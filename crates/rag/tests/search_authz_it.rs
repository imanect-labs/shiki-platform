//! permission-aware 検索の adversarial 結合テスト（Task 2.7 受入条件）。
//!
//! 実 Postgres ＋ **実 OpenFGA** が必要（`STORAGE_TEST_DATABASE_URL` と
//! `OPENFGA_TEST_URL` が揃った時のみ実行・未設定なら skip）。
//! インジェストは実パイプライン（relay→jobq→consumer→実 Tantivy）で行い、
//! 検索は SearchService の実経路（可読集合→ハイブリッド→post-filter→rerank→監査）を通す。
//!
//! 検証（受入条件）:
//! - 共有付与 → **5 秒以内**に検索へ出る（grant SLA・PIT-3）
//! - 共有解除 → **直後**の検索から消える（HigherConsistency・PIT-11）
//! - pre-filter タグを故意に汚染しても post-filter が混入を止める（二重防御）
//! - ロール（role#member→folder viewer）経由の可読性が pre-filter に反映される
//! - 別テナントには絶対に混入しない
//! - 引用 chunk と認可判定が監査ログに残る（trace_id 付き）

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Relation, Subject};
use common::{test_ctx, FakeEmbedder, FakeObjectStore, FakeReranker, FakeVectorStore};
use rag::pipeline::{consumer, relay, PipelineDeps};
use rag::types::{BlockType, ParsedBlock, ParsedDocument};
use rag::{
    DocumentParser, ParseRequest, RagConfig, RagError, SearchMode, SearchService, TantivyFulltext,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::audit::AuditRecorder;
use storage::event::{emit_on, WriteEvent};
use storage::{IndexerStorage, WriteOp};
use uuid::Uuid;

/// ファイル名から決定的な本文を返すパーサ（検索語「売上」を必ず含む）。
struct ContentParser;

#[async_trait]
impl DocumentParser for ContentParser {
    async fn parse(
        &self,
        _ctx: &AuthContext,
        req: ParseRequest<'_>,
    ) -> Result<ParsedDocument, RagError> {
        Ok(ParsedDocument {
            blocks: vec![ParsedBlock {
                block_type: BlockType::Paragraph,
                level: None,
                text: format!("「{}」の四半期売上の報告です。", req.file_name),
                page: Some(1),
            }],
            used_ocr: false,
        })
    }
}

struct Env {
    pool: PgPool,
    deps: Arc<PipelineDeps>,
    search: SearchService,
    authz: Arc<dyn AuthzClient>,
    alice: AuthContext,
    bob: AuthContext,
    _tmp: tempfile::TempDir,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

fn serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn setup() -> Option<Env> {
    let serial = serial_lock().lock().await;
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(fga_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    // 他バイナリの残骸と混ざらないよう outbox/queue を掃除（テナントはテストごとに一意）。
    sqlx::query("update storage_event_outbox set processed_at = now() where processed_at is null")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("delete from job_queue where queue = 'rag_ingest'")
        .execute(&pool)
        .await
        .unwrap();

    let http = reqwest::Client::new();
    let fga = OpenFgaClient::connect(
        http,
        &OpenFgaConfig {
            base_url: fga_url,
            store_name: format!("shiki-rag-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);

    let tmp = tempfile::tempdir().unwrap();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let config = RagConfig {
        enabled: true,
        consumer_concurrency: 2,
        ..RagConfig::default()
    };
    let vector = Arc::new(FakeVectorStore::default());
    let fulltext = Arc::new(TantivyFulltext::new(tmp.path()));
    let deps = Arc::new(PipelineDeps {
        pool: pool.clone(),
        config: config.clone(),
        parser: Arc::new(ContentParser),
        embedder: Arc::new(FakeEmbedder),
        vector: Arc::clone(&vector) as _,
        fulltext: Arc::clone(&fulltext) as _,
        indexer_storage: Arc::new(IndexerStorage::new(pool.clone(), Arc::new(FakeObjectStore))),
    });
    let search = SearchService::new(
        pool.clone(),
        config,
        Arc::new(FakeEmbedder),
        Arc::new(FakeReranker),
        vector,
        fulltext,
        Arc::clone(&authz),
        AuditRecorder::new(pool.clone()),
    );
    Some(Env {
        pool,
        deps,
        search,
        authz,
        alice: test_ctx(&tenant, "alice"),
        bob: test_ctx(&tenant, "bob"),
        _tmp: tmp,
        _serial: serial,
    })
}

/// フォルダ node（closure 自己行つき）を作る。
async fn create_folder(env: &Env, name: &str) -> Uuid {
    let ctx = &env.alice;
    let mut tx = env.pool.begin().await.unwrap();
    let id: Uuid = sqlx::query_scalar(
        // updated_by は NOT NULL（migration 0045）。作成＝最初の更新なので created_by と同値。
        "insert into node (org, tenant_id, kind, name, created_by, updated_by) \
         values ($1, $2, 'folder', $3, $4, $4) returning id",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(name)
    .bind(&ctx.principal.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "insert into node_closure (org, tenant_id, ancestor, descendant, depth) \
         values ($1, $2, $3, $3, 0)",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

/// ファイル node を作り、FGA タプル（owner=alice・parent=フォルダ）を張り、
/// create イベントを発行してパイプラインで索引する。
async fn index_file(env: &Env, folder: Uuid, name: &str) -> Uuid {
    let ctx = &env.alice;
    let sha = format!("{:0>64}", Uuid::new_v4().simple().to_string());
    let mut tx = env.pool.begin().await.unwrap();
    sqlx::query(
        "insert into blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount) \
         values ($1, $2, $3, 10, 'text/plain', $4, 1)",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(&sha)
    .bind(format!("{}/{}/{}", ctx.tenant_id, ctx.org, sha))
    .execute(&mut *tx)
    .await
    .unwrap();
    let node_id: Uuid = sqlx::query_scalar(
        // updated_by は NOT NULL（migration 0045）。作成＝最初の更新なので created_by と同値。
        "insert into node (org, tenant_id, kind, name, parent_id, blob_sha256, size_bytes, \
                           content_type, created_by, updated_by) \
         values ($1, $2, 'file', $3, $4, $5, 10, 'text/plain', $6, $6) returning id",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(name)
    .bind(folder)
    .bind(&sha)
    .bind(&ctx.principal.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "insert into node_closure (org, tenant_id, ancestor, descendant, depth) \
         values ($1, $2, $3, $3, 0), ($1, $2, $4, $3, 1)",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(node_id)
    .bind(folder)
    .execute(&mut *tx)
    .await
    .unwrap();
    emit_on(
        &mut tx,
        ctx,
        WriteEvent {
            node_id,
            version: 1,
            op: WriteOp::Create,
            payload: serde_json::json!({}),
        },
        Some("trace-search-it"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // FGA: owner=alice・parent=フォルダ（folder viewer の継承経路）。
    let file_obj = ctx.ns().file(&node_id.to_string());
    env.authz
        .write_tuple(&ctx.subject(), Relation::Owner, &file_obj)
        .await
        .unwrap();
    env.authz
        .write_tuple(
            &Subject::object(&ctx.ns().folder(&folder.to_string())),
            Relation::Parent,
            &file_obj,
        )
        .await
        .unwrap();

    drain(env).await;
    node_id
}

async fn drain(env: &Env) {
    for _ in 0..20 {
        // バックオフ待ちのリトライも即時消費し、drain を決定的にする（テスト専用）。
        sqlx::query("update job_queue set visible_at = now() where queue = 'rag_ingest'")
            .execute(&env.pool)
            .await
            .unwrap();
        relay::relay_once(&env.pool, &env.deps.config)
            .await
            .unwrap();
        let n = consumer::consume_once(&env.deps).await.unwrap();
        if n == 0 {
            let queued: i64 =
                sqlx::query_scalar("select count(*) from job_queue where queue = 'rag_ingest'")
                    .fetch_one(&env.pool)
                    .await
                    .unwrap();
            if queued == 0 {
                return;
            }
        }
    }
}

async fn hit_files(env: &Env, ctx: &AuthContext, query: &str) -> Vec<Uuid> {
    env.search
        .search(
            ctx,
            query,
            Some(10),
            SearchMode::Hybrid,
            None,
            Some("trace-search-it"),
        )
        .await
        .unwrap()
        .results
        .iter()
        .map(|r| r.file_id)
        .collect()
}

#[tokio::test]
async fn grant_appears_within_sla_and_revoke_disappears_immediately() {
    let Some(env) = setup().await else { return };
    let folder = create_folder(&env, "経営企画").await;
    let file = index_file(&env, folder, "極秘計画").await;

    // owner の alice には見える。
    assert_eq!(hit_files(&env, &env.alice, "売上").await, vec![file]);
    // 共有前の bob には見えない。
    assert!(hit_files(&env, &env.bob, "売上").await.is_empty());

    // 【受入条件】共有付与 → 5 秒以内に検索へ出る（pre-filter はクエリごと算出）。
    let folder_obj = env.alice.ns().folder(&folder.to_string());
    env.authz
        .write_tuple(&env.bob.subject(), Relation::Viewer, &folder_obj)
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if hit_files(&env, &env.bob, "売上").await == vec![file] {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "grant 後 5 秒以内に検索へ出ること（PIT-3 SLA）"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // 【受入条件】共有解除 → 直後の検索から消える（剥奪の即時反映）。
    env.authz
        .delete_tuple(&env.bob.subject(), Relation::Viewer, &folder_obj)
        .await
        .unwrap();
    assert!(
        hit_files(&env, &env.bob, "売上").await.is_empty(),
        "剥奪直後に検索から消えること"
    );
}

#[tokio::test]
async fn post_filter_blocks_poisoned_pre_filter_tags() {
    let Some(env) = setup().await else { return };
    let secret_folder = create_folder(&env, "秘匿").await;
    let public_folder = create_folder(&env, "公開").await;
    let secret_file = index_file(&env, secret_folder, "秘匿文書").await;

    // bob は public_folder の viewer（secret_file への権限は無い）。
    env.authz
        .write_tuple(
            &env.bob.subject(),
            Relation::Viewer,
            &env.alice.ns().folder(&public_folder.to_string()),
        )
        .await
        .unwrap();

    // pre-filter タグを故意に汚染: secret_file のタグに public_folder を混ぜる
    //（タグ再評価バグ・タグ焼き込みの陳腐化を模す）。
    let poisoned = vec![env
        .alice
        .ns()
        .folder(&public_folder.to_string())
        .as_str()
        .to_string()];
    sqlx::query("update rag_chunk set authz_tags = $2 where node_id = $1")
        .bind(secret_file)
        .bind(&poisoned)
        .execute(&env.pool)
        .await
        .unwrap();
    env.deps
        .vector
        .set_authz_tags(&env.alice, secret_file, &poisoned)
        .await
        .unwrap();
    {
        // Tantivy 側も汚染タグで再投入。
        let stored = rag::store::chunks_for_node(&env.pool, &env.alice, secret_file)
            .await
            .unwrap();
        let docs: Vec<rag::FulltextDoc<'_>> = stored
            .iter()
            .filter(|c| c.kind != "parent")
            .map(|c| rag::FulltextDoc {
                chunk_id: c.id,
                node_id: c.node_id,
                version: c.version,
                text: &c.content,
                authz_tags: &poisoned,
            })
            .collect();
        env.deps
            .fulltext
            .replace_node(&env.alice, secret_file, &docs)
            .unwrap();
    }

    // 【受入条件】pre-filter が汚染されても post-filter（OpenFGA file check）が混入を止める。
    let output = env
        .search
        .search(&env.bob, "売上", Some(10), SearchMode::Hybrid, None, None)
        .await
        .unwrap();
    assert!(
        output.results.is_empty(),
        "混入ゼロ（二重防御の post-filter）"
    );
    assert!(
        output.debug.authz_denied_files >= 1,
        "pre-filter を通過した候補が post-filter で落とされている"
    );
}

#[tokio::test]
async fn role_membership_grants_visibility_through_folder() {
    let Some(env) = setup().await else { return };
    let folder = create_folder(&env, "営業部フォルダ").await;
    let file = index_file(&env, folder, "営業実績").await;

    // 営業部ロール: bob をメンバーにし、フォルダ viewer をロールへ付与。
    let role = env.alice.ns().role("sales");
    env.authz
        .write_tuple(&env.bob.subject(), Relation::Member, &role)
        .await
        .unwrap();
    env.authz
        .write_tuple(
            &env.alice.ns().role_member("sales"),
            Relation::Viewer,
            &env.alice.ns().folder(&folder.to_string()),
        )
        .await
        .unwrap();

    // role#member → folder viewer → file viewer の継承が pre-filter（ListObjects）に反映される。
    assert_eq!(hit_files(&env, &env.bob, "売上").await, vec![file]);
}

#[tokio::test]
async fn other_tenant_never_sees_anything() {
    let Some(env) = setup().await else { return };
    let folder = create_folder(&env, "本社").await;
    index_file(&env, folder, "本社資料").await;

    // 別テナントの charlie は何をしても 0 件（index-per-tenant ＋ tenant 無条件 AND）。
    let charlie = test_ctx(&format!("b-{}", Uuid::new_v4().simple()), "charlie");
    assert!(hit_files(&env, &charlie, "売上").await.is_empty());
}

#[tokio::test]
async fn citations_and_decisions_are_audited_with_trace_id() {
    let Some(env) = setup().await else { return };
    let folder = create_folder(&env, "監査対象").await;
    let file = index_file(&env, folder, "監査文書").await;

    let output = env
        .search
        .search(
            &env.alice,
            "売上",
            Some(5),
            SearchMode::Hybrid,
            None,
            Some("trace-audit-test"),
        )
        .await
        .unwrap();
    assert!(!output.results.is_empty());

    // 【受入条件】引用 chunk と認可判定が監査ログに残る（trace_id 付き）。
    let (metadata, trace_id): (serde_json::Value, Option<String>) = sqlx::query_as(
        "select metadata, trace_id from audit_log \
         where tenant_id = $1 and action = 'rag.search' order by id desc limit 1",
    )
    .bind(&env.alice.tenant_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(trace_id.as_deref(), Some("trace-audit-test"));
    let cited = metadata["cited_chunk_ids"].as_array().unwrap();
    assert!(!cited.is_empty(), "引用 chunk_id 群が記録される");
    assert_eq!(
        metadata["cited_file_ids"][0].as_str().unwrap(),
        file.to_string()
    );
    assert!(
        metadata["file_decisions"]["allowed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some(file.to_string().as_str())),
        "file 粒度の認可判定（allow）が記録される"
    );
    assert!(metadata["query_sha256"].as_str().unwrap().len() == 64);
}
