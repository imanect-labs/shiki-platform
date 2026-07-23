//! shiki-server エントリポイント。設定 → 計装 → 依存配線 → axum 起動。

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use api::{
    config::AppConfig, middleware::JwksCache, server::build_router, session::RedisSessionStore,
    state::AppState, telemetry,
};
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    model, AuthzClient,
};
use sqlx::postgres::PgPoolOptions;
use storage::{DirectoryStore, TenantStore};

mod dev_seed;
mod gateway_ai;
mod gateway_functions;
mod miniapp_triggers;
mod wiring;
mod wiring_gateway;
mod wiring_gui;
mod wiring_websearch;
// main はアプリ全体（ストレージ/RAG/チャット/ワークフロー/data/ゲートウェイ等）の配線点で
// あり、各フェーズの依存を順に組み上げる性質上どうしても長くなる（各配線は wire_* ヘルパへ
// 分離済み）。分割で可読性を落とすより配線列挙として素直に保つ。
#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::load().context("設定のロードに失敗")?;
    let _telemetry = telemetry::init(&config.telemetry).context("計装の初期化に失敗")?;

    tracing::info!(service = %config.telemetry.service_name, "shiki-server 起動中");

    // Postgres は lazy 接続（compose の起動順に耐性を持たせ、/readyz で疎通を表現）。
    let db = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect_lazy(&config.database.url)
        .context("Postgres プールの初期化に失敗")?;

    // スキーマ・マイグレーションを適用（起動時 fail-fast）。
    // ここで初めて実接続が張られる。compose は depends_on で postgres healthy を待つ。
    sqlx::migrate!("../../migrations")
        .run(&db)
        .await
        .context("DB マイグレーションの適用に失敗")?;
    tracing::info!("DB マイグレーション適用完了");

    // JWKS 取得・OpenFGA で共用する HTTP クライアント。
    let http = reqwest::Client::new();

    // OpenFGA（store/model を冪等にロード）。
    let fga_config = OpenFgaConfig {
        base_url: config.authz.base_url.clone(),
        store_name: config.authz.store_name.clone(),
    };
    let fga = OpenFgaClient::connect(http.clone(), &fga_config, &model::default_model())
        .await
        .context("OpenFGA への接続に失敗")?;
    // ユーザーディレクトリ（共有相手検索。storage と同じ db プールを共有）。dev_seed で使う。
    let directory = Arc::new(DirectoryStore::new(db.clone()));
    // テナントレジストリ（プロビジョニング/削除・SAAS.2）。
    let tenants = Arc::new(TenantStore::new(db.clone()));
    dev_seed::dev_seed(&fga, &directory, &config.auth).await?;
    // authz は AppState と StorageService で同一インスタンスを共有する（単一チョークポイント）。
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);

    // ストレージ: backend に応じて ObjectStore を選び StorageService を構築する（wiring）。
    let (object_store, storage) = wiring::wire_storage(&config, &db, &authz).await?;

    let jwks = Arc::new(JwksCache::new(
        http.clone(),
        config.auth.effective_jwks_uri(),
        Duration::from_secs(config.auth.jwks_ttl_secs),
    ));

    // BFF セッションストア（Redis）。compose では depends_on で redis healthy を待つ。
    let sessions = Arc::new(
        RedisSessionStore::connect(&config.session.redis_url)
            .await
            .context("Redis セッションストアへの接続に失敗")?,
    );

    // RAG（Phase 2）: enabled のときのみインジェスト・パイプラインと検索を配線する。
    let (search, rag_admin) = wiring::wire_rag(&config, &http, &db, &object_store, &authz)?;

    // アーティファクト共通枠（Task 6.1）: authz と同一インスタンスを共有（単一チョークポイント）。
    let artifacts = Arc::new(artifact::ArtifactStore::new(db.clone(), authz.clone()));
    // 構造化データサービス（Task 9.2〜9.10）: data ストア＋保存ビュー＋FSM を束ねて配線する。
    let (data_store, data_views, fsms) = wire_data(&db, &authz, &directory, &storage, &artifacts);
    // ミニアプリ基盤（Task 9.1/9.13a）: マニフェスト store ＋汎用レジストリ。
    let mini_app_code = Arc::new(app_platform::MiniAppCodeStore::new(
        Arc::clone(&artifacts),
        app_platform::Registry::new(db.clone()),
    ));
    // B1 フロントバンドル保管（Task 9.11・content-addressed・owner のみ put）。
    let bundles = Arc::new(app_platform::BundleStore::new(
        Arc::clone(&object_store),
        authz.clone(),
        storage::audit::AuditRecorder::new(db.clone()),
    ));
    // アプリ利用量集計（Task 9.15・capability＋llm）。
    let app_usage = Arc::new(app_platform::AppUsageStore::new(db.clone(), authz.clone()));
    // generative UI / skill / ミニアプリ（Phase 6）: 検証は全経路が同一実装を共有する信頼境界。
    let gui_stores = wiring_gui::wire_gui(&db, &artifacts);

    // ワークフロー IR ストア（Task 10.1a）: artifact の上に保存時検証を載せる。
    // chat（emit_workflow・Task 10.13）が使うため wire_chat より先に組む。
    let workflows = Arc::new(workflow_engine::WorkflowStore::new(Arc::clone(&artifacts)));

    // シークレット管理（Task 10.9）: マスターキーファイルが設定されていれば配線する。
    let secrets = match &config.secrets.master_key_file {
        Some(path) => {
            let provider = secrets::LocalKeyFileProvider::from_file(std::path::Path::new(path))
                .context("secrets マスターキーの読み込みに失敗")?;
            tracing::info!("シークレット管理を配線しました（local-key-file）");
            Some(Arc::new(secrets::SecretStore::new(
                db.clone(),
                authz.clone(),
                Arc::new(provider),
            )))
        }
        None => None,
    };

    // 同意インストール（Task 9.13b）: Keycloak admin（provisioner）があれば client 登録も行う。
    let installs = Arc::new(wiring_gateway::wire_installs(
        &config,
        &http,
        &db,
        &authz,
        &mini_app_code,
        &data_store,
        secrets.as_ref(),
    ));

    // チャット（Phase 3）: enabled のとき llm-gateway＋生成ワーカーを配線し、API 用ストアを返す。
    // ノート共同編集ハブ（Task 11P.1）: authz ゲート＋update log/snapshot 永続化。
    // wire_chat（document.edit・Task 11P.4）より先に組む。
    let collab = Arc::new(collab::CollabHub::new(
        db.clone(),
        authz.clone(),
        storage.clone(),
    ));
    // CSV クエリ/パッチサービス（Task 11P.7）: 隔離 DuckDB ランナーへ委譲するチョークポイント。
    let tabular = Arc::new(tabular::TabularService::new(
        storage.clone(),
        tabular::RunnerConfig::new(
            config.tabular.runner_path.clone(),
            std::time::Duration::from_secs(config.tabular.timeout_secs),
        ),
        tabular::Quotas {
            memory_limit_mb: config.tabular.memory_limit_mb,
            max_rows: config.tabular.max_rows,
            page_size: config.tabular.page_size,
        },
    ));

    // storage はツール成果物（code_interpreter）の保存先として渡す（Task 4.11）。
    // workflows/secrets は AI ワークフロー編集（emit_workflow・Task 10.13）のカタログ源。
    // collab は AI ノート共同編集（document.edit・Task 11P.4）。
    // skill の publish / 同意インストール（Phase 9 レジストリ流用・ユーザー単位・#344）。
    // chat の skill カタログ（インストール済み ∪ 本人）と V4 skill 照合の材料になる。
    let skill_installs = Arc::new(app_platform::SkillInstallService::new(
        db.clone(),
        app_platform::Registry::new(db.clone()),
        app_platform::TrustedKeyStore::new(db.clone()),
        Arc::clone(&artifacts),
        authz.clone(),
    ));

    let chat = wiring::wire_chat(
        &config,
        &http,
        &db,
        &authz,
        search.as_ref(),
        &storage,
        &gui_stores.validator,
        &artifacts,
        &workflows,
        secrets.as_ref(),
        &collab,
        &tabular,
        &skill_installs,
    )
    .await?;

    // ワークフロー実行時（Stage A W3）: enabled のとき launcher/runs を組み、worker/scheduler/relay を spawn。
    let (workflow_launcher, workflow_runs) = wiring::wire_workflow(
        &config,
        &http,
        &db,
        &authz,
        &workflows,
        &storage,
        search.as_ref(),
        secrets.as_ref(),
        &tabular,
        &artifacts,
    )
    .await?;

    // ワークフロー有効化・同意・トリガ実体化（Task 10.4a）。runtime 無効でも enable/disable は
    // 受け付ける（トリガは runtime 有効時に発火）。
    let workflow_registration = Arc::new(workflow_engine::RegistrationService::new(
        db.clone(),
        workflow_engine::DelegationStore::new(db.clone(), authz.clone()),
    ));
    let audit = Arc::new(storage::audit::AuditRecorder::new(db.clone()));
    let workflow_summaries = Arc::new(workflow_engine::WorkflowSummaryStore::new(db.clone()));
    let workflow_layout = Arc::new(workflow_engine::EditorLayoutStore::new(db.clone()));

    // 宣言的 UI アクションの実行系（Task 6.5）: chat.submit・安全ツール・workflow 起動を束ねる。
    let ui_actions = wiring_gui::wire_ui_actions(
        &config,
        &http,
        &db,
        chat.as_ref(),
        search.as_ref(),
        workflow_launcher.as_ref(),
    )?;

    // Office 統合（Task 11.5/11.6）: enabled のときのみ Collabora suite ＋ WOPI を配線する。
    let office = wiring::wire_office(&config, &http, &db, &authz, &storage)?;
    // md→docx 合成（POST /documents・#332）: worker のみ必要なので office フラグに載せず常時配線。
    // 共有クライアントは無期限のため、worker 遅延がハンドラを永久ブロックしないよう専用に切る。
    let compose_http = reqwest::Client::builder()
        .timeout(Duration::from_mins(1))
        .build()
        .context("docx compose 用 HTTP クライアントの構築に失敗")?;
    let docx_composer = Arc::new(office::DocxComposer::new(
        compose_http,
        &config.rag.worker_base_url,
    ));

    let bind = format!("{}:{}", config.server.host, config.server.port);
    // ミニアプリの第2/第3リスナ（ゲートウェイ・B1 配信）と B2 トリガ（event/cron）を
    // 一括で組んで spawn する（Task 9.6/9.8/9.9/9.11/9.12・enabled 時のみ）。
    wiring_gateway::spawn_miniapp_listeners(wiring_gateway::MiniappListenerDeps {
        config: &config,
        http: &http,
        db: &db,
        jwks: &jwks,
        authz: &authz,
        storage: &storage,
        data: &data_store,
        fsms: &fsms,
        search: search.as_ref(),
        object_store: &object_store,
        secrets: secrets.as_ref(),
    })
    .await?;
    let state = AppState {
        config: Arc::new(config),
        // 生 PgPool は StorageService 等のチョークポイントにのみ渡し、AppState には
        // readiness 専用の newtype で載せる（#91 M-2・ハンドラの生 SQL を型で防ぐ）。
        db: api::state::ReadinessProbe::new(db),
        authz,
        jwks,
        sessions,
        http,
        storage,
        collab,
        tabular,
        artifacts,
        data: data_store,
        data_views,
        fsms,
        mini_app_code,
        installs,
        skill_installs,
        bundles,
        app_usage,
        ui_specs: gui_stores.ui_specs,
        ui_actions,
        skills: gui_stores.skills,
        mini_apps: gui_stores.mini_apps,
        secrets,
        workflows,
        workflow_launcher,
        workflow_runs,
        workflow_registration,
        workflow_summaries,
        workflow_layout,
        audit,
        directory,
        tenants,
        search,
        chat,
        rag_admin,
        office,
        docx_composer,
    };

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .context("bind に失敗")?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("サーバ実行中のエラー")?;
    Ok(())
}

