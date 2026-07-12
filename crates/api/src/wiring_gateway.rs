//! 公開 API ゲートウェイ（Task 9.6/9.8）の配線。wiring.rs から切り出す（1 ファイル 500 行規約）。

use std::sync::Arc;

use anyhow::Context;
use authz::AuthzClient;

use api::config::AppConfig;

/// `rag::SearchService` を app-gateway の [`app_gateway::RagPort`] へ適合させるラッパ。
///
/// permission-aware 検索（pre-filter＋OpenFGA post-filter）をそのまま通すため、アプリ経由でも
/// 呼出ユーザーが読めない文書は結果に混入しない（Task 9.8 受け入れ条件）。
struct GatewayRagPort(Arc<rag::SearchService>);

#[async_trait::async_trait]
impl app_gateway::RagPort for GatewayRagPort {
    async fn query(
        &self,
        ctx: &authz::AuthContext,
        query: &str,
        top_k: Option<u32>,
        trace_id: Option<&str>,
    ) -> Result<Vec<app_gateway::RagHit>, app_gateway::GatewayError> {
        let out = self
            .0
            .search(ctx, query, top_k, rag::SearchMode::Hybrid, None, trace_id)
            .await
            .map_err(|e| app_gateway::GatewayError::Upstream(format!("rag: {e}")))?;
        Ok(out
            .results
            .into_iter()
            .map(|r| app_gateway::RagHit {
                chunk_id: r.chunk_id,
                file_id: r.file_id,
                file_name: r.file_name,
                page: r.page,
                heading_path: r.heading_path,
                content: r.content,
                score: r.score,
            })
            .collect())
    }
}

/// 公開 API ゲートウェイ（Task 9.6/9.8/9.9）の第2リスナ用 `Router` を組む（無効時は `None`）。
///
/// 内部 API（cookie セッション）とは別ポート＝別オリジンで待ち受ける Bearer 専用の面。
/// JWKS は内部 API と同じ `JwksCache` を共有し（同一 issuer）、認可は同一 OpenFGA
/// クライアント（`authz`）を共有する（鍵/認可のチョークポイントを一本化する）。
/// 能力アダプタの委譲先（storage/data/fsm/rag/llm）も**内部 API と同一チョークポイント**。
#[allow(clippy::too_many_arguments)] // 依存束の注入点（main からの一回きり）。
pub(crate) fn wire_gateway(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    jwks: &Arc<api::middleware::JwksCache>,
    authz: &Arc<dyn AuthzClient>,
    storage_service: &Arc<storage::StorageService>,
    data_store: &Arc<data::DataStore>,
    fsms: &Arc<data::FsmStore>,
    search: Option<&Arc<rag::SearchService>>,
) -> Option<axum::Router> {
    if !config.gateway.enabled {
        return None;
    }
    let keys: Arc<dyn app_gateway::KeyResolver> = jwks.clone();
    let rag_port: Arc<dyn app_gateway::RagPort> = match search {
        Some(s) => Arc::new(GatewayRagPort(Arc::clone(s))),
        None => Arc::new(app_gateway::NoRag),
    };
    // llm.invoke / agent.invoke（Task 9.9）: llm-gateway は chat と同じ config.llm から構築
    // （会計は同一 DB の llm_usage・app_id 付き）。構築失敗（設定不備）は AI 能力のみ無効化する。
    let llm = match crate::wiring::build_gateway(config, http, db) {
        Ok(g) => Some(Arc::new(g)),
        Err(e) => {
            tracing::warn!(error = %e, "llm-gateway 構築に失敗（AI 能力は 502 で応答）");
            None
        }
    };
    let agent: Arc<dyn app_gateway::AgentPort> = match &llm {
        Some(llm) => Arc::new(crate::gateway_ai::GatewayAgentPort {
            llm: Arc::clone(llm),
            search: search.map(Arc::clone),
        }),
        None => Arc::new(app_gateway::NoAgent),
    };
    let state = app_gateway::GatewayState {
        installations: app_gateway::AppInstallationStore::new(db.clone()),
        keys,
        token_cfg: app_gateway::GatewayTokenConfig {
            audience: config.gateway.audience.clone(),
            issuer: config.auth.issuer.clone(),
        },
        authz: authz.clone(),
        audit: storage::audit::AuditRecorder::new(db.clone()),
        // multi テナンシーでは tenant クレームを必須にする（fail-closed）。
        require_tenant_claim: matches!(config.auth.tenancy, api::config::Tenancy::Multi),
        default_tenant: config
            .auth
            .tenant_id
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        default_org: "default".to_string(),
        caps: app_gateway::CapabilityDeps {
            db: db.clone(),
            storage: Arc::clone(storage_service),
            data: Arc::clone(data_store),
            fsms: Arc::clone(fsms),
            rag: rag_port,
            notifications: app_gateway::NotificationStore::new(db.clone()),
            llm,
            agent,
            ai_daily_cap_usd_micros: config.gateway.ai_daily_cap_usd_micros,
        },
    };
    Some(app_gateway::build_gateway_router(state))
}

