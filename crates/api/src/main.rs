//! shiki-server エントリポイント。設定 → 計装 → 依存配線 → axum 起動。

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use api::{
    config::AppConfig, middleware::JwksCache, server::build_router, session::RedisSessionStore,
    state::AppState, telemetry,
};
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    model, AuthzClient, FgaObject, Relation, Subject,
};
use sqlx::postgres::PgPoolOptions;

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
        authz: Arc::new(fga),
        jwks,
        sessions,
        http,
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

/// 開発/E2E 用の最小シード。`SHIKI_DEV_SEED_USER` と `SHIKI_DEV_SEED_ORG` が
/// 両方設定されている時のみ、その org への member tuple を投入する。
async fn dev_seed(fga: &OpenFgaClient) -> anyhow::Result<()> {
    let (Ok(user), Ok(org)) = (
        std::env::var("SHIKI_DEV_SEED_USER"),
        std::env::var("SHIKI_DEV_SEED_ORG"),
    ) else {
        return Ok(());
    };
    let subject = Subject::user(&user);
    let object = FgaObject::organization(&org);
    // 冪等化: 既に member なら再投入しない（OpenFGA は重複 tuple を拒否するため）。
    if fga.check(&subject, Relation::Member, &object).await? {
        tracing::info!(%user, %org, "dev seed: 既に member のため skip");
        return Ok(());
    }
    fga.write_tuple(&subject, Relation::Member, &object)
        .await
        .context("dev seed tuple の書き込みに失敗")?;
    tracing::info!(%user, %org, "dev seed: org member tuple を投入");
    Ok(())
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
