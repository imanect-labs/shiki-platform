//! 実 HTTP 経路（router → TraceLayer の `http_request` span → ハンドラ → StorageService →
//! OpenFGA / ObjectStore）で、認可・ストレージの子 span が `http_request` の下に出ることを確かめる。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）・OpenFGA（`OPENFGA_TEST_URL`）・S3 互換ストレージ
//! （`STORAGE_TEST_S3_*`・RustFS）が必要。未設定ならスキップする。

use super::*;
use std::sync::Mutex;

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, TenantId,
};
use tracing::{
    span::{Attributes, Id},
    Subscriber,
};
use tracing_subscriber::{
    layer::{Context, SubscriberExt},
    registry::LookupSpan,
    EnvFilter, Layer,
};
use uuid::Uuid;

/// span の（名前, 親の名前）。
type SpanRec = (&'static str, Option<&'static str>);

/// `http_request` の配下に作られた span を記録する。
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<SpanRec>>>);

impl<S> Layer<S> for Recorder
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        if span.scope().skip(1).any(|s| s.name() == "http_request") {
            self.0
                .lock()
                .unwrap()
                .push((span.name(), span.parent().map(|p| p.name())));
        }
    }
}

impl Recorder {
    fn take(&self) -> Vec<SpanRec> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

fn has(spans: &[SpanRec], name: &str, parent: &str) -> bool {
    spans.iter().any(|(n, p)| *n == name && *p == Some(parent))
}

/// 1 リクエストあたりの span 数の上限（Issue の目安「数十まで」）。
const MAX_SPANS_PER_REQUEST: usize = 30;

#[tokio::test]
async fn list_and_download_requests_emit_child_spans_under_http_request() {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return;
    };
    let Ok(openfga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        return;
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
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let fga: Arc<dyn AuthzClient> = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: openfga_url,
                store_name: format!("shiki-api-span-it-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .unwrap(),
    );
    let store: Arc<dyn storage::ObjectStore> = Arc::new(storage::S3ObjectStore::new(
        &storage::object_store::S3Config {
            internal_endpoint: s3_endpoint.clone(),
            public_endpoint: s3_endpoint,
            bucket: "shiki-it-blobs".into(),
            access_key,
            secret_key,
            region: "us-east-1".into(),
            presign_get_ttl_secs: 300,
            presign_put_ttl_secs: 900,
            cors_allowed_origins: vec![],
        },
    ));
    store.ensure_bucket().await.unwrap();
    let service = Arc::new(storage::StorageService::new(
        pool.clone(),
        store,
        fga.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024,
    ));

    // セッションの principal（group `/acme` → org `acme`・tenant `default`）で準備する。
    let ctx = AuthContext::new(
        test_principal(),
        "acme".into(),
        TenantId::parse("default").unwrap(),
    );
    fga.write_tuple(
        &ctx.subject(),
        Relation::Member,
        &ctx.ns().organization("acme"),
    )
    .await
    .unwrap();
    let folder = service
        .create_folder(
            &ctx,
            None,
            &format!("span-http-{}", Uuid::new_v4().simple()),
            None,
        )
        .await
        .unwrap();
    let mut file_id = None;
    for i in 0..60 {
        let node = service
            .write_file_internal(
                &ctx,
                Some(folder.id),
                &format!("f-{i}.txt"),
                format!("content {i}").as_bytes(),
                "text/plain",
                None,
            )
            .await
            .unwrap();
        file_id.get_or_insert(node.id);
    }
    let file_id = file_id.unwrap();

    let mut state = state_with(store_with_valid_session().await, None);
    state.authz = fga;
    state.storage = service;
    state.directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    let app = build_router(state);

    // 本番の既定 EnvFilter（`info,...`）と同じく INFO 以上だけを数える。
    let rec = Recorder::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::registry()
            .with(EnvFilter::new("info"))
            .with(rec.clone()),
    );

    // --- ファイル一覧（GET /nodes?parent_id=…） ---
    let (status, body) = get_with_session(
        app.clone(),
        &format!("/nodes?parent_id={}&limit=50", folder.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 50);
    let spans = rec.take();
    println!("GET /nodes の span 数: {} {spans:?}", spans.len());
    assert!(has(&spans, "storage.list_children", "http_request"));
    assert!(has(&spans, "authz.batch_check", "storage.list_children"));
    assert!(spans.len() <= MAX_SPANS_PER_REQUEST, "{}", spans.len());

    // --- ダウンロード（GET /files/{id}/download-url） ---
    let (status, body) = get_with_session(app, &format!("/files/{file_id}/download-url")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let spans = rec.take();
    println!(
        "GET /files/{{id}}/download-url の span 数: {} {spans:?}",
        spans.len()
    );
    assert!(has(&spans, "storage.issue_download_url", "http_request"));
    assert!(has(&spans, "authz.check", "storage.issue_download_url"));
    assert!(has(
        &spans,
        "objstore.presign_get",
        "storage.issue_download_url"
    ));
    assert!(spans.len() <= MAX_SPANS_PER_REQUEST, "{}", spans.len());
}
