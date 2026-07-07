//! インジェスト・パイプラインの結合テスト（Task 2.8/2.9 受入条件・実 Postgres が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定なら early-return skip）。
//! parser / embedder / vector store はフェイク、全文索引は実 Tantivy（tempdir）で、
//! outbox → relay → jobq → consumer → 索引 3 系統の実経路を検証する:
//! - アップロード（create イベント）から検索可能になる
//! - 同一版の二重インジェストが起きない（冪等）
//! - 恒久パース失敗は即 DLQ・一時失敗はリトライ経由で DLQ に落ち再実行できる
//! - move で authz_tags が再評価される・delete で全索引から消える

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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::{AuthContext, Principal};
use rag::embedding::{EmbedInput, EmbedResponse, EmbeddingProvider};
use rag::pipeline::{consumer, relay, PipelineDeps, RAG_INGEST_QUEUE};
use rag::types::{BlockType, ParsedBlock, ParsedDocument};
use rag::vector_store::{ChunkPoint, PreFilter, ScoredChunk, VectorSearch, VectorStore};
use rag::{DocumentParser, ParseRequest, RagConfig, RagError, TantivyFulltext};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::event::{emit_on, WriteEvent};
use storage::{IndexerStorage, ObjectStore, ObjectStoreError, WriteOp};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// フェイク実装
// ---------------------------------------------------------------------------

/// 決定的フェイク・パーサ（呼び出し回数を数え、指定回数まで失敗もできる）。
struct FakeParser {
    calls: AtomicUsize,
    /// Some(n) なら先頭 n 回を一時エラーにする。None は常に成功。
    transient_failures: Option<usize>,
    /// true なら常に恒久パース失敗（422 相当）。
    permanent_failure: bool,
}

impl FakeParser {
    fn ok() -> Self {
        FakeParser {
            calls: AtomicUsize::new(0),
            transient_failures: None,
            permanent_failure: false,
        }
    }
}

#[async_trait]
impl DocumentParser for FakeParser {
    async fn parse(
        &self,
        _ctx: &AuthContext,
        req: ParseRequest<'_>,
    ) -> Result<ParsedDocument, RagError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.permanent_failure {
            return Err(RagError::Parse {
                code: "parse_failed".into(),
                detail: "壊れたファイル".into(),
            });
        }
        if let Some(n) = self.transient_failures {
            if call < n {
                return Err(RagError::Worker("一時的に落ちています".into()));
            }
        }
        // 見出しのみ（埋め込み対象ゼロ）の文書を名前で再現できるようにする。
        if req.file_name.starts_with("headings-only") {
            return Ok(ParsedDocument {
                blocks: vec![ParsedBlock {
                    block_type: BlockType::Heading,
                    level: Some(1),
                    text: "見出しだけの文書".into(),
                    page: Some(1),
                }],
                used_ocr: false,
            });
        }
        Ok(ParsedDocument {
            blocks: vec![
                ParsedBlock {
                    block_type: BlockType::Heading,
                    level: Some(1),
                    text: format!("{} の報告", req.file_name),
                    page: Some(1),
                },
                ParsedBlock {
                    block_type: BlockType::Paragraph,
                    level: None,
                    text: "四半期売上は好調に推移した。".into(),
                    page: Some(1),
                },
            ],
            used_ocr: false,
        })
    }
}

/// 決定的フェイク埋め込み（sha256 → 正規化 8 次元）。
struct FakeEmbedder;

#[async_trait]
impl EmbeddingProvider for FakeEmbedder {
    async fn embed(
        &self,
        _ctx: &AuthContext,
        _input: EmbedInput,
        texts: &[String],
    ) -> Result<EmbedResponse, RagError> {
        let vectors = texts.iter().map(|t| fake_vector(t)).collect();
        Ok(EmbedResponse {
            vectors,
            model_version: "fake-model".into(),
            dimension: 8,
        })
    }