/// 公開 API ゲートウェイの第2リスナを spawn する（router/bind が揃ったときのみ）。
///
/// 別ポート＝別オリジンでミニアプリ向けの面を提供する。graceful shutdown はプロセス終了で
/// 代替する（alpha・メインリスナの停止で全体が落ちる）。
pub(crate) async fn spawn_gateway_listener(
    router: Option<axum::Router>,
    bind: Option<String>,
) -> anyhow::Result<()> {
    if let (Some(router), Some(bind)) = (router, bind) {
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .context("gateway bind に失敗")?;
        tracing::info!(%bind, "gateway listening（第2リスナ）");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!(error = %e, "ゲートウェイ・リスナが停止しました");
            }
        });
    }
    Ok(())
}

/// 同意インストール（Task 9.13b）の InstallService を組む。
///
/// Keycloak admin（provisioner 資格情報＋admin base）が構成済みなら client 登録も配線する。
/// 未構成（dev/テスト）は登録スキップ（InstallService 側で warn）。
pub(crate) fn wire_installs(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
    mini_app_code: &Arc<app_platform::MiniAppCodeStore>,
    data_store: &Arc<data::DataStore>,
) -> app_platform::InstallService {
    let oauth = if let (Some(base), Some((id, secret))) = (
        config.auth.admin_base(),
        config.auth.provisioner_credentials(),
    ) {
        Some(app_gateway::OAuthClient::new(
            http.clone(),
            base,
            config.auth.token_endpoint(),
            id.to_string(),
            secret.to_string(),
        ))
    } else {
        tracing::info!("Keycloak admin 未構成: インストール時の client 登録は無効（dev）");
        None
    };
    // B1 redirect は web シェルの popup callback（PR10 が消費）。web origin 由来。
    let b1_redirects = vec![config.auth.redirect_uri.clone()];
    app_platform::InstallService::new(
        db.clone(),
        app_platform::Registry::new(db.clone()),
        Arc::clone(mini_app_code),
        Arc::clone(data_store),
        authz.clone(),
        oauth,
        b1_redirects,
    )
}

/// B1 フロントバンドル配信（第3リスナ・Task 9.11）の Router を組む（gateway 無効時は None）。
///
/// apps オリジン＝第2リスナともホストとも別ポート。cookie を持たず、CSP はゲートウェイ
/// への connect とホストからの埋め込みのみを許す（B1State の rustdoc 参照）。
pub(crate) fn wire_b1(
    config: &AppConfig,
    db: &sqlx::PgPool,
    object_store: &Arc<dyn storage::ObjectStore>,
) -> Option<axum::Router> {
    if !config.gateway.enabled {
        return None;
    }
    let gateway_origin = config
        .gateway
        .public_origin
        .clone()
        .unwrap_or_else(|| format!("http://localhost:{}", config.gateway.port));
    // frame-ancestors は web シェルのオリジン（未設定は auth.redirect_uri から導出）。
    let host_origin = config.gateway.web_origin.clone().unwrap_or_else(|| {
        url::Url::parse(&config.auth.redirect_uri)
            .ok()
            .and_then(|u| {
                u.port_or_known_default().map(|p| {
                    format!(
                        "{}://{}:{p}",
                        u.scheme(),
                        u.host_str().unwrap_or("localhost")
                    )
                })
            })
            .unwrap_or_else(|| "http://localhost:3000".to_string())
    });
    let state = app_gateway::B1State {
        installations: app_gateway::AppInstallationStore::new(db.clone()),
        store: Arc::clone(object_store),
        gateway_origin,
        host_origin,
    };
    Some(app_gateway::build_b1_router(state))
}
