//! **実 Drive API（StorageService）経由**のアップロード → 自動索引 → 検索の結合テスト。
//!
//! pipeline_it が DB 直挿入で経路を検証するのに対し、本テストは実サービスの
//! declare → **presigned PUT（実 MinIO）** → finalize → outbox → relay → consumer
//! → 索引 → SearchService（実 OpenFGA の二段 authz）を通し、パーサも
//! **内部 presigned GET で実際に blob を読む**（IndexerStorage の presign 経路の実証）。
//!
//! `STORAGE_TEST_DATABASE_URL` / `OPENFGA_TEST_URL` /（任意）`STORAGE_TEST_S3_ENDPOINT`
//! が揃った時のみ実行（CI coverage で実走）。
//!
//! 検証:
//! - ネストしたフォルダ階層へ複数ファイルをアップロード → 全てが検索可能になる
//! - authz_tags に**実 closure 由来の祖先フォルダ**が乗る（中間フォルダ共有で子孫が見える）
//! - 実 API のフォルダ move → 子孫ファイルのタグが付け替わる
//! - 実 API のファイル削除 → 検索から消える

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
use authz::{AuthContext, AuthzClient, Principal, Relation};
use common::{FakeEmbedder, FakeReranker, FakeVectorStore};
use rag::pipeline::{consumer, relay, PipelineDeps};
use rag::types::{BlockType, ParsedBlock, ParsedDocument};
use rag::{
    DocumentParser, ParseRequest, RagConfig, RagError, SearchMode, SearchService, TantivyFulltext,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{
    content_address::sha256_hex, IndexerStorage, Node, ObjectStore, S3Config, S3ObjectStore,
    ShareRole, ShareTarget, StorageService,
};
use uuid::Uuid;

/// **presigned GET を実際に fetch** して本文をブロック化するパーサ
/// （IndexerStorage → MinIO 内部署名 URL の経路を実証する）。
struct FetchingParser {
    http: reqwest::Client,
}

#[async_trait]
impl DocumentParser for FetchingParser {
    async fn parse(
        &self,
        _ctx: &AuthContext,
        req: ParseRequest<'_>,
    ) -> Result<ParsedDocument, RagError> {
        let resp = self.http.get(req.source_url).send().await?;
        if !resp.status().is_success() {
            return Err(RagError::Worker(format!(
                "presigned GET が失敗: {}",
                resp.status()
            )));
        }
        let text = resp.text().await?;
        let blocks = text
            .split("\n\n")
            .filter(|p| !p.trim().is_empty())
            .map(|p| ParsedBlock {
                block_type: BlockType::Paragraph,
                level: None,
                text: p.trim().to_string(),
                page: None,
            })
            .collect();
        Ok(ParsedDocument {
            blocks,
            used_ocr: false,
        })
    }
}

struct Env {
    pool: PgPool,
    service: StorageService,
    http: reqwest::Client,
    deps: Arc<PipelineDeps>,
    search: SearchService,
    alice: AuthContext,
    bob: AuthContext,
    _tmp: tempfile::TempDir,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

fn serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn user(tenant: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
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
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
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
        http.clone(),
        &OpenFgaConfig {
            base_url: fga_url,
            store_name: format!("shiki-drive-it-{}", Uuid::new_v4()),
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

    let tmp = tempfile::tempdir().unwrap();
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    // root へのフォルダ作成/アップロードは org member が要る（実サービスの認可要件）。
    let alice = user(&tenant, "alice");
    let bob = user(&tenant, "bob");
    for u in [&alice, &bob] {
        authz
            .write_tuple(&u.subject(), Relation::Member, &u.ns().organization(&u.org))
            .await
            .unwrap();
    }
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
        parser: Arc::new(FetchingParser { http: http.clone() }),
        embedder: Arc::new(FakeEmbedder),
        vector: Arc::clone(&vector) as _,
        fulltext: Arc::clone(&fulltext) as _,
        indexer_storage: Arc::new(IndexerStorage::new(pool.clone(), store)),
    });
    let search = SearchService::new(
        pool.clone(),
        config,
        Arc::new(FakeEmbedder),
        Arc::new(FakeReranker),
        vector,
        fulltext,
        Arc::clone(&authz),
        storage::audit::AuditRecorder::new(pool.clone()),
    );
    Some(Env {
        pool,
        service,
        http,
        deps,
        search,
        alice,
        bob,
        _tmp: tmp,
        _serial: serial,
    })
}

/// 実 API: declare → presigned PUT（実 MinIO）→ finalize。
async fn upload(env: &Env, parent: Option<Uuid>, name: &str, content: &str) -> Node {
    let bytes = content.as_bytes();
    let ticket = env
        .service
        .begin_upload(
            &env.alice,
            parent,
            name,
            "text/plain",
            &sha256_hex(bytes),
            bytes.len() as i64,
            None,
            None,
        )
        .await
        .unwrap();
    let resp = env
        .http
        .put(&ticket.upload_url)
        .body(bytes.to_vec())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "PUT: {}", resp.status());
    env.service
        .finalize_upload(&env.alice, ticket.upload_id, None)
        .await
        .unwrap()
}

