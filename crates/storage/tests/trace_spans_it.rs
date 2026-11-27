//! 認可・ストレージの子 span の結合テスト。実 Postgres・RustFS（S3 互換）・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する。
//!
//! 検証: ファイル一覧・ダウンロードの 1 リクエスト（`http_request` span の中）で、
//! `storage.*`（StorageService）・`authz.*`（OpenFGA クライアント）・`objstore.*`（ObjectStore）の
//! 子 span が出ること。子ごとの認可がループで span を増やさず（BatchCheck に寄せる）、
//! 1 リクエストの span 数が有界であること。拒否（エラー）が span 内の ERROR イベントになること。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, Consistency, Principal, Relation,
};
use sqlx::postgres::PgPoolOptions;
use storage::{object_store::S3Config, ObjectStore, S3ObjectStore, StorageError, StorageService};
use tracing::{
    span::{Attributes, Id},
    Event, Instrument, Level, Subscriber,
};
use tracing_subscriber::{
    layer::{Context, SubscriberExt},
    registry::LookupSpan,
    EnvFilter, Layer,
};
use uuid::Uuid;

/// 作られた span（名前・親の名前・`http_request` の配下か）と、ERROR イベントが出た span 名を記録する。
#[derive(Clone, Default)]
struct Recorder {
    spans: Arc<Mutex<Vec<SpanRec>>>,
    errors: Arc<Mutex<Vec<String>>>,
    /// `authz.batch_check` の `count`（判定した問いの数）。
    batch_counts: Arc<Mutex<Vec<u64>>>,
}

/// span 属性から `count` を拾う。
struct CountVisitor(Option<u64>);

impl tracing::field::Visit for CountVisitor {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "count" {
            self.0 = Some(value);
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}

#[derive(Debug, Clone)]
struct SpanRec {
    name: &'static str,
    parent: Option<&'static str>,
    under_request: bool,
}

impl<S> Layer<S> for Recorder
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        if span.name() == "authz.batch_check" {
            let mut v = CountVisitor(None);
            attrs.record(&mut v);
            self.batch_counts
                .lock()
                .unwrap()
                .push(v.0.expect("batch_check の count"));
        }
        let parent = span.parent().map(|p| p.name());
        let under_request = span.scope().skip(1).any(|s| s.name() == "http_request");
        self.spans.lock().unwrap().push(SpanRec {
            name: span.name(),
            parent,
            under_request,
        });
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if *event.metadata().level() == Level::ERROR {
            if let Some(span) = ctx.event_span(event) {
                self.errors.lock().unwrap().push(span.name().to_string());
            }
        }
    }
}

impl Recorder {
    /// 記録を取り出して空にする（1 リクエストずつ数える）。
    fn take(&self) -> (Vec<SpanRec>, Vec<String>) {
        self.batch_counts.lock().unwrap().clear();
        (
            std::mem::take(&mut *self.spans.lock().unwrap()),
            std::mem::take(&mut *self.errors.lock().unwrap()),
        )
    }
}

fn make_ctx(org: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.into(),
        authz::TenantId::parse("default").unwrap(),
    )
}

async fn setup() -> Option<(StorageService, Arc<dyn AuthzClient>)> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(openfga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let access_key =
        std::env::var("STORAGE_TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let secret_key =
        std::env::var("STORAGE_TEST_S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".into());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: openfga_url,
            store_name: format!("shiki-storage-span-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key,
        secret_key,
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    }));
    store.ensure_bucket().await.expect("バケット準備");
    let service = StorageService::new(
        pool,
        store,
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024,
    );
    Some((service, authz))
}

/// org メンバーのタプルを付与し、MinimizeLatency で見えるまで待つ（storage_it と同じ理由）。
async fn seed_org_member(authz: &Arc<dyn AuthzClient>, ctx: &AuthContext) {
    let org = ctx.ns().organization(ctx.org());
    authz
        .write_tuple(&ctx.subject(), Relation::Member, &org)
        .await
        .expect("member tuple seed");
    for _ in 0..200 {
        if authz
            .check(
                &ctx.subject(),
                Relation::Member,
                &org,
                Consistency::MinimizeLatency,
            )
            .await
            .expect("member check")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("seed した org member タプルが可視化されませんでした");
}

fn names_under_request(spans: &[SpanRec]) -> Vec<&'static str> {
    spans
        .iter()
        .filter(|s| s.under_request)
        .map(|s| s.name)
        .collect()
}

fn has_child(spans: &[SpanRec], name: &str, parent: &str) -> bool {
    spans
        .iter()
        .any(|s| s.under_request && s.name == name && s.parent == Some(parent))
}

/// 1 リクエストあたりの span 数の上限（Issue の目安「数十まで」）。
const MAX_SPANS_PER_REQUEST: usize = 30;

