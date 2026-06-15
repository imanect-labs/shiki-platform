//! ルータ構築。公開ルート（認証不要）と保護ルート（要認証）を組み立てる。

use std::time::Duration;

use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware,
    response::IntoResponse,
    routing::get,
    Router,
};
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer, trace::TraceLayer};

use crate::{health, middleware::require_auth, openapi, routes, state::AppState, telemetry};

/// アプリの axum ルータを構築する（テストからも利用）。
pub fn build_router(state: AppState) -> Router {
    // 保護ルート: require_auth を通過しないと到達できない。
    let protected = Router::new()
        .route("/me", get(routes::get_me))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    // 公開ルート: 認証不要。
    let public = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/api-docs/openapi.json", get(openapi_handler));

    public
        .merge(protected)
        // observe は span 内で動く必要があるため TraceLayer より内側（先に追加）。
        .layer(middleware::from_fn(telemetry::observe))
        .layer(TraceLayer::new_for_http().make_span_with(make_request_span))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
        .layer(CorsLayer::permissive())
        .with_state(state)
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