async fn drain(env: &Env) {
    for _ in 0..30 {
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
    panic!("パイプラインが収束しませんでした");
}

async fn hit_files(env: &Env, ctx: &AuthContext, query: &str) -> Vec<Uuid> {
    // 埋め込みはフェイク（ハッシュベクトル）で dense 側が無関係文書にもノイズヒット
    // するため、内容一致の検証は決定的な keyword（実 Tantivy/BM25）モードで行う。
    // 認可の正しさはモード非依存（pre/post-filter は共通経路）。
    let mut files: Vec<Uuid> = env
        .search
        .search(ctx, query, Some(20), SearchMode::Keyword, None)
        .await
        .unwrap()
        .results
        .iter()
        .map(|r| r.file_id)
        .collect();
    files.sort_unstable();
    files.dedup();
    files
}

#[tokio::test]
async fn nested_folders_and_multiple_files_become_searchable_via_real_drive_api() {
    let Some(env) = setup().await else { return };
    // 実 API でフォルダ階層を作る: 本部/営業部/第一課
    let hq = env
        .service
        .create_folder(&env.alice, None, "本部", None)
        .await
        .unwrap();
    let sales = env
        .service
        .create_folder(&env.alice, Some(hq.id), "営業部", None)
        .await
        .unwrap();
    let section = env
        .service
        .create_folder(&env.alice, Some(sales.id), "第一課", None)
        .await
        .unwrap();

    // 複数ファイルを別々の階層へ（presigned PUT → finalize → outbox 発行は実サービス）。
    let f_root = upload(
        &env,
        None,
        "全社通達.txt",
        "全社の経費精算は毎月25日締めです。",
    )
    .await;
    let f_sales = upload(
        &env,
        Some(sales.id),
        "営業方針.txt",
        "営業の売上目標を引き上げる。",
    )
    .await;
    let f_section = upload(
        &env,
        Some(section.id),
        "第一課計画.txt",
        "第一課の売上と計画、担当割りを定める。",
    )
    .await;
    drain(&env).await;

    // owner の alice には 3 ファイルとも検索で見える（実 presigned GET でパースされた内容）。
    assert_eq!(
        hit_files(&env, &env.alice, "経費精算").await,
        vec![f_root.id]
    );
    let sales_hits = hit_files(&env, &env.alice, "売上").await;
    assert!(
        sales_hits.contains(&f_sales.id) && sales_hits.contains(&f_section.id),
        "sales={:?} section={:?} hits={sales_hits:?}",
        f_sales.id,
        f_section.id
    );

    // authz_tags には実 closure 由来の祖先が乗る: 中間フォルダ（営業部）を bob に共有すると
    // その配下（営業部直下＋第一課の孫）だけが見え、root のファイルは見えない。
    env.service
        .share_node(
            &env.alice,
            sales.id,
            &ShareTarget::User { id: "bob".into() },
            ShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let bob_hits = hit_files(&env, &env.bob, "売上").await;
    assert!(
        bob_hits.contains(&f_sales.id) && bob_hits.contains(&f_section.id),
        "中間フォルダ共有で子孫（孫含む）が検索に出る: {bob_hits:?}"
    );
    assert!(
        hit_files(&env, &env.bob, "経費精算").await.is_empty(),
        "共有範囲外（root 直下）は見えない"
    );
}

#[tokio::test]
async fn real_folder_move_retags_descendants_and_delete_hides() {
    let Some(env) = setup().await else { return };
    let archive = env
        .service
        .create_folder(&env.alice, None, "アーカイブ", None)
        .await
        .unwrap();
    let dept = env
        .service
        .create_folder(&env.alice, None, "企画部", None)
        .await
        .unwrap();
    let file = upload(&env, Some(dept.id), "議事録.txt", "四半期予算の議事録。").await;
    drain(&env).await;

    // アーカイブを bob に共有（この時点で bob には何も見えない）。
    env.service
        .share_node(
            &env.alice,
            archive.id,
            &ShareTarget::User { id: "bob".into() },
            ShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    assert!(hit_files(&env, &env.bob, "議事録").await.is_empty());

    // 実 API でフォルダ move（企画部 → アーカイブ配下）→ 子孫のタグが付け替わる。
    env.service
        .move_folder(&env.alice, dept.id, Some(archive.id), None)
        .await
        .unwrap();
    drain(&env).await;
    assert_eq!(
        hit_files(&env, &env.bob, "議事録").await,
        vec![file.id],
        "実 API のフォルダ move で子孫ファイルのタグが再評価される"
    );

    // 実 API のファイル削除 → 検索から消える。
    env.service
        .soft_delete_file(&env.alice, file.id, None)
        .await
        .unwrap();
    drain(&env).await;
    assert!(hit_files(&env, &env.alice, "議事録").await.is_empty());
}
