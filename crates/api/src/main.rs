//! shiki-server エントリポイント。設定 → 計装 → 依存配線 → axum 起動。

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use api::{
    config::{AppConfig, ObjectStoreBackend},
    middleware::JwksCache,
    server::build_router,
    session::RedisSessionStore,
    state::AppState,
    telemetry,
};
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    model, AuthzClient, Consistency, FgaObject, Relation, Subject,
};
use sqlx::postgres::PgPoolOptions;
use storage::{ObjectStore, S3ObjectStore, StorageService};

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
    dev_seed(&fga).await?;
    // authz は AppState と StorageService で同一インスタンスを共有する（単一チョークポイント）。
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);

    // ストレージ: backend に応じて ObjectStore を選び StorageService を構築する。
    // GCS は Phase 8 で同 trait 裏に追加する。s3 設定の必須チェックは minio の分岐内で行う
    // （gcs 選択時に s3 未設定エラーで誤って落ちないようにする）。
    let (object_store, presign_get_ttl, presign_put_ttl): (Arc<dyn ObjectStore>, _, _) =
        match config.storage.backend {
            ObjectStoreBackend::Minio => {
                let s3 = config
                    .storage
                    .s3
                    .as_ref()
                    .context("storage.s3 が未設定です（backend=minio）")?;
                (
                    Arc::new(S3ObjectStore::new(s3)) as Arc<dyn ObjectStore>,
                    s3.presign_get_ttl(),
                    s3.presign_put_ttl(),
                )
            }
            ObjectStoreBackend::Gcs => {
                anyhow::bail!("storage.backend=gcs は Phase 8 で実装予定です")
            }
        };
    object_store
        .ensure_bucket()
        .await
        .context("オブジェクトストアのバケット準備に失敗")?;
    let storage = Arc::new(StorageService::new(
        db.clone(),
        object_store,
        authz.clone(),
        presign_get_ttl,
        presign_put_ttl,
        config.storage.max_upload_size_bytes,
    ));

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

    let bind = format!("{}:{}", config.server.host, config.server.port);
    let state = AppState {
        config: Arc::new(config),
        db,
        authz,
        jwks,
        sessions,
        http,
        storage,
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

/// 開発/E2E 用の最小シード。**明示的に `SHIKI_DEV_SEED=true` が指定された時のみ**
/// `SHIKI_DEV_SEED_USER`/`SHIKI_DEV_SEED_ORG` の org への member tuple を投入する。
///
/// 任意ユーザーを任意 org の member に昇格できる権限付与経路のため、本番で env が
/// 紛れ込んでも作動しないよう、専用の有効化フラグでガードする（fail-safe）。
async fn dev_seed(fga: &OpenFgaClient) -> anyhow::Result<()> {
    if !dev_seed_enabled() {
        return Ok(());
    }
    let (Ok(user), Ok(org)) = (
        std::env::var("SHIKI_DEV_SEED_USER"),
        std::env::var("SHIKI_DEV_SEED_ORG"),
    ) else {
        return Ok(());
    };
    tracing::warn!("dev seed 有効（SHIKI_DEV_SEED=true）。本番では設定しないこと");
    let subject = Subject::user(&user);
    let object = FgaObject::organization(&org);
    // 冪等化: 既に member なら再投入しない（OpenFGA は重複 tuple を拒否するため）。
    if fga
        .check(
            &subject,
            Relation::Member,
            &object,
            Consistency::HigherConsistency,
        )
        .await?
    {
        tracing::info!(%user, %org, "dev seed: 既に member のため skip");
        return Ok(());
    }
    fga.write_tuple(&subject, Relation::Member, &object)
        .await
        .context("dev seed tuple の書き込みに失敗")?;
    tracing::info!(%user, %org, "dev seed: org member tuple を投入");
    Ok(())
}

/// dev seed の有効化フラグ（`SHIKI_DEV_SEED` が真値のときのみ true）。
fn dev_seed_enabled() -> bool {
    matches!(
        std::env::var("SHIKI_DEV_SEED").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

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
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("シャットダウンシグナル受信");
}
