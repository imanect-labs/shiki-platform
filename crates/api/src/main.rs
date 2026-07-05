//! shiki-server エントリポイント。設定 → 計装 → 依存配線 → axum 起動。

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use api::{
    config::{AppConfig, AuthConfig, ObjectStoreBackend, Tenancy},
    middleware::JwksCache,
    server::build_router,
    session::RedisSessionStore,
    state::AppState,
    telemetry,
};
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    model, AuthContext, AuthzClient, Consistency, Principal, Relation,
};
use sqlx::postgres::PgPoolOptions;
use storage::{DirectoryStore, ObjectStore, S3ObjectStore, StorageService, TenantStore};

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
    dev_seed(&fga, &directory, &config.auth).await?;
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
        // 生 PgPool は StorageService 等のチョークポイントにのみ渡し、AppState には
        // readiness 専用の newtype で載せる（#91 M-2・ハンドラの生 SQL を型で防ぐ）。
        db: api::state::ReadinessProbe::new(db),
        authz,
        jwks,
        sessions,
        http,
        storage,
        directory,
        tenants,
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

/// dev fixture の 1 ユーザー（OIDC sub / tenant / org / email / 表示名）。
/// realm（`deploy/keycloak/shiki-realm.json`）の sub・tenant 属性・group と一致させる。
struct SeedUser {
    id: &'static str,
    tenant: &'static str,
    org: &'static str,
    email: &'static str,
    display_name: &'static str,
}

/// dev/E2E の固定ユーザー群。**alice/bob は tenant `a-corp`、charlie は別 tenant `b-corp`**。
/// これによりテナント分離（charlie が alice の検索/共有に出ない）を検証できる。
const SEED_USERS: &[SeedUser] = &[
    SeedUser {
        id: "00000000-0000-0000-0000-000000000001",
        tenant: "a-corp",
        org: "a-corp",
        email: "alice@a-corp.example.com",
        display_name: "Alice",
    },
    SeedUser {
        id: "00000000-0000-0000-0000-000000000002",
        tenant: "a-corp",
        org: "a-corp",
        email: "bob@a-corp.example.com",
        display_name: "Bob",
    },
    SeedUser {
        id: "00000000-0000-0000-0000-000000000003",
        tenant: "b-corp",
        org: "b-corp",
        email: "charlie@b-corp.example.com",
        display_name: "Charlie",
    },
];

/// dev fixture の 1 ロール/部署（#76 role 共有の検証用）。`members` は所属ユーザーの sub。
struct SeedRole {
    tenant: &'static str,
    org: &'static str,
    id: &'static str,
    display_name: &'static str,
    members: &'static [&'static str],
}

/// dev/E2E の固定ロール群。a-corp の「営業部」に alice/bob が所属する
/// （部署へ共有すると両者に反映されることを検証できる）。
const SEED_ROLES: &[SeedRole] = &[SeedRole {
    tenant: "a-corp",
    org: "a-corp",
    id: "sales",
    display_name: "営業部",
    members: &[
        "00000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000002",
    ],
}];