/// 構造化データ（Task 9.2〜9.10）の 3 ストアを配線する。
///
/// data ストア（テーブル/レコード/行述語）・保存ビュー（9.4）・FSM 宣言的ガード（9.10）は
/// いずれも同じ data チョークポイントと artifact 共通枠の上に載る。
fn wire_data(
    db: &sqlx::PgPool,
    authz: &std::sync::Arc<dyn AuthzClient>,
    directory: &std::sync::Arc<DirectoryStore>,
    storage: &std::sync::Arc<storage::StorageService>,
    artifacts: &std::sync::Arc<artifact::ArtifactStore>,
) -> (
    std::sync::Arc<data::DataStore>,
    std::sync::Arc<data::DataViewStore>,
    std::sync::Arc<data::FsmStore>,
) {
    // 参照整合（user/role/file）は directory / StorageService のチョークポイントへ委譲する。
    let data_store = Arc::new(data::DataStore::new(
        db.clone(),
        authz.clone(),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::clone(directory),
            storage: Arc::clone(storage),
        }),
    ));
    let data_views = Arc::new(data::DataViewStore::new(
        Arc::clone(artifacts),
        (*data_store).clone(),
    ));
    let fsms = Arc::new(data::FsmStore::new(
        Arc::clone(artifacts),
        (*data_store).clone(),
    ));
    (data_store, data_views, fsms)
}

// シグナルハンドラの登録失敗はプロセス起動直後の致命的な環境不整合であり、
// 継続不能なため `expect` で即時 abort する（本番運用でも復帰手段が無い）。
#[allow(clippy::expect_used)]
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("ctrl_c ハンドラ設定に失敗");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM ハンドラ設定に失敗")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("シャットダウンシグナル受信");
}
