//! 社内文書の検索への取り込み直しの結合テスト（D-126・GAP-51）。
//!
//! 実 StorageService（実 OpenFGA・実 RustFS）でファイルを置き、取り込みの記録が無い状態（社内文書の検索を有効に
//! する前に置いた・前の版の不具合で取り込みを飛ばした）を作ってから、取り込み直しの依頼が outbox → relay →
//! job_queue → 取り込み（rag_ingest_job）の既存の経路を通って本文を索引に入れることを確かめる。
//!
//! - ファイルごと: 編集できる人だけが頼める（見えるだけの人は Forbidden、見えない人は NotFound・拒否は監査に
//!   残る）。記録が無いファイルと取り込みに失敗したファイルだけを積み、取り込み中・取り込み済みは積まない（同時に
//!   押しても 1 件）。取り込めない種類・取り込まない場所は理由つきで断る。
//! - まとめて: 管理者が始めると、取り込みの記録が無いファイルだけを、テナントの待ち行列が溜まらないよう少しずつ
//!   積む（待っている作業が上限に届いたら積まない）。進み具合（確かめた数・回した数・待っている件数）が分かる。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が無いとスキップする。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines,
    clippy::cognitive_complexity
)]

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Relation};
use common::{test_ctx, FakeEmbedder, FakeReranker, FakeVectorStore};
use rag::pipeline::backfill::{self, StepOutcome, MAX_WAITING};
use rag::pipeline::{consumer, relay, PipelineDeps, RAG_INGEST_QUEUE};
use rag::types::{BlockType, ParsedBlock, ParsedDocument};
use rag::{
    BackfillState, DocumentParser, IndexState, NotIndexable, ParseRequest, RagConfig, RagError,
    ReindexOutcome, SearchMode, SearchService, TantivyFulltext,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{
    IndexerStorage, Node, ObjectStore, S3Config, S3ObjectStore, ShareRole, ShareTarget,
    StorageError, StorageService,
};
use uuid::Uuid;

/// 受け取った本文を段落に分けるパーサ（取り込んだファイル名を数える）。`fail` が立っている間は、名前が
/// `broken` で始まるファイルを恒久的な失敗（壊れたファイル）にする。
struct CountingParser {
    seen: Mutex<Vec<String>>,
    fail: AtomicBool,
}

#[async_trait]
impl DocumentParser for CountingParser {
    async fn parse(
        &self,
        _ctx: &AuthContext,
        req: ParseRequest<'_>,
    ) -> Result<ParsedDocument, RagError> {
        self.seen.lock().unwrap().push(req.file_name.to_string());
        if self.fail.load(Ordering::SeqCst) && req.file_name.starts_with("broken") {
            return Err(RagError::Parse {
                code: "parse_failed".into(),
                detail: "壊れたファイル".into(),
            });
        }
        let text = match req.source {
            rag::ParseSource::Bytes(bytes) => String::from_utf8_lossy(bytes).to_string(),
            rag::ParseSource::Url(_) => format!("{} の本文", req.file_name),
        };
        Ok(ParsedDocument {
            blocks: vec![ParsedBlock {
                block_type: BlockType::Paragraph,
                level: None,
                text,
                page: Some(1),
                prov: Vec::new(),
                list_marker: None,
            }],
            used_ocr: false,
        })
    }
}

struct Env {
    pool: PgPool,
    storage: StorageService,
    deps: Arc<PipelineDeps>,
    parser: Arc<CountingParser>,
    search: Arc<SearchService>,
    alice: AuthContext,
    /// 見えるだけ（viewer で共有された）人。
    bob: AuthContext,
    /// 組織のメンバーだが、共有されていない人。
    carol: AuthContext,
    fga_url: String,
    store_id: String,
    _tmp: tempfile::TempDir,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

fn serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn setup() -> Option<Env> {
    let serial = serial_lock().lock().await;
    let (Ok(db_url), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    // このテストは relay と consumer を自分で回す。ほかのテストの残りを取り込みへ流さない。
    sqlx::query("update storage_event_outbox set processed_at = now() where processed_at is null")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("delete from job_queue where queue = $1")
        .bind(RAG_INGEST_QUEUE)
        .execute(&pool)
        .await
        .unwrap();

    let http = reqwest::Client::new();
    let fga = OpenFgaClient::connect(
        http.clone(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("shiki-reingest-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let store_id = fga.store_id().to_string();
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
    let object_store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    object_store.ensure_bucket().await.expect("バケット準備");
    let storage = StorageService::new(
        pool.clone(),
        Arc::clone(&object_store),
        Arc::clone(&authz),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    );
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = test_ctx(&tenant, "alice");
    let bob = test_ctx(&tenant, "bob");
    let carol = test_ctx(&tenant, "carol");
    for u in [&alice, &bob, &carol] {
        authz
            .write_tuple(&u.subject(), Relation::Member, &u.ns().organization(&u.org))
            .await
            .unwrap();
    }
    let tmp = tempfile::tempdir().unwrap();
    let config = RagConfig {
        enabled: true,
        consumer_concurrency: 2,
        job_max_attempts: 1,
        ..RagConfig::default()
    };
    let vector = Arc::new(FakeVectorStore::default());
    let fulltext = Arc::new(TantivyFulltext::new(tmp.path()));
    let parser = Arc::new(CountingParser {
        seen: Mutex::new(Vec::new()),
        fail: AtomicBool::new(false),
    });
    let deps = Arc::new(PipelineDeps {
        pool: pool.clone(),
        config: config.clone(),
        parser: Arc::clone(&parser) as _,
        embedder: Arc::new(FakeEmbedder),
        vector: Arc::clone(&vector) as _,
        fulltext: Arc::clone(&fulltext) as _,
        indexer_storage: Arc::new(IndexerStorage::new(pool.clone(), object_store)),
    });
    let search = Arc::new(SearchService::new(
        pool.clone(),
        config,
        Arc::new(FakeEmbedder),
        Arc::new(FakeReranker),
        vector,
        fulltext,
        Arc::clone(&authz),
        storage::audit::AuditRecorder::new(pool.clone()),
    ));
    Some(Env {
        pool,
        storage,
        deps,
        parser,
        search,
        alice,
        bob,
        carol,
        fga_url,
        store_id,
        _tmp: tmp,
        _serial: serial,
    })
}

impl Env {
    /// alice が文章のファイルを置き、取り込みへ流さずに outbox を処理済みにする（社内文書の検索を有効にする前に
    /// 置いたファイルと同じ: 取り込みへ向かう記録がどこにも無い）。
    async fn orphan(&self, name: &str, body: &str) -> Node {
        let node = self.put(name, body.as_bytes(), "text/plain").await;
        self.forget_outbox(&node).await;
        node
    }

    async fn put(&self, name: &str, bytes: &[u8], content_type: &str) -> Node {
        self.storage
            .write_file_internal(&self.alice, None, name, bytes, content_type, None)
            .await
            .unwrap()
    }

    async fn forget_outbox(&self, node: &Node) {
        sqlx::query(
            "update storage_event_outbox set processed_at = now() \
             where tenant_id = $1 and node_id = $2 and processed_at is null",
        )
        .bind(&node.tenant_id)
        .bind(node.id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn state(&self, node: &Node) -> IndexState {
        self.search.index_state(&self.alice, node).await.unwrap()
    }

    /// `ctx` が `node` を取り込み直す（認可は StorageService・積むのは SearchService）。
    async fn reindex(
        &self,
        ctx: &AuthContext,
        node: &Node,
    ) -> Result<ReindexOutcome, StorageError> {
        let node = self.storage.authorize_reindex(ctx, node.id, None).await?;
        Ok(self.search.request_reindex(ctx, &node, None).await.unwrap())
    }

    /// 取り込み直しのイベントの数（`node` の outbox）。
    async fn reindex_events(&self, node: &Node) -> i64 {
        sqlx::query_scalar(
            "select count(*) from storage_event_outbox \
             where tenant_id = $1 and node_id = $2 and op = 'reindex'",
        )
        .bind(&node.tenant_id)
        .bind(node.id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// relay と consumer を、待ち行列が空になるまで回す。
    async fn drain(&self) {
        for _ in 0..30 {
            sqlx::query("update job_queue set visible_at = now() where queue = $1")
                .bind(RAG_INGEST_QUEUE)
                .execute(&self.pool)
                .await
                .unwrap();
            relay::relay_once(&self.pool, &self.deps.config)
                .await
                .unwrap();
            let n = consumer::consume_once(&self.deps).await.unwrap();
            if n == 0 {
                let left: i64 =
                    sqlx::query_scalar("select count(*) from job_queue where queue = $1")
                        .bind(RAG_INGEST_QUEUE)
                        .fetch_one(&self.pool)
                        .await
                        .unwrap();
                if left == 0 {
                    return;
                }
            }
        }
        panic!("取り込みが収束しませんでした");
    }

    /// 取り込んだ回数（パーサが受け取った回数）。
    fn parsed(&self, name: &str) -> usize {
        self.parser
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|n| *n == name)
            .count()
    }

    async fn found(&self, query: &str) -> Vec<Uuid> {
        self.search
            .search(
                &self.alice,
                query,
                Some(20),
                SearchMode::Keyword,
                None,
                None,
            )
            .await
            .unwrap()
            .results
            .iter()
            .map(|r| r.file_id)
            .collect()
    }

    async fn audits(&self, action: &str, decision: &str) -> i64 {
        sqlx::query_scalar(
            "select count(*) from audit_log where tenant_id = $1 and action = $2 and decision = $3",
        )
        .bind(&self.alice.tenant_id)
        .bind(action)
        .bind(decision)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// テナントの行・タプル・オブジェクトと、テストのストアを消す。
    async fn cleanup(&self) {
        let tenant = self.alice.tenant_id.clone();
        sqlx::query("delete from job_queue where tenant_id = $1")
            .bind(&tenant)
            .execute(&self.pool)
            .await
            .unwrap();
        self.storage
            .purge_tenant(&tenant, &self.alice.org, "reingest-it")
            .await
            .unwrap();
        rag::RagAdmin::new(self.pool.clone(), None, None)
            .purge_tenant(&tenant)
            .await
            .unwrap();
        sqlx::query("delete from audit_log where tenant_id = $1")
            .bind(&tenant)
            .execute(&self.pool)
            .await
            .unwrap();
        let resp = reqwest::Client::new()
            .delete(format!("{}/stores/{}", self.fga_url, self.store_id))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_success(),
            "delete store: {}",
            resp.status()
        );
    }
}

/// 記録が無いファイルは、編集できる人が取り込み直すと検索で見つかる。取り込み中・取り込み済みは積まず、同時に
/// 押しても 1 件しか積まない。見えるだけの人・見えない人は断られ、拒否は監査に残る。取り込みに失敗したファイルは
/// 取り込み直せ、取り込めない種類と取り込まない場所は理由つきで断る。
#[tokio::test]
async fn files_without_a_record_are_reingested_once_by_editors() {
    let Some(env) = setup().await else { return };
    let outcome = async {
        let memo = env.orphan("memo.txt", "四半期の議事録").await;
        assert_eq!(env.state(&memo).await, IndexState::NotIngested);
        // bob は見えるだけ（viewer）、carol は共有されていない。
        env.storage
            .share_node(
                &env.alice,
                memo.id,
                &ShareTarget::User { id: "bob".into() },
                ShareRole::Viewer,
                None,
            )
            .await
            .unwrap();
        let viewer = env.reindex(&env.bob, &memo).await;
        let stranger = env.reindex(&env.carol, &memo).await;

        let first = env.reindex(&env.alice, &memo).await.unwrap();
        let again = env.reindex(&env.alice, &memo).await.unwrap();
        let events_before_drain = env.reindex_events(&memo).await;
        let pending = env.state(&memo).await;

        // 同時に何回押しても、積むのは 1 件（ファイルごとの xact ロックの後で状態を決める）。
        let twin = env.orphan("twin.txt", "同時に押した文書").await;
        let twin_node = env
            .storage
            .authorize_reindex(&env.alice, twin.id, None)
            .await
            .unwrap();
        let twins: Vec<ReindexOutcome> = futures::future::join_all(
            (0..6).map(|_| env.search.request_reindex(&env.alice, &twin_node, None)),
        )
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
        let twin_events = env.reindex_events(&twin).await;

        env.drain().await;
        let after = env.state(&memo).await;
        let hits = env.found("議事録").await;
        let indexed = env.reindex(&env.alice, &memo).await.unwrap();

        // 取り込みに失敗した（壊れた）ファイルは、直した後に取り込み直せる。
        env.parser.fail.store(true, Ordering::SeqCst);
        let broken = env
            .put("broken.txt", "壊れていた文書".as_bytes(), "text/plain")
            .await;
        env.drain().await;
        let failed = env.state(&broken).await;
        env.parser.fail.store(false, Ordering::SeqCst);
        let retried = env.reindex(&env.alice, &broken).await.unwrap();
        env.drain().await;
        let repaired = env.state(&broken).await;

        // 前の版の不具合で取り込みを飛ばしたファイル（記録が skipped）も取り込み直せる。
        let renamed = env.orphan("renamed.txt", "名前を変えた文書").await;
        sqlx::query(
            "insert into rag_ingest_job (tenant_id, org, node_id, version, op, status) \
             values ($1, $2, $3, $4, 'create', 'skipped')",
        )
        .bind(&renamed.tenant_id)
        .bind(&renamed.org)
        .bind(renamed.id)
        .bind(renamed.version)
        .execute(&env.pool)
        .await
        .unwrap();
        let skipped = env.state(&renamed).await;
        let renamed_out = env.reindex(&env.alice, &renamed).await.unwrap();

        // 取り込み直しを飛ばしたファイル（記録が reindex の skipped・中身が見つからなかったなど）は、失敗として言い、
        // もう一度頼むと試し直す（以前は「記録が無い」と言って積むが、取り込みは処理済みとして何もしなかった）。
        let reskipped = env
            .orphan("reskipped.txt", "取り込み直しを飛ばした文書")
            .await;
        sqlx::query(
            "insert into rag_ingest_job (tenant_id, org, node_id, version, op, status) \
             values ($1, $2, $3, $4, 'reindex', 'skipped')",
        )
        .bind(&reskipped.tenant_id)
        .bind(&reskipped.org)
        .bind(reskipped.id)
        .bind(reskipped.version)
        .execute(&env.pool)
        .await
        .unwrap();
        let reskipped_state = env.state(&reskipped).await;
        let reskipped_out = env.reindex(&env.alice, &reskipped).await.unwrap();
        env.drain().await;
        let reskipped_after = env.state(&reskipped).await;

        // 取り込めない種類・取り込まない場所（AI エージェントの作業用のフォルダー）。
        let image = env
            .put("photo.png", b"\x89PNG\r\n\x1a\n", "image/png")
            .await;
        let unsupported = env.reindex(&env.alice, &image).await.unwrap();
        let scratch = env.orphan("scratch.txt", "作業メモ").await;
        sqlx::query("update node set system = true where id = $1")
            .bind(scratch.id)
            .execute(&env.pool)
            .await
            .unwrap();
        let excluded = env.reindex(&env.alice, &scratch).await.unwrap();

        let allowed = env.audits("file.reindex", "allow").await;
        let denied = env.audits("file.reindex", "deny").await;
        let reindex_jobs: Vec<(String, String)> = sqlx::query_as(
            "select op, status from rag_ingest_job where tenant_id = $1 and node_id = $2 \
             order by updated_at",
        )
        .bind(&memo.tenant_id)
        .bind(memo.id)
        .fetch_all(&env.pool)
        .await
        .unwrap();
        (
            memo.id,
            viewer,
            stranger,
            first,
            again,
            events_before_drain,
            pending,
            twins,
            twin_events,
            after,
            hits,
            indexed,
            failed,
            retried,
            repaired,
            skipped,
            renamed_out,
            (reskipped_state, reskipped_out, reskipped_after),
            unsupported,
            excluded,
            allowed,
            denied,
            reindex_jobs,
        )
    }
    .await;
    let parsed_memo = env.parsed("memo.txt");
    let parsed_twin = env.parsed("twin.txt");
    let parsed_reskipped = env.parsed("reskipped.txt");
    env.cleanup().await;
    let (
        memo,
        viewer,
        stranger,
        first,
        again,
        events_before_drain,
        pending,
        twins,
        twin_events,
        after,
        hits,
        indexed,
        failed,
        retried,
        repaired,
        skipped,
        renamed_out,
        (reskipped_state, reskipped_out, reskipped_after),
        unsupported,
        excluded,
        allowed,
        denied,
        reindex_jobs,
    ) = outcome;

    assert!(matches!(viewer, Err(StorageError::Forbidden)), "{viewer:?}");
    assert!(
        matches!(stranger, Err(StorageError::NotFound)),
        "{stranger:?}"
    );
    assert_eq!(first, ReindexOutcome::Queued { version: 1 });
    assert_eq!(again, ReindexOutcome::InProgress, "積んだ後は取り込み中");
    assert_eq!(events_before_drain, 1, "二度目は積まない");
    assert_eq!(pending, IndexState::Pending);
    let queued_twins = twins
        .iter()
        .filter(|o| matches!(o, ReindexOutcome::Queued { version: 1 }))
        .count();
    assert_eq!(queued_twins, 1, "{twins:?}");
    assert!(
        twins.iter().all(|o| matches!(
            o,
            ReindexOutcome::Queued { version: 1 } | ReindexOutcome::InProgress
        )),
        "{twins:?}"
    );
    assert_eq!(twin_events, 1, "同時に押しても 1 件");
    assert_eq!(parsed_twin, 1);

    assert_eq!(after, IndexState::Ready, "取り込み直すと本文が索引に入る");
    assert!(hits.contains(&memo), "検索で見つかる: {hits:?}");
    assert_eq!(
        indexed,
        ReindexOutcome::AlreadyIndexed,
        "同じ内容は二重に取り込まない"
    );
    assert_eq!(parsed_memo, 1, "取り込みは 1 回だけ");
    assert_eq!(
        reindex_jobs,
        [("reindex".to_string(), "succeeded".to_string())],
        "既存の経路（rag_ingest_job）を通った"
    );

    assert_eq!(failed, IndexState::Failed);
    assert_eq!(retried, ReindexOutcome::Queued { version: 1 });
    assert_eq!(
        repaired,
        IndexState::Ready,
        "失敗したファイルも取り込み直せる"
    );
    assert_eq!(skipped, IndexState::NotIngested);
    assert_eq!(renamed_out, ReindexOutcome::Queued { version: 1 });
    assert_eq!(reskipped_state, IndexState::Failed);
    assert_eq!(reskipped_out, ReindexOutcome::Queued { version: 1 });
    assert_eq!(
        (reskipped_after, parsed_reskipped),
        (IndexState::Ready, 1),
        "取り込み直しを飛ばした記録があっても、頼み直すと取り込む"
    );
    assert_eq!(
        unsupported,
        ReindexOutcome::NotIndexable(NotIndexable::Unsupported)
    );
    assert_eq!(
        excluded,
        ReindexOutcome::NotIndexable(NotIndexable::Excluded)
    );

    // 積んだ 5 件（memo・twin・broken・renamed・reskipped）の許可。拒否は bob（編集できない）と carol（見えない）。
    assert_eq!(allowed, 5, "積んだときだけ許可を残す");
    assert!(denied >= 2, "見えない・編集できない拒否を残す: {denied}");
}

/// 管理者のまとめての取り込みは、記録が無いファイルだけを、待っている作業が上限に届かない分だけ積む。待ち行列が
/// 溜まっている間は積まず、空くと続きから積む。最後まで確かめたら終わり、進み具合が分かる。取り込み済み・取り込めない
/// 種類・作業用のフォルダーのファイルは積まない。
#[tokio::test]
async fn backfill_queues_files_without_a_record_little_by_little() {
    let Some(env) = setup().await else { return };
    let outcome = async {
        let indexed = env.put("indexed.txt", "前から検索できる文書".as_bytes(), "text/plain").await;
        env.drain().await;
        let mut orphans = Vec::new();
        for i in 0..3 {
            orphans.push(env.orphan(&format!("old-{i}.txt"), &format!("古い文書 {i}")).await);
        }
        let image = env.put("photo.png", b"\x89PNG\r\n\x1a\n", "image/png").await;
        env.forget_outbox(&image).await;
        let scratch = env.orphan("scratch.txt", "作業メモ").await;
        sqlx::query("update node set system = true where id = $1")
            .bind(scratch.id)
            .execute(&env.pool)
            .await
            .unwrap();

        // 組織の待ち行列を、上限の 1 件手前まで埋める（ほかの取り込みが溜まっている状態）。同じテナントのほかの組織の
        // 待ちは、上限を超えるほどあっても数えない（ほかの組織の量を見せず、この組織のまとめての取り込みを止めない）。
        for (org, n) in [(env.alice.org.as_str(), MAX_WAITING - 1), ("another-org", MAX_WAITING * 2)] {
            sqlx::query(
                "insert into job_queue (queue, tenant_id, payload, visible_at, fair_seq) \
                 select $1, $2, jsonb_build_object('node_id', gen_random_uuid()::text, 'op', 'create', 'org', $4::text), \
                        now() + interval '1 day', 0 \
                 from generate_series(1, $3)",
            )
            .bind(RAG_INGEST_QUEUE)
            .bind(&env.alice.tenant_id)
            .bind(n as i32)
            .bind(org)
            .execute(&env.pool)
            .await
            .unwrap();
        }

        let (started, fresh) = env.search.start_backfill(&env.alice, None).await.unwrap();
        let (_, again) = env.search.start_backfill(&env.alice, None).await.unwrap();
        let step1 = backfill::step_once(&env.deps).await.unwrap();
        let step2 = backfill::step_once(&env.deps).await.unwrap();
        let mid = env.search.ingest_status(&env.alice).await.unwrap();
        sqlx::query("delete from job_queue where tenant_id = $1 and visible_at > now() + interval '1 hour'")
            .bind(&env.alice.tenant_id)
            .execute(&env.pool)
            .await
            .unwrap();
        let step3 = backfill::step_once(&env.deps).await.unwrap();
        let step4 = backfill::step_once(&env.deps).await.unwrap();
        env.drain().await;
        let states = futures::future::join_all(orphans.iter().map(|n| env.state(n))).await;
        let done = env.search.ingest_status(&env.alice).await.unwrap();
        let reindexed: Vec<Uuid> = sqlx::query_scalar(
            "select distinct node_id from rag_ingest_job where tenant_id = $1 and op = 'reindex'",
        )
        .bind(&env.alice.tenant_id)
        .fetch_all(&env.pool)
        .await
        .unwrap();
        // 終わった後にもう一度始めると、新しく確かめ直す（積むものは無い）。
        let (_, restarted) = env.search.start_backfill(&env.alice, None).await.unwrap();
        let rerun = backfill::step_once(&env.deps).await.unwrap();
        let audits = env.audits("org_admin.search_backfill", "allow").await;
        (
            indexed.id,
            orphans.iter().map(|n| n.id).collect::<Vec<_>>(),
            image.id,
            scratch.id,
            started,
            fresh,
            again,
            step1,
            step2,
            mid,
            step3,
            step4,
            states,
            done,
            reindexed,
            restarted,
            rerun,
            audits,
        )
    }
    .await;
    env.cleanup().await;
    let (
        indexed,
        orphans,
        image,
        scratch,
        started,
        fresh,
        again,
        step1,
        step2,
        mid,
        step3,
        step4,
        states,
        done,
        reindexed,
        restarted,
        rerun,
        audits,
    ) = outcome;

    // 分母は、ごみ箱と作業用のフォルダーを除いたファイル（indexed・old-0〜2・photo）。
    assert!(fresh && !again, "動いている間に頼んでも、新しく始めない");
    assert_eq!(started.state, BackfillState::Running);
    assert_eq!(started.files_total, 5);
    // 空いているのは 1 件だけ: 1 件積んで、その次のファイルの手前で止める。
    assert!(
        matches!(
            step1,
            StepOutcome::Advanced {
                queued: 1,
                done: false,
                ..
            }
        ),
        "{step1:?}"
    );
    assert_eq!(
        step2,
        StepOutcome::Throttled,
        "待っている作業が上限に届いたら積まない"
    );
    let mid_backfill = mid.backfill.clone().unwrap();
    assert_eq!(mid_backfill.files_queued, 1);
    assert!(mid_backfill.files_scanned < 5, "{mid_backfill:?}");
    assert_eq!(mid.waiting, MAX_WAITING, "{mid:?}");
    assert!(
        matches!(
            step3,
            StepOutcome::Advanced {
                queued: 2,
                done: true,
                ..
            }
        ),
        "空くと続きから積み、最後まで確かめて終わる: {step3:?}"
    );
    assert_eq!(step4, StepOutcome::Idle);
    assert!(
        states.iter().all(|s| *s == IndexState::Ready),
        "記録の無いファイルが取り込まれた: {states:?}"
    );
    let finished = done.backfill.unwrap();
    assert_eq!(finished.state, BackfillState::Done);
    assert!(finished.finished_at.is_some());
    assert_eq!((finished.files_scanned, finished.files_queued), (5, 3));
    assert_eq!(done.waiting, 0);
    let mut want = orphans.clone();
    want.sort_unstable();
    let mut got = reindexed.clone();
    got.sort_unstable();
    assert_eq!(got, want, "積んだのは記録の無いファイルだけ");
    for skip in [indexed, image, scratch] {
        assert!(!reindexed.contains(&skip), "{skip}");
    }
    assert!(restarted, "終わった後は新しく始める");
    assert!(
        matches!(
            rerun,
            StepOutcome::Advanced {
                queued: 0,
                done: true,
                ..
            }
        ),
        "{rerun:?}"
    );
    assert_eq!(audits, 2, "始めたときだけ監査に残す");
}
