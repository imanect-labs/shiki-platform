//! **実 Drive API（StorageService）経由**のアップロード → 自動索引 → 検索の結合テスト。
//!
//! pipeline_it が DB 直挿入で経路を検証するのに対し、本テストは実サービスの
//! declare → **presigned PUT（実 RustFS）** → finalize → outbox → relay → consumer
//! → 索引 → SearchService（実 OpenFGA の二段 authz）を通し、blob を実際に読む。文章のファイルは取り込みが
//! IndexerStorage で実 RustFS から読んで UTF-8 にしたバイト列を、PDF などはパーサが**内部 presigned GET** で
//! 読んだ原本を使う（IndexerStorage の読み取りと presign の両方の経路の実証・D-118）。
//!
//! `STORAGE_TEST_DATABASE_URL` / `OPENFGA_TEST_URL` /（任意）`STORAGE_TEST_S3_ENDPOINT`
//! が揃った時のみ実行（CI coverage で実走）。
//!
//! 検証:
//! - ネストしたフォルダ階層へ複数ファイルをアップロード → 全てが検索可能になる
//! - authz_tags に**実 closure 由来の祖先フォルダ**が乗る（中間フォルダ共有で子孫が見える）
//! - 実 API のフォルダ move → 子孫ファイルのタグが付け替わる
//! - 実 API のファイル削除 → 検索から消える
//! - 文章のファイル（Shift_JIS・BOM 付きの UTF-8・Windows の `.csv` の MIME）は、取り込みが文字コードを決めて
//!   UTF-8 にしたバイト列で渡し（D-118）、PDF などは presigned GET で渡す

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

use std::sync::{Arc, Mutex};
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
/// （IndexerStorage → オブジェクトストア内部署名 URL の経路を実証する）。
///
/// 文章のファイルは、取り込みが中身を読んで UTF-8 にしたバイト列（`ParseSource::Bytes`）で渡す（D-118）。どちらの
/// 渡し方でも、worker（Docling の CSV・Markdown）と同じく UTF-8 として厳密に読み、読めなければパースの失敗にする。
/// 受け取った渡し方・種類・本文はファイル名ごとに残す（テストが確かめる）。
struct FetchingParser {
    http: reqwest::Client,
    seen: Mutex<Vec<Seen>>,
}

/// パーサが受け取ったもの。
#[derive(Debug, Clone)]
struct Seen {
    file_name: String,
    /// `url`（presigned GET）か `bytes`（取り込みが読んだ中身）。
    source: &'static str,
    content_type: String,
    text: String,
}

#[async_trait]
impl DocumentParser for FetchingParser {
    async fn parse(
        &self,
        _ctx: &AuthContext,
        req: ParseRequest<'_>,
    ) -> Result<ParsedDocument, RagError> {
        let (source, bytes) = match req.source {
            rag::ParseSource::Url(source_url) => {
                let resp = self.http.get(source_url).send().await?;
                if !resp.status().is_success() {
                    return Err(RagError::Worker(format!(
                        "presigned GET が失敗: {}",
                        resp.status()
                    )));
                }
                ("url", resp.bytes().await?.to_vec())
            }
            rag::ParseSource::Bytes(bytes) => ("bytes", bytes.to_vec()),
        };
        let text = String::from_utf8(bytes).map_err(|e| RagError::Parse {
            code: "parse_failed".into(),
            detail: format!("UTF-8 として読めません: {e}"),
        })?;
        self.seen.lock().unwrap().push(Seen {
            file_name: req.file_name.to_string(),
            source,
            content_type: req.content_type.to_string(),
            text: text.clone(),
        });
        let blocks = text
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
    parser: Arc<FetchingParser>,
    search: SearchService,
    alice: AuthContext,
    bob: AuthContext,
    /// テストのストア（後片付けで消す）。
    fga_url: String,
    store_id: String,
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
        authz::TenantId::parse(tenant).unwrap(),
    )
}

async fn setup() -> Option<Env> {
    let serial = serial_lock().lock().await;
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
            base_url: fga_url.clone(),
            store_name: format!("shiki-drive-it-{}", Uuid::new_v4()),
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
            .write_tuple(
                &u.subject(),
                Relation::Member,
                &u.ns().organization(u.org()),
            )
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
    let parser = Arc::new(FetchingParser {
        http: http.clone(),
        seen: Mutex::new(Vec::new()),
    });
    let deps = Arc::new(PipelineDeps {
        pool: pool.clone(),
        config: config.clone(),
        parser: Arc::clone(&parser) as _,
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
        parser,
        search,
        alice,
        bob,
        fga_url,
        store_id,
        _tmp: tmp,
        _serial: serial,
    })
}

/// 実 API: declare → presigned PUT（実 RustFS）→ finalize。
async fn upload(env: &Env, parent: Option<Uuid>, name: &str, content: &str) -> Node {
    upload_bytes(env, parent, name, content.as_bytes(), "text/plain").await
}