/// 開発/E2E 用の最小シード。**明示的に `SHIKI_DEV_SEED=true` が指定された時のみ**、
/// 固定ユーザー群（[`SEED_USERS`]）を ① OpenFGA の org member tuple ② ユーザーディレクトリ
/// （共有相手検索の backing）へ冪等投入する。
///
/// 任意ユーザーを任意 org の member に昇格できる権限付与経路のため、本番で env が
/// 紛れ込んでも作動しないよう、専用の有効化フラグでガードする（fail-safe）。
async fn dev_seed(
    fga: &OpenFgaClient,
    directory: &DirectoryStore,
    auth: &AuthConfig,
) -> anyhow::Result<()> {
    if !dev_seed_enabled() {
        return Ok(());
    }
    tracing::warn!("dev seed 有効（SHIKI_DEV_SEED=true）。本番では設定しないこと");
    for u in SEED_USERS {
        // **実行時と同じ tenant 名前空間へ seed する**（SAAS.1）。single モードでは runtime の
        // `resolve_tenant_id` が固定 `auth.tenant_id` を使うため、fixture の `u.tenant`（a-corp 等）で
        // 書くと `user:<u.tenant>|...` になり `user:<auth.tenant_id>|...` の check と一致せず未認可に
        // なる。よって single では `auth.tenant_id` を、multi では claim 相当の `u.tenant` を使う。
        let seed_tenant = effective_seed_tenant(auth, u.tenant);
        let ctx = AuthContext::new(
            Principal {
                id: u.id.to_string(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(seed_tenant.to_string()),
            },
            u.org.to_string(),
            seed_tenant.to_string(),
        );
        let subject = ctx.subject();
        let object = ctx.ns().organization(u.org);
        // 冪等化: 既に member なら再投入しない（OpenFGA は重複 tuple を拒否するため）。
        if !fga
            .check(
                &subject,
                Relation::Member,
                &object,
                Consistency::HigherConsistency,
            )
            .await?
        {
            fga.write_tuple(&subject, Relation::Member, &object)
                .await
                .context("dev seed tuple の書き込みに失敗")?;
            tracing::info!(user = %u.id, org = %u.org, tenant = %seed_tenant, "dev seed: org member tuple を投入");
        }
        // ディレクトリ（共有相手検索）へ投入。ON CONFLICT 更新で冪等。
        directory
            .upsert_user(u.id, seed_tenant, u.org, u.email, u.display_name)
            .await
            .context("dev seed: directory_user の投入に失敗")?;
    }
    tracing::info!(count = SEED_USERS.len(), "dev seed: ユーザー群を投入");

    // role/部署（#76 共有の検証用）: メンバーシップタプルと directory_role を冪等投入する。
    for r in SEED_ROLES {
        // ユーザー seed と同じく **実行時の実効テナント**へ書く（single では auth.tenant_id）。
        // fixture の r.tenant を直接使うとユーザーと別 tenant に書かれ role 共有が機能しない。
        let seed_tenant = effective_seed_tenant(auth, r.tenant);
        for member in r.members {
            let ctx = AuthContext::new(
                Principal {
                    id: (*member).to_string(),
                    email: None,
                    groups: vec![],
                    roles: vec![],
                    tenant_id: Some(seed_tenant.to_string()),
                },
                r.org.to_string(),
                seed_tenant.to_string(),
            );
            let subject = ctx.subject();
            let role_obj = ctx.ns().role(r.id);
            if !fga
                .check(
                    &subject,
                    Relation::Member,
                    &role_obj,
                    Consistency::HigherConsistency,
                )
                .await?
            {
                fga.write_tuple(&subject, Relation::Member, &role_obj)
                    .await
                    .context("dev seed: role member tuple の書き込みに失敗")?;
            }
        }
        directory
            .upsert_role(r.id, seed_tenant, r.org, r.display_name)
            .await
            .context("dev seed: directory_role の投入に失敗")?;
    }
    tracing::info!(count = SEED_ROLES.len(), "dev seed: ロール群を投入");
    Ok(())
}

/// dev seed の実効テナント。single は runtime と一致させるため固定 `auth.tenant_id` を、
/// multi は fixture の tenant（claim 相当）を使う。
fn effective_seed_tenant<'a>(auth: &'a AuthConfig, fixture_tenant: &'a str) -> &'a str {
    match auth.tenancy {
        Tenancy::Single => auth
            .tenant_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("default"),
        Tenancy::Multi => fixture_tenant,
    }
}

/// dev seed の有効化フラグ（`SHIKI_DEV_SEED` が真値のときのみ true）。
fn dev_seed_enabled() -> bool {
    matches!(
        std::env::var("SHIKI_DEV_SEED").ok().as_deref(),
        Some("1" | "true" | "TRUE")
    )
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