#[tokio::test]
async fn list_and_download_emit_bounded_child_spans() {
    let Some((service, authz)) = setup().await else {
        return;
    };
    let org = format!("span-{}", Uuid::new_v4().simple());
    let actx = make_ctx(&org, "alice");
    seed_org_member(&authz, &actx).await;

    // 子が多いフォルダ（ページ 50 件・子ごとの判定がループで span を増やさないことを見る）。
    const CHILDREN: usize = 60;
    let folder = service
        .create_folder(&actx, None, "span-folder", None)
        .await
        .expect("フォルダ作成");
    let mut first_file = None;
    for i in 0..CHILDREN {
        let node = service
            .write_file_internal(
                &actx,
                Some(folder.id),
                &format!("f-{i}.txt"),
                format!("content {i}").as_bytes(),
                "text/plain",
                None,
            )
            .await
            .expect("ファイル書き込み");
        first_file.get_or_insert(node.id);
    }
    let file_id = first_file.unwrap();

    // 本番の既定 EnvFilter（`info,...`）と同じく INFO 以上だけを数える。
    let rec = Recorder::default();
    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::new("info"))
        .with(rec.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    // --- ファイル一覧 ---
    let page = service
        .list_children(
            &actx,
            Some(folder.id),
            Default::default(),
            None,
            50,
            false,
            None,
        )
        .instrument(tracing::info_span!("http_request"))
        .await
        .expect("一覧");
    assert_eq!(page.items.len(), 50);
    // ページ（50 件）に入り得る行だけを判定し、ページ確定後の 10 行は判定しない。
    let checked: u64 = rec.batch_counts.lock().unwrap().iter().sum();
    assert_eq!(checked, 50, "ページ外の行まで認可していないこと");
    let (spans, errors) = rec.take();
    let names = names_under_request(&spans);
    println!("ファイル一覧の span 数: {} {names:?}", names.len());
    assert!(has_child(&spans, "storage.list_children", "http_request"));
    assert!(has_child(
        &spans,
        "authz.batch_check",
        "storage.list_children"
    ));
    assert!(has_child(&spans, "authz.check", "storage.list_children"));
    assert!(
        !names.contains(&"objstore.get"),
        "一覧はオブジェクトストアを読まない"
    );
    assert!(
        names.len() <= MAX_SPANS_PER_REQUEST,
        "一覧の span が多すぎる: {}",
        names.len()
    );
    assert!(errors.is_empty(), "成功した一覧に ERROR が出た: {errors:?}");

    // --- ダウンロード（presigned GET URL の発行） ---
    service
        .issue_download_url(&actx, file_id, None)
        .instrument(tracing::info_span!("http_request"))
        .await
        .expect("ダウンロード URL");
    let (spans, errors) = rec.take();
    let names = names_under_request(&spans);
    println!("ダウンロードの span 数: {} {names:?}", names.len());
    assert!(has_child(
        &spans,
        "storage.issue_download_url",
        "http_request"
    ));
    assert!(has_child(
        &spans,
        "authz.check",
        "storage.issue_download_url"
    ));
    assert!(has_child(
        &spans,
        "objstore.presign_get",
        "storage.issue_download_url"
    ));
    assert!(names.len() <= MAX_SPANS_PER_REQUEST);
    assert!(errors.is_empty(), "{errors:?}");

    // --- 内部読み取り（実バイトを ObjectStore から読む経路） ---
    let (_, bytes) = service
        .read_file_internal(&actx, file_id, None)
        .instrument(tracing::info_span!("http_request"))
        .await
        .expect("内部読み取り");
    assert!(!bytes.is_empty());
    let (spans, _) = rec.take();
    println!("内部読み取りの span: {:?}", names_under_request(&spans));
    assert!(has_child(&spans, "storage.read_file", "http_request"));
    assert!(
        spans
            .iter()
            .any(|s| s.under_request && s.name.starts_with("authz.")),
        "読み取りにも認可の子 span が出る"
    );

    // --- 拒否はエラーとして span に記録される ---
    let mallory = make_ctx(&org, "mallory");
    let err = service
        .issue_download_url(&mallory, file_id, None)
        .instrument(tracing::info_span!("http_request"))
        .await
        .expect_err("共有されていない利用者は拒否される");
    // 読めない対象は存在秘匿で NotFound になる経路もある（どちらも拒否）。
    assert!(
        matches!(err, StorageError::Forbidden | StorageError::NotFound),
        "{err:?}"
    );
    let (_, errors) = rec.take();
    assert!(
        errors.iter().any(|n| n == "storage.issue_download_url"),
        "拒否が storage span の ERROR イベントにならない: {errors:?}"
    );
}
