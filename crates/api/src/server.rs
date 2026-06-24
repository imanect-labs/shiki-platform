//! ルータ構築。公開ルート（認証不要）と保護ルート（要認証）を組み立てる。

use std::time::Duration;

use axum::{
    extract::Request,
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{get, patch, post, put},
    Router,
};
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer, trace::TraceLayer};

use crate::{health, middleware::require_session, openapi, routes, state::AppState, telemetry};

/// アプリの axum ルータを構築する（テストからも利用）。
pub fn build_router(state: AppState) -> Router {
    let session_layer = middleware::from_fn_with_state(state.clone(), require_session);
    let standard_timeout =
        || TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30));

    // 標準保護ルート（短い 30s タイムアウト）。
    let protected_standard = Router::new()
        .route("/me", get(routes::get_me))
        .route_layer(session_layer.clone())
        .layer(standard_timeout());

    // フォルダ階層＋共有ルート（メタ操作のみ＝標準 30s タイムアウト）。
    let protected_nodes = Router::new()
        .route("/folders", post(routes::folders::create_folder))
        .route(
            "/folders/{id}",
            patch(routes::folders::update_folder).delete(routes::folders::delete_folder),
        )
        .route("/nodes", get(routes::folders::list_children))
        .route("/nodes/{id}/breadcrumb", get(routes::folders::breadcrumb))
        .route(
            "/nodes/{id}/shares",
            put(routes::shares::share_node)
                .delete(routes::shares::unshare_node)
                .get(routes::shares::list_shares),
        )
        .route(
            "/shares/shared-with-me",
            get(routes::shares::shared_with_me),
        )
        .route_layer(session_layer.clone())
        .layer(standard_timeout());

    // ファイルルート: finalize は staging を server-side でハッシュ＋コピーするため 30s では
    // 足りない（大容量で 408 になり、バイトは MinIO にあるのに file が作れない事故を防ぐ）。
    // 長め(300s)のタイムアウトを当て、グローバル 30s からは除外する。
    let protected_files = Router::new()
        .route("/files", post(routes::files::begin_upload))
        .route(
            "/files/{id}",
            get(routes::files::get_file)
                .patch(routes::files::update_file)
                .delete(routes::files::delete_file),
        )
        .route(
            "/files/{upload_id}/finalize",
            post(routes::files::finalize_upload),
        )
        .route("/files/{id}/download-url", get(routes::files::download_url))
        .route("/files/{id}/restore", post(routes::files::restore_file))
        .route_layer(session_layer)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(300),
        ));

    // 公開ルート: 認証不要。BFF 認証エンドポイント（/auth/*）もここ
    // （セッション確立前に叩くため。logout は内部で CSRF を自己検証する）。
    let public = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/auth/login", get(routes::auth::login))
        .route("/auth/callback", get(routes::auth::callback))
        .route("/auth/logout", post(routes::auth::logout))
        .route("/auth/session", get(routes::auth::session))
        .route("/api-docs/openapi.json", get(openapi_handler))
        .layer(standard_timeout());

    let router = public
        .merge(protected_standard)
        .merge(protected_nodes)
        .merge(protected_files)
        // observe は span 内で動く必要があるため TraceLayer より内側（先に追加）。
        .layer(middleware::from_fn(telemetry::observe))
        .layer(TraceLayer::new_for_http().make_span_with(make_request_span));

    // CORS: 同一オリジン配信が既定（レイヤ無し）。別オリジン dev のみ、設定された
    // オリジンに限定して credential 付きを許可する（permissive はセッション Cookie と
    // 併用すると危険なので使わない）。
    let router = match cors_layer(&state.config.server.cors_allowed_origins) {
        Some(cors) => router.layer(cors),
        None => router,
    };

    router.with_state(state)
}

/// 設定されたオリジンに限定した CORS レイヤを構築する（空なら `None` = レイヤ無効）。
fn cors_layer(origins: &[String]) -> Option<CorsLayer> {
    if origins.is_empty() {
        return None;
    }
    let parsed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| o.parse::<HeaderValue>().ok())
        .collect();
    if parsed.is_empty() {
        tracing::warn!("cors_allowed_origins が全て不正なため CORS を無効化");
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(parsed)
            .allow_credentials(true)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                header::CONTENT_TYPE,
                HeaderName::from_static("x-csrf-token"),
            ]),
    )
}

/// リクエスト span。`trace_id` は [`telemetry::observe`] が後から記録するため Empty 宣言する。
fn make_request_span(req: &Request) -> tracing::Span {
    tracing::info_span!(
        "http_request",
        method = %req.method(),
        path = %req.uri().path(),
        trace_id = tracing::field::Empty,
    )
}

async fn openapi_handler() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        openapi::openapi_json(),
    )
}
