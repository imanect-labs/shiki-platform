//! ゴミ箱の完全削除（GAP-108b）が検索の索引を残さないことの結合テスト。
//!
//! 実 StorageService（実 Postgres・RustFS・OpenFGA）で書き・論理削除・完全削除し、outbox → relay → jobq →
//! 自分のテナントのジョブの取り込み（`indexer::handle`）→ 索引の実経路を回す。全文索引は実 Tantivy（tempdir）、`rag_chunk`・`doc_block` は実 DB。
//! ベクタはインメモリの VectorStore（tenant の無条件 AND と delete_node の意味は本物と同じ）を使う。実 Qdrant の
//! `delete_node` は `vector_qdrant_it` が確かめる。ここで実 Qdrant を使わないのは、索引が毎回 alias
//! （`rag_chunks_active`・インスタンスで共有）を自分の collection に向け直すため、別のテストのバイナリと並ぶと
//! `vector_qdrant_it` の alias を奪い合うから。
//!
//! `STORAGE_TEST_DATABASE_URL` / `OPENFGA_TEST_URL` が揃った時のみ実行する（未設定ならスキップ・
//! `SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! 確かめること:
//! - 論理削除のイベントを取り込む前に完全削除しても（フォルダのイベントは node が無いので配下へ展開されない）、
//!   ファイルごとの delete で `rag_chunk`・`doc_block`・ベクタ・全文の索引が残らない。
//! - 完全削除の delete は、論理削除のときの delete と取り込みの重複除け（tenant, node_id, version, op）が重ならない
//!   （論理削除の delete を済ませた後に残った索引も消える）。

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
use authz::{AuthContext, AuthzClient, Consistency, Principal, Relation};
use common::{FakeEmbedder, FakeVectorStore};
use rag::pipeline::{indexer, relay, IngestMessage, PipelineDeps, RAG_INGEST_QUEUE};
use rag::types::{BlockType, ParsedBlock, ParsedDocument};
use rag::vector_store::PreFilter;
use rag::{DocumentParser, ParseRequest, RagConfig, RagError, TantivyFulltext};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::service::PurgeOutcome;
use storage::{IndexerStorage, Node, ObjectStore, S3Config, S3ObjectStore, StorageService};
use uuid::Uuid;

/// 文章のファイルの中身（取り込みが UTF-8 にして渡す・D-118）を段落に分けるパーサ。
struct TextParser {
    http: reqwest::Client,
}

#[async_trait]
impl DocumentParser for TextParser {
    async fn parse(
        &self,
        _ctx: &AuthContext,
        req: ParseRequest<'_>,
    ) -> Result<ParsedDocument, RagError> {
        let bytes = match req.source {
            rag::ParseSource::Bytes(b) => b.to_vec(),
            rag::ParseSource::Url(u) => self.http.get(u).send().await?.bytes().await?.to_vec(),
        };
        let text = String::from_utf8(bytes).map_err(|e| RagError::Parse {
            code: "parse_failed".into(),
            detail: e.to_string(),
        })?;
        Ok(ParsedDocument {
            blocks: text
                .split("\n\n")
                .filter(|p| !p.trim().is_empty())
                .map(|p| ParsedBlock {
                    block_type: BlockType::Paragraph,
                    level: None,
                    text: p.trim().to_string(),
                    page: None,
                    prov: Vec::new(),
                    list_marker: None,
                })
                .collect(),
            used_ocr: false,
        })
    }
}