/// [`upload`] の中身と種類を決められる版（ブラウザーが付けた MIME のまま保存する）。
async fn upload_bytes(
    env: &Env,
    parent: Option<Uuid>,
    name: &str,
    bytes: &[u8],
    content_type: &str,
) -> Node {
    let ticket = env
        .service
        .begin_upload(
            &env.alice,
            parent,
            name,
            content_type,
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
        .search(ctx, query, Some(20), SearchMode::Keyword, None, None)
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

/// テナントの行・タプル・オブジェクトと、テストのストアを消す（落ちても後片付けをしてから結果を返す）。
async fn cleanup(env: &Env) {
    let tenant = env.alice.tenant_id().to_string();
    env.service
        .purge_tenant(&tenant, env.alice.org(), "drive-upload-it")
        .await
        .unwrap();
    let resp = env
        .http
        .delete(format!("{}/stores/{}", env.fga_url, env.store_id))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete store: {}",
        resp.status()
    );
}

/// 日本語の Windows で作った文章のファイルが、文字化けせずに索引に入り、検索に出る（GAP-46・D-118）。
///
/// - Shift_JIS のテキスト（メモ帳・古いシステムの書き出し）
/// - BOM 付きの UTF-8 の CSV（Excel の「CSV UTF-8」）。先頭の U+FEFF を本文に残さない
/// - Excel を入れた Windows のブラウザーが `application/vnd.ms-excel` を付けた Shift_JIS の `.csv`（Excel の既定）
///
/// 文章のファイルは、取り込みが実 RustFS から中身を読み、文字コードを決めて UTF-8 にしたバイト列で worker へ渡す。
/// PDF などの文章でない種類は、これまでどおり presigned GET で渡す。
#[tokio::test]
async fn japanese_encodings_are_indexed_as_utf8_and_found_by_search() {
    let Some(env) = setup().await else { return };
    let outcome = {
        let env = &env;
        async move {
            let sjis_text = "出張旅費の精算は翌月5日までに申請する。";
            let (sjis, _, _) = textenc::encoding_rs::SHIFT_JIS.encode(sjis_text);
            let notice = upload_bytes(env, None, "旅費規程.txt", &sjis, "text/plain").await;

            let bom_csv = "区分,金額\n宿泊,12000\n";
            let bom = [b"\xEF\xBB\xBF".as_slice(), bom_csv.as_bytes()].concat();
            let utf8_csv = upload_bytes(env, None, "宿泊費.csv", &bom, "text/csv").await;

            let excel_csv = "勘定科目,支払額\n接待,8000\n";
            let (excel, _, _) = textenc::encoding_rs::SHIFT_JIS.encode(excel_csv);
            let excel_node =
                upload_bytes(env, None, "交際費.csv", &excel, "application/vnd.ms-excel").await;

            // 本物の .xls は取り込まない（名前が .csv のときだけ CSV として扱う）。
            let xls =
                upload_bytes(env, None, "古い表.xls", &excel, "application/vnd.ms-excel").await;

            let pdf_text = "慶弔見舞金の規程。";
            let pdf = upload_bytes(
                env,
                None,
                "慶弔規程.pdf",
                pdf_text.as_bytes(),
                "application/pdf",
            )
            .await;
            drain(env).await;

            let seen = env.parser.seen.lock().unwrap().clone();
            let find = |name: &str| {
                seen.iter()
                    .find(|s| s.file_name == name)
                    .cloned()
                    .unwrap_or_else(|| panic!("{name} がパーサに渡っていない: {seen:?}"))
            };
            let s = find("旅費規程.txt");
            assert_eq!((s.source, s.text.as_str()), ("bytes", sjis_text));
            let s = find("宿泊費.csv");
            assert_eq!((s.source, s.text.as_str()), ("bytes", bom_csv));
            assert!(!s.text.starts_with('\u{FEFF}'), "BOM を本文に残さない");
            let s = find("交際費.csv");
            assert_eq!(
                (s.source, s.content_type.as_str(), s.text.as_str()),
                ("bytes", "text/csv", excel_csv)
            );
            let s = find("慶弔規程.pdf");
            assert_eq!((s.source, s.text.as_str()), ("url", pdf_text));
            assert!(
                !seen.iter().any(|s| s.file_name == "古い表.xls"),
                "本物の .xls は取り込まない: {seen:?}"
            );

            assert_eq!(hit_files(env, &env.alice, "出張").await, vec![notice.id]);
            assert_eq!(hit_files(env, &env.alice, "宿泊").await, vec![utf8_csv.id]);
            assert_eq!(
                hit_files(env, &env.alice, "接待").await,
                vec![excel_node.id]
            );
            assert_eq!(hit_files(env, &env.alice, "慶弔").await, vec![pdf.id]);
            let _ = xls;
        }
    };
    let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(outcome)).await;
    cleanup(&env).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