    fn model_version(&self) -> &str {
        "fake-model"
    }
}

fn fake_vector(text: &str) -> Vec<f32> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut v: Vec<f32> = (0..8u64)
        .map(|i| {
            let mut h = DefaultHasher::new();
            (text, i).hash(&mut h);
            (h.finish() % 1000) as f32 / 1000.0 + 0.001
        })
        .collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in &mut v {
        *x /= norm;
    }
    v
}

/// インメモリ VectorStore（tenant 無条件 AND を含む本物と同じフィルタ意味論）。
#[derive(Default)]
struct FakeVectorStore {
    points: Mutex<Vec<(String, ChunkPoint)>>, // (tenant_id, point)
}

#[async_trait]
impl VectorStore for FakeVectorStore {
    async fn ensure_ready(&self, _dimension: usize) -> Result<(), RagError> {
        Ok(())
    }
    async fn upsert(&self, ctx: &AuthContext, points: &[ChunkPoint]) -> Result<(), RagError> {
        let mut store = self.points.lock().unwrap();
        for p in points {
            store.retain(|(_, q)| q.chunk_id != p.chunk_id);
            store.push((
                ctx.tenant_id.clone(),
                ChunkPoint {
                    chunk_id: p.chunk_id,
                    node_id: p.node_id,
                    version: p.version,
                    vector: p.vector.clone(),
                    authz_tags: p.authz_tags.clone(),
                },
            ));
        }
        Ok(())
    }
    async fn delete_node(&self, ctx: &AuthContext, node_id: Uuid) -> Result<(), RagError> {
        self.points
            .lock()
            .unwrap()
            .retain(|(t, p)| !(t == &ctx.tenant_id && p.node_id == node_id));
        Ok(())
    }
    async fn delete_stale_versions(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        keep_version: i64,
    ) -> Result<(), RagError> {
        self.points.lock().unwrap().retain(|(t, p)| {
            !(t == &ctx.tenant_id && p.node_id == node_id && p.version != keep_version)
        });
        Ok(())
    }
    async fn set_authz_tags(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        tags: &[String],
    ) -> Result<(), RagError> {
        for (t, p) in self.points.lock().unwrap().iter_mut() {
            if t == &ctx.tenant_id && p.node_id == node_id {
                p.authz_tags = tags.to_vec();
            }
        }
        Ok(())
    }
    async fn search(
        &self,
        ctx: &AuthContext,
        query: &VectorSearch<'_>,
    ) -> Result<Vec<ScoredChunk>, RagError> {
        let store = self.points.lock().unwrap();
        let mut hits: Vec<ScoredChunk> = store
            .iter()
            // tenant 無条件 AND（本物と同じ意味論）。
            .filter(|(t, _)| t == &ctx.tenant_id)
            .filter(|(_, p)| match query.prefilter {
                PreFilter::TenantOnly => true,
                PreFilter::Tags(tags) => p.authz_tags.iter().any(|t| tags.contains(t)),
            })
            .filter(|(_, p)| !query.exclude.contains(&p.chunk_id))
            .map(|(_, p)| ScoredChunk {
                chunk_id: p.chunk_id,
                node_id: p.node_id,
                score: p.vector.iter().zip(query.vector).map(|(a, b)| a * b).sum(),
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(query.limit);
        Ok(hits)
    }
    async fn purge_tenant(&self, tenant_id: &str) -> Result<(), RagError> {
        self.points.lock().unwrap().retain(|(t, _)| t != tenant_id);
        Ok(())
    }
}

/// presign だけ機能するフェイク ObjectStore（バイトは FakeParser が読まないため不要）。
struct FakeObjectStore;

#[async_trait]
impl ObjectStore for FakeObjectStore {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(
        &self,
        _key: &str,
        _ttl: Duration,
        _len: i64,
    ) -> Result<String, ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn presign_get(
        &self,
        _key: &str,
        _ttl: Duration,
        _filename: Option<&str>,
        _content_type: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn presign_get_internal(
        &self,
        key: &str,
        _ttl: Duration,
    ) -> Result<String, ObjectStoreError> {
        Ok(format!("http://fake-minio/{key}"))
    }
    async fn read_and_hash(&self, _key: &str) -> Result<(String, u64), ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn put_object(
        &self,
        _key: &str,
        _bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<(), ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn get_object(&self, _key: &str) -> Result<Vec<u8>, ObjectStoreError> {
        unreachable!("テストでは使わない")
    }
    async fn exists(&self, _key: &str) -> Result<bool, ObjectStoreError> {
        Ok(true)
    }
    async fn copy(&self, _src: &str, _dst: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _prefix: &str,
        _continuation: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _keys: &[String]) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// セットアップ
// ---------------------------------------------------------------------------

struct TestEnv {
    pool: PgPool,
    deps: Arc<PipelineDeps>,
    ctx: AuthContext,
    _tmp: tempfile::TempDir,
    /// outbox（共有テーブル）を触るため、バイナリ内のテストを直列化するガード。
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

fn serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn test_ctx(tenant: &str) -> AuthContext {
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

async fn setup(parser: FakeParser) -> Option<TestEnv> {
    let serial = serial_lock().lock().await;
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

    // テスト分離: テナントをテストごとに一意化し、他テストの outbox 残骸と混ざらないよう
    // 未処理イベントを掃除する（このテストは relay を自分で駆動する）。
    sqlx::query("update storage_event_outbox set processed_at = now() where processed_at is null")
        .execute(&pool)
        .await
        .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let config = RagConfig {
        enabled: true,
        consumer_concurrency: 2,
        job_max_attempts: 2,
        ..RagConfig::default()
    };
    let deps = Arc::new(PipelineDeps {
        pool: pool.clone(),
        config,
        parser: Arc::new(parser),
        embedder: Arc::new(FakeEmbedder),
        vector: Arc::new(FakeVectorStore::default()),
        fulltext: Arc::new(TantivyFulltext::new(tmp.path())),
        indexer_storage: Arc::new(IndexerStorage::new(pool.clone(), Arc::new(FakeObjectStore))),
    });
    Some(TestEnv {
        pool,
        deps,
        ctx: test_ctx(&tenant),
        _tmp: tmp,
        _serial: serial,
    })
}

/// blob＋node 行を直接作り、outbox に create イベントを発行する（StorageService を
/// 経由しない最小セットアップ。実サービス経由の E2E は compose 検証で行う）。
async fn create_file_with_event(env: &TestEnv, parent: Option<Uuid>, name: &str) -> Uuid {
    let ctx = &env.ctx;
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
        "insert into node (org, tenant_id, kind, name, parent_id, blob_sha256, size_bytes, \
                           content_type, created_by) \
         values ($1, $2, 'file', $3, $4, $5, 10, 'text/plain', $6) returning id",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(name)
    .bind(parent)
    .bind(&sha)
    .bind(&ctx.principal.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    // closure: 自分自身（depth 0）＋親フォルダ（あれば）。
    sqlx::query(
        "insert into node_closure (org, tenant_id, ancestor, descendant, depth) \
         values ($1, $2, $3, $3, 0)",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(node_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    if let Some(p) = parent {
        sqlx::query(
            "insert into node_closure (org, tenant_id, ancestor, descendant, depth) \
             values ($1, $2, $3, $4, 1)",
        )
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(p)
        .bind(node_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    emit_on(
        &mut tx,
        ctx,
        WriteEvent {
            node_id,
            version: 1,
            op: WriteOp::Create,
            payload: serde_json::json!({}),
        },
        Some("trace-test"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    node_id
}

/// フォルダ node を作る（closure 自己行つき）。
async fn create_folder(env: &TestEnv, name: &str) -> Uuid {
    let ctx = &env.ctx;
    let mut tx = env.pool.begin().await.unwrap();
    let id: Uuid = sqlx::query_scalar(
        "insert into node (org, tenant_id, kind, name, created_by) \
         values ($1, $2, 'folder', $3, $4) returning id",
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

/// relay → consumer を、キューが空になるまで回す。
async fn drain_pipeline(env: &TestEnv) {
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
            let queued: i64 = sqlx::query_scalar("select count(*) from job_queue where queue = $1")
                .bind(RAG_INGEST_QUEUE)
                .fetch_one(&env.pool)
                .await
                .unwrap();
            if queued == 0 {
                return;
            }
        }
    }
}

async fn job_status(env: &TestEnv, node: Uuid, op: &str) -> Option<(String, Option<String>)> {
    sqlx::query_as(
        "select status, last_error from rag_ingest_job \
         where tenant_id = $1 and node_id = $2 and op = $3",
    )
    .bind(&env.ctx.tenant_id)
    .bind(node)
    .bind(op)
    .fetch_optional(&env.pool)
    .await
    .unwrap()
}

fn fulltext_hits(env: &TestEnv, query: &str, prefilter: &PreFilter) -> Vec<ScoredChunk> {
    env.deps
        .fulltext
        .search(&env.ctx, query, 10, prefilter, &[])
        .unwrap()
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upload_event_flows_to_all_indexes() {
    let Some(env) = setup(FakeParser::ok()).await else {
        return;
    };
    let folder = create_folder(&env, "sales").await;
    let node = create_file_with_event(&env, Some(folder), "q1-report.txt").await;

    drain_pipeline(&env).await;

    // rag_chunk（正本）: 親 1 ＋ leaf 1、authz_tags は file 自身＋祖先フォルダ。
    let chunks: Vec<(String, Vec<String>)> = sqlx::query_as(
        "select kind, authz_tags from rag_chunk where tenant_id = $1 and node_id = $2 \
         order by ordinal",
    )
    .bind(&env.ctx.tenant_id)
    .bind(node)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(chunks.len(), 2, "parent + leaf");
    let expected_tags = vec![
        env.ctx.ns().file(&node.to_string()).as_str().to_string(),
        env.ctx
            .ns()
            .folder(&folder.to_string())
            .as_str()
            .to_string(),
    ];
    assert_eq!(chunks[0].1, expected_tags, "全チャンクに authz_tags が付く");

    // 全文索引にヒットする（形態素）。
    assert_eq!(fulltext_hits(&env, "売上", &PreFilter::TenantOnly).len(), 1);
    // dense 側もタグ付きで登録されている。
    let hits = env
        .deps
        .vector
        .search(
            &env.ctx,
            &VectorSearch {
                vector: &fake_vector("dummy"),
                limit: 10,
                prefilter: &PreFilter::Tags(expected_tags),
                exclude: &[],
            },
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);

    let (status, _) = job_status(&env, node, "create").await.unwrap();
    assert_eq!(status, "succeeded");
}

#[tokio::test]
async fn duplicate_events_do_not_double_ingest() {
    let Some(env) = setup(FakeParser::ok()).await else {
        return;
    };
    let node = create_file_with_event(&env, None, "dup.txt").await;
    // 同一 (node, version) のイベントをもう一度発行（at-least-once の再配信を模す）。
    let mut tx = env.pool.begin().await.unwrap();
    emit_on(
        &mut tx,
        &env.ctx,
        WriteEvent {
            node_id: node,
            version: 1,
            op: WriteOp::Create,
            payload: serde_json::json!({}),
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    drain_pipeline(&env).await;

    let count: i64 =
        sqlx::query_scalar("select count(*) from rag_chunk where tenant_id = $1 and node_id = $2")
            .bind(&env.ctx.tenant_id)
            .bind(node)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(count, 2, "重複インジェストでもチャンクは 1 セットのみ");
    assert_eq!(
        fulltext_hits(&env, "売上", &PreFilter::TenantOnly).len(),
        1,
        "全文側も重複しない"
    );
    // 2 回目のジョブは already-processed skip として成功扱いで消化される。
    let (status, _) = job_status(&env, node, "create").await.unwrap();
    assert_eq!(status, "succeeded");
}

#[tokio::test]
async fn permanent_parse_failure_goes_straight_to_dlq_and_requeue_recovers() {
    let Some(env) = setup(FakeParser {
        calls: AtomicUsize::new(0),
        transient_failures: None,
        permanent_failure: true,
    })
    .await
    else {
        return;
    };
    let node = create_file_with_event(&env, None, "broken.txt").await;
    drain_pipeline(&env).await;

    // 恒久エラー: リトライせず即 DLQ・rag_ingest_job は dead。
    let (status, last_error) = job_status(&env, node, "create").await.unwrap();
    assert_eq!(status, "dead");
    assert!(
        last_error.unwrap().contains("パース失敗"),
        "エラーが記録される"
    );
    let mut conn = env.pool.acquire().await.unwrap();
    let dead = jobq::dead_jobs(&mut conn, RAG_INGEST_QUEUE, 10)
        .await
        .unwrap();
    let dead_job = dead
        .iter()
        .find(|d| d.tenant_id == env.ctx.tenant_id)
        .expect("DLQ に入る");

    // 再実行（requeue）: 今度も失敗するが「DLQ から再投入できる」経路を検証。
    assert!(jobq::requeue_dead(&mut conn, dead_job.id).await.unwrap());
    drop(conn);
    drain_pipeline(&env).await;
    let (status, _) = job_status(&env, node, "create").await.unwrap();
    assert_eq!(status, "dead", "再実行も失敗し再び dead（経路は機能）");
}

#[tokio::test]
async fn transient_failures_retry_until_attempts_exhausted() {
    // max_attempts=2・毎回一時エラー → 2 回試行後 DLQ。
    let Some(env) = setup(FakeParser {
        calls: AtomicUsize::new(0),
        transient_failures: Some(usize::MAX),
        permanent_failure: false,
    })
    .await
    else {
        return;
    };
    let node = create_file_with_event(&env, None, "flaky.txt").await;

    relay::relay_once(&env.pool, &env.deps.config)
        .await
        .unwrap();
    // 1 回目: 失敗 → バックオフ再配信待ち。バックオフを 0 に潰して即再試行できるようにする。
    consumer::consume_once(&env.deps).await.unwrap();
    sqlx::query("update job_queue set visible_at = now() where queue = $1")
        .bind(RAG_INGEST_QUEUE)
        .execute(&env.pool)
        .await
        .unwrap();
    // 2 回目: attempts=2 >= max_attempts=2 → DLQ。
    consumer::consume_once(&env.deps).await.unwrap();

    let (status, _) = job_status(&env, node, "create").await.unwrap();
    assert_eq!(status, "dead");
    let mut conn = env.pool.acquire().await.unwrap();
    let dead = jobq::dead_jobs(&mut conn, RAG_INGEST_QUEUE, 10)
        .await
        .unwrap();
    assert!(dead.iter().any(|d| d.tenant_id == env.ctx.tenant_id));
}

#[tokio::test]
async fn move_reevaluates_tags_and_delete_removes_everywhere() {
    let Some(env) = setup(FakeParser::ok()).await else {
        return;
    };
    let folder_old = create_folder(&env, "old").await;
    let folder_new = create_folder(&env, "new").await;
    let node = create_file_with_event(&env, Some(folder_old), "moving.txt").await;
    drain_pipeline(&env).await;

    let old_tag = env
        .ctx
        .ns()
        .folder(&folder_old.to_string())
        .as_str()
        .to_string();
    let new_tag = env
        .ctx
        .ns()
        .folder(&folder_new.to_string())
        .as_str()
        .to_string();
    assert_eq!(
        fulltext_hits(&env, "売上", &PreFilter::Tags(vec![old_tag.clone()])).len(),
        1
    );

    // move: closure を差し替えて move イベントを発行（StorageService の move 相当）。
    let mut tx = env.pool.begin().await.unwrap();
    sqlx::query("delete from node_closure where descendant = $1 and depth > 0")
        .bind(node)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "insert into node_closure (org, tenant_id, ancestor, descendant, depth) \
         values ($1, $2, $3, $4, 1)",
    )
    .bind(&env.ctx.org)
    .bind(&env.ctx.tenant_id)
    .bind(folder_new)
    .bind(node)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("update node set parent_id = $2, version = version + 1 where id = $1")
        .bind(node)
        .bind(folder_new)
        .execute(&mut *tx)
        .await
        .unwrap();
    emit_on(
        &mut tx,
        &env.ctx,
        WriteEvent {
            node_id: node,
            version: 2,
            op: WriteOp::Move,
            payload: serde_json::json!({}),
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drain_pipeline(&env).await;

    // 共有変更（move）で authz_tags が再評価される（Task 2.9 受入条件）。
    assert!(
        fulltext_hits(&env, "売上", &PreFilter::Tags(vec![old_tag])).is_empty(),
        "旧フォルダの viewer からは消える"
    );
    assert_eq!(
        fulltext_hits(&env, "売上", &PreFilter::Tags(vec![new_tag.clone()])).len(),
        1,
        "新フォルダの viewer に見える"
    );
    let db_tags: Vec<String> =
        sqlx::query_scalar("select distinct unnest(authz_tags) from rag_chunk where node_id = $1")
            .bind(node)
            .fetch_all(&env.pool)
            .await
            .unwrap();
    assert!(db_tags.contains(&new_tag));

    // delete: 全索引から消える（Task 2.9 受入条件）。
    let mut tx = env.pool.begin().await.unwrap();
    sqlx::query("update node set deleted_at = now(), version = version + 1 where id = $1")
        .bind(node)
        .execute(&mut *tx)
        .await
        .unwrap();
    emit_on(
        &mut tx,
        &env.ctx,
        WriteEvent {
            node_id: node,
            version: 3,
            op: WriteOp::Delete,
            payload: serde_json::json!({}),
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drain_pipeline(&env).await;

    assert!(fulltext_hits(&env, "売上", &PreFilter::TenantOnly).is_empty());
    let count: i64 = sqlx::query_scalar("select count(*) from rag_chunk where node_id = $1")
        .bind(node)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let vec_hits = env
        .deps
        .vector
        .search(
            &env.ctx,
            &VectorSearch {
                vector: &fake_vector("dummy"),
                limit: 10,
                prefilter: &PreFilter::TenantOnly,
                exclude: &[],
            },
        )
        .await
        .unwrap();
    assert!(vec_hits.is_empty());
}

#[tokio::test]
async fn rename_is_noop_for_indexes() {
    let Some(env) = setup(FakeParser::ok()).await else {
        return;
    };
    let node = create_file_with_event(&env, None, "before.txt").await;
    drain_pipeline(&env).await;

    let mut tx = env.pool.begin().await.unwrap();
    emit_on(
        &mut tx,
        &env.ctx,
        WriteEvent {
            node_id: node,
            version: 2,
            op: WriteOp::Rename,
            payload: serde_json::json!({}),
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drain_pipeline(&env).await;

    let (status, _) = job_status(&env, node, "rename").await.unwrap();
    assert_eq!(
        status, "skipped",
        "rename は索引 no-op（名前は検索時 JOIN）"
    );
    assert_eq!(fulltext_hits(&env, "売上", &PreFilter::TenantOnly).len(), 1);
}

#[tokio::test]
async fn folder_move_fans_out_to_descendant_files() {
    let Some(env) = setup(FakeParser::ok()).await else {
        return;
    };
    let folder_a = create_folder(&env, "移動元").await;
    let folder_b = create_folder(&env, "移動先").await;
    let node = create_file_with_event(&env, Some(folder_a), "nested.txt").await;
    drain_pipeline(&env).await;

    let tag_b = env
        .ctx
        .ns()
        .folder(&folder_b.to_string())
        .as_str()
        .to_string();
    assert!(
        fulltext_hits(&env, "売上", &PreFilter::Tags(vec![tag_b.clone()])).is_empty(),
        "移動前は移動先フォルダの viewer からは見えない"
    );

    // フォルダ A を B 配下へ移動（closure 差し替え）し、**フォルダの** move イベントを発行。
    let mut tx = env.pool.begin().await.unwrap();
    sqlx::query(
        "insert into node_closure (org, tenant_id, ancestor, descendant, depth) \
         values ($1, $2, $3, $4, 1), ($1, $2, $3, $5, 2)",
    )
    .bind(&env.ctx.org)
    .bind(&env.ctx.tenant_id)
    .bind(folder_b)
    .bind(folder_a)
    .bind(node)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("update node set parent_id = $2, version = version + 1 where id = $1")
        .bind(folder_a)
        .bind(folder_b)
        .execute(&mut *tx)
        .await
        .unwrap();
    emit_on(
        &mut tx,
        &env.ctx,
        WriteEvent {
            node_id: folder_a,
            version: 2,
            op: WriteOp::Move,
            payload: serde_json::json!({}),
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drain_pipeline(&env).await;

    // フォルダイベントが子孫ファイルへ展開され、authz_tags に移動先の祖先が乗る。
    assert_eq!(
        fulltext_hits(&env, "売上", &PreFilter::Tags(vec![tag_b])).len(),
        1,
        "フォルダ move の子孫展開でタグ再評価される"
    );
    let (status, _) = job_status(&env, node, "move").await.unwrap();
    assert_eq!(status, "succeeded");
}

#[tokio::test]
async fn stale_delete_event_does_not_wipe_restored_node() {
    let Some(env) = setup(FakeParser::ok()).await else {
        return;
    };
    let node = create_file_with_event(&env, None, "resilient.txt").await;
    drain_pipeline(&env).await;
    assert_eq!(fulltext_hits(&env, "売上", &PreFilter::TenantOnly).len(), 1);

    // 「削除 → 復元」後に古い delete イベントが遅延到着したケースを再現する:
    // 現行版を進めて生存させたまま、旧版の delete を投げる。
    sqlx::query("update node set version = 3 where id = $1")
        .bind(node)
        .execute(&env.pool)
        .await
        .unwrap();
    let mut tx = env.pool.begin().await.unwrap();
    emit_on(
        &mut tx,
        &env.ctx,
        WriteEvent {
            node_id: node,
            version: 1,
            op: WriteOp::Delete,
            payload: serde_json::json!({}),
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drain_pipeline(&env).await;

    assert_eq!(
        fulltext_hits(&env, "売上", &PreFilter::TenantOnly).len(),
        1,
        "生きている索引が古い delete で消えない"
    );
    let (status, _) = job_status(&env, node, "delete").await.unwrap();
    assert_eq!(status, "skipped");
}

#[tokio::test]
async fn headings_only_document_succeeds_without_vectors() {
    let Some(env) = setup(FakeParser::ok()).await else {
        return;
    };
    let node = create_file_with_event(&env, None, "headings-only.txt").await;
    drain_pipeline(&env).await;

    // 埋め込み対象ゼロでも失敗せず succeeded（dimension=0 の collection 初期化を避ける）。
    let (status, err) = job_status(&env, node, "create").await.unwrap();
    assert_eq!(status, "succeeded", "err={err:?}");
    let queued: i64 = sqlx::query_scalar("select count(*) from job_queue where queue = $1")
        .bind(RAG_INGEST_QUEUE)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(queued, 0, "リトライ/DLQ に落ちない");
}
