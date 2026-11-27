//! HTTP ミドルウェア補助（CORS・リクエスト span）。server.rs の行数ゲート対応で分離。

use axum::extract::{MatchedPath, Request};
use axum::http::{header, HeaderName, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use tower_http::cors::CorsLayer;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;

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

/// トレース対象から外す疎通確認のパス（オーケストレータが数秒おきに叩くため、span を作らない）。
const UNTRACED_PATHS: &[&str] = &["/healthz", "/readyz"];

/// span を作る対象か（true = 作る）。
fn is_traced_path(path: &str) -> bool {
    !UNTRACED_PATHS.contains(&path)
}

/// リクエストの root span を作るミドルウェア。
///
/// span の宣言は `tracing-opentelemetry-instrumentation-sdk`（`OtelAxumLayer` と同じ部品）の
/// HTTP semconv をそのまま使い、`http.route`（ルートのテンプレート）・`otel.name` を足して、
/// 受信した W3C `traceparent` を親として継続する。応答では `http.response.status_code` を
/// **整数**で、5xx なら `otel.status_code=ERROR` を記録する（`OtelAxumLayer` は u16 で記録し、
/// tracing-opentelemetry 0.31 がそれを文字列に変えてしまうため、層は自前で持つ）。
/// `trace_id` は [`telemetry::observe`] が後から記録する（span 側で Empty 宣言済み）。
///
/// `Router::layer` で掛ける（ルートごとに包まれるので `MatchedPath` が引ける）。
///
/// [`telemetry::observe`]: crate::telemetry::observe
pub(super) async fn request_span(req: Request, next: Next) -> Response {
    if !is_traced_path(req.uri().path()) {
        return next.run(req).await;
    }
    let span = otel_http::http_server::make_span_from_request(&req);
    let method = otel_http::http_method(req.method());
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or("", MatchedPath::as_str);
    span.record("http.route", route);
    span.record("otel.name", format!("{method} {route}").trim());
    span.set_parent(otel_http::extract_context(req.headers()));

    let resp = next.run(req).instrument(span.clone()).await;
    record_response(&span, resp.status().as_u16());
    resp
}

/// 応答の status を root span に記録する（semconv: `http.response.status_code` は整数、
/// サーバ span の status は 5xx のときだけ Error・4xx 以下は Unset のまま）。
fn record_response(span: &tracing::Span, status: u16) {
    span.record("http.response.status_code", i64::from(status));
    if status >= 500 {
        span.record("otel.status_code", "ERROR");
    }
}

/// 受信したトレース文脈（`traceparent` / `tracestate`）を捨てる。
///
/// `telemetry.trust_incoming_trace_context=false`（既定）のとき [`request_span`] の外側に掛け、
/// クライアントが選んだ trace_id・sampled フラグを継続しない（毎リクエスト新しいトレース）。
pub(super) async fn strip_incoming_trace_context(mut req: Request) -> Request {
    let headers = req.headers_mut();
    headers.remove("traceparent");
    headers.remove("tracestate");
    req
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_checks_are_not_traced() {
        assert!(!is_traced_path("/healthz"));
        assert!(!is_traced_path("/readyz"));
        assert!(is_traced_path("/me"));
        assert!(is_traced_path("/healthz/extra"));
    }

    #[tokio::test]
    async fn strip_removes_only_trace_context_headers() {
        let req = Request::builder()
            .header(
                "traceparent",
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            )
            .header("tracestate", "k=v")
            .header("x-csrf-token", "t")
            .body(axum::body::Body::empty())
            .unwrap();
        let req = strip_incoming_trace_context(req).await;
        assert!(req.headers().get("traceparent").is_none());
        assert!(req.headers().get("tracestate").is_none());
        assert_eq!(req.headers().get("x-csrf-token").unwrap(), "t");
    }
}
