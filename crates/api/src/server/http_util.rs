//! HTTP ミドルウェア補助（CORS・リクエスト span）。server.rs の行数ゲート対応で分離。

use axum::extract::Request;
use axum::http::{header, HeaderName, HeaderValue, Method};
use axum_tracing_opentelemetry::middleware::OtelAxumLayer;
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

/// トレース対象から外す疎通確認のパス（オーケストレータが数秒おきに叩くため、span を作らない）。
const UNTRACED_PATHS: &[&str] = &["/healthz", "/readyz"];

/// `OtelAxumLayer` の対象判定（true = span を作る）。
fn is_traced_path(path: &str) -> bool {
    !UNTRACED_PATHS.contains(&path)
}

/// リクエストの root span を作るレイヤ。
///
/// OTel の HTTP semconv（`http.request.method`・`http.route`＝テンプレート・
/// `http.response.status_code`・5xx で `otel.status_code=ERROR`）を載せ、受信した W3C
/// `traceparent` を親として継続する。`trace_id` は [`telemetry::observe`] が後から記録する
/// （span 側で Empty 宣言済み）。
///
/// [`telemetry::observe`]: crate::telemetry::observe
pub(super) fn request_span_layer() -> OtelAxumLayer {
    OtelAxumLayer::default().filter(is_traced_path)
}

/// 受信したトレース文脈（`traceparent` / `tracestate`）を捨てる。
///
/// `telemetry.trust_incoming_trace_context=false` のときだけ [`request_span_layer`] の外側に
/// 掛け、クライアントが選んだ trace_id・sampled フラグを継続しない（毎リクエスト新しいトレース）。
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
