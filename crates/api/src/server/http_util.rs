//! HTTP ミドルウェア補助（CORS・リクエスト span）。server.rs の 500 行ゲート対応で分離。

use axum::extract::Request;
use axum::http::{header, HeaderName, HeaderValue, Method};
use tower_http::cors::CorsLayer;

/// 設定されたオリジンに限定した CORS レイヤを構築する（空なら `None` = レイヤ無効）。
pub(super) fn cors_layer(origins: &[String]) -> Option<CorsLayer> {
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
pub(super) fn make_request_span(req: &Request) -> tracing::Span {
    tracing::info_span!(
        "http_request",
        method = %req.method(),
        path = %req.uri().path(),
        trace_id = tracing::field::Empty,
    )
}