struct Env {
    pool: PgPool,
    service: StorageService,
    authz: Arc<dyn AuthzClient>,
    deps: Arc<PipelineDeps>,
    vector: Arc<FakeVectorStore>,
    alice: AuthContext,
    _tmp: tempfile::TempDir,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(fga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let http = reqwest::Client::new();
    let fga = OpenFgaClient::connect(
        http.clone(),
        &OpenFgaConfig {
            base_url: fga_url,
            store_name: format!("shiki-purge-index-it-{}", Uuid::new_v4().simple()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let s3 = S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    store.ensure_bucket().await.expect("バケット準備");
    let service = StorageService::new(
        pool.clone(),
        Arc::clone(&store),
        Arc::clone(&authz),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    );
    // テナントはテストごとに一意にし、relay は自分のテナントの outbox だけを流す（BUG-159）。
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        "acme".into(),
        authz::TenantId::parse(&tenant).unwrap(),
    );
    let org_obj = alice.ns().organization(alice.org());
    authz
        .write_tuple(&alice.subject(), Relation::Member, &org_obj)
        .await
        .unwrap();
    wait_check(&authz, &alice, Relation::Member, &org_obj).await;
    let tmp = tempfile::tempdir().unwrap();
    let vector = Arc::new(FakeVectorStore::default());
    let deps = Arc::new(PipelineDeps {
        pool: pool.clone(),
        config: RagConfig {
            enabled: true,
            consumer_concurrency: 2,
            ..RagConfig::default()
        },
        parser: Arc::new(TextParser { http }),
        embedder: Arc::new(FakeEmbedder),
        vector: Arc::clone(&vector) as _,
        fulltext: Arc::new(TantivyFulltext::new(tmp.path())),
        indexer_storage: Arc::new(IndexerStorage::new(pool.clone(), store)),
    });
    Some(Env {
        pool,
        service,
        authz,
        deps,
        vector,
        alice,
        _tmp: tmp,
    })
}

async fn wait_check(
    authz: &Arc<dyn AuthzClient>,
    ctx: &AuthContext,
    relation: Relation,
    obj: &authz::FgaObject,
) {
    for _ in 0..200 {
        if authz
            .check(
                &ctx.subject(),
                relation,
                obj,
                Consistency::HigherConsistency,
            )
            .await
            .unwrap()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("seed したタプルが見えませんでした");
}

/// relay → 取り込みを、自分のテナントのイベントとジョブが無くなるまで回す。
///
/// 取り込みは `consumer::consume_once`（全テナントのジョブを取る）ではなく、自分のテナントのジョブだけを取って
/// `indexer::handle` に渡す。並ぶ別のテストのプロセスが取ると、そのプロセスのベクタ・全文の索引に書かれて、
/// こちらの確かめが合わなくなるため。
async fn drain(env: &Env) {
    for _ in 0..30 {
        relay::relay_tenant_once(&env.pool, &env.deps.config, env.alice.tenant_id())
            .await
            .unwrap();
        let mut tx = env.pool.begin().await.unwrap();
        let jobs: Vec<(i64, serde_json::Value)> = sqlx::query_as(
            "select id, payload from job_queue where queue = $1 and tenant_id = $2 \
             order by id for update skip locked",
        )
        .bind(RAG_INGEST_QUEUE)
        .bind(env.alice.tenant_id())
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        for (id, payload) in &jobs {
            let message: IngestMessage = serde_json::from_value(payload.clone()).unwrap();
            indexer::handle(&env.deps, &message, None)
                .await
                .unwrap_or_else(|e| panic!("取り込み {message:?}: {e}"));
            sqlx::query("delete from job_queue where tenant_id = $1 and id = $2")
                .bind(env.alice.tenant_id())
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let unrelayed: i64 = sqlx::query_scalar(
            "select count(*) from storage_event_outbox where tenant_id = $1 and processed_at is null",
        )
        .bind(env.alice.tenant_id())
        .fetch_one(&env.pool)
        .await
        .unwrap();
        if jobs.is_empty() && unrelayed == 0 {
            return;
        }
    }
    panic!("パイプラインが収束しませんでした");
}

async fn write(env: &Env, parent: Option<Uuid>, name: &str, text: &str) -> Node {
    let node = env
        .service
        .write_file_internal(
            &env.alice,
            parent,
            name,
            text.as_bytes(),
            "text/plain",
            None,
        )
        .await
        .unwrap();
    wait_check(
        &env.authz,
        &env.alice,
        Relation::Editor,
        &env.alice.ns().file(&node.id.to_string()),
    )
    .await;
    node
}

/// ノードの索引の残り（rag_chunk の行・doc_block の行・ベクタの点・全文のヒット）。
async fn index_left(env: &Env, node: Uuid, word: &str) -> (i64, i64, usize, usize) {
    let chunks: i64 =
        sqlx::query_scalar("select count(*) from rag_chunk where tenant_id = $1 and node_id = $2")
            .bind(env.alice.tenant_id())
            .bind(node)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    let blocks: i64 =
        sqlx::query_scalar("select count(*) from doc_block where tenant_id = $1 and node_id = $2")
            .bind(env.alice.tenant_id())
            .bind(node)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    let points = env
        .vector
        .points
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, p)| t == env.alice.tenant_id() && p.node_id == node)
        .count();
    let hits = env
        .deps
        .fulltext
        .search(&env.alice, word, 50, &PreFilter::TenantOnly, &[], &[])
        .unwrap()
        .into_iter()
        .filter(|h| h.node_id == node)
        .count();
    (chunks, blocks, points, hits)
}

/// 論理削除のイベントを取り込む前に完全削除しても、配下のファイルの索引がすべて消える。ほかのファイルの索引は残る。
#[tokio::test]
async fn purge_before_soft_delete_is_ingested_leaves_no_index() {
    let Some(env) = setup().await else { return };
    let dir = env
        .service
        .create_folder(&env.alice, None, "見積もり", None)
        .await
        .unwrap();
    wait_check(
        &env.authz,
        &env.alice,
        Relation::Editor,
        &env.alice.ns().folder(&dir.id.to_string()),
    )
    .await;
    let a = write(
        &env,
        Some(dir.id),
        "a.txt",
        "見積書の本文です。\n\n消える段落です。",
    )
    .await;
    let b = write(&env, Some(dir.id), "b.txt", "見積書の別紙です。").await;
    let keep = write(&env, None, "keep.txt", "見積書の控えです。").await;
    drain(&env).await;
    for n in [&a, &b, &keep] {
        let (chunks, blocks, points, hits) = index_left(&env, n.id, "見積書").await;
        assert!(
            chunks > 0 && blocks > 0 && points > 0 && hits > 0,
            "取り込まれている: {} {chunks} {blocks} {points} {hits}",
            n.name
        );
    }

    env.service
        .soft_delete_folder(&env.alice, dir.id, None)
        .await
        .unwrap();
    assert!(matches!(
        env.service.purge_node(&env.alice, dir.id, None).await,
        Ok(PurgeOutcome::Purged(_))
    ));
    drain(&env).await;
    for n in [&a, &b] {
        assert_eq!(
            index_left(&env, n.id, "見積書").await,
            (0, 0, 0, 0),
            "完全に削除したファイルの索引が残らない: {}",
            n.name
        );
    }
    let (chunks, _, points, hits) = index_left(&env, keep.id, "見積書").await;
    assert!(
        chunks > 0 && points > 0 && hits > 0,
        "ほかのファイルの索引は残る"
    );
}

/// 完全削除の delete は、論理削除の delete と取り込みの重複除けの鍵が重ならない。論理削除の delete を済ませた後に
/// 残った索引（取り込みの遅れなど）も、完全削除で消える。
#[tokio::test]
async fn purge_delete_does_not_collide_with_soft_delete_job() {
    let Some(env) = setup().await else { return };
    let f = write(&env, None, "memo.txt", "議事録の本文です。").await;
    drain(&env).await;
    env.service
        .soft_delete_file(&env.alice, f.id, None)
        .await
        .unwrap();
    drain(&env).await;
    assert_eq!(index_left(&env, f.id, "議事録").await.0, 0);
    let deleted_version: i64 =
        sqlx::query_scalar("select version from node where tenant_id = $1 and id = $2")
            .bind(env.alice.tenant_id())
            .bind(f.id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    // 論理削除の後に残った索引の行（遅れて書かれたものに見立てる）。
    sqlx::query(
        "insert into rag_chunk (id, tenant_id, org, node_id, version, kind, ordinal, content, char_count, authz_tags) \
         values ($1, $2, $3, $4, 1, 'leaf', 0, '議事録の残り', 6, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(env.alice.tenant_id())
    .bind(env.alice.org())
    .bind(f.id)
    .bind(vec![env.alice.ns().file(&f.id.to_string()).as_str().to_string()])
    .execute(&env.pool)
    .await
    .unwrap();

    assert!(matches!(
        env.service.purge_node(&env.alice, f.id, None).await,
        Ok(PurgeOutcome::Purged(_))
    ));
    drain(&env).await;
    assert_eq!(index_left(&env, f.id, "議事録").await, (0, 0, 0, 0));
    let jobs: Vec<(i64, String)> = sqlx::query_as(
        "select version, status from rag_ingest_job \
         where tenant_id = $1 and node_id = $2 and op = 'delete' order by version",
    )
    .bind(env.alice.tenant_id())
    .bind(f.id)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        jobs,
        vec![
            (deleted_version, "succeeded".to_string()),
            (deleted_version + 1, "succeeded".to_string()),
        ],
        "論理削除と完全削除の delete が別のジョブとして処理される"
    );
}
