//! HTTP ミドルウェア補助（CORS・リクエスト span）。server.rs の行数ゲート対応で分離。

use std::time::Duration;

use axum::extract::{MatchedPath, Request};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method};
use axum::response::Response;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::{SpanContext, TraceContextExt};
use tower_http::cors::CorsLayer;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;

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

/// リクエスト span（OTel の HTTP server span）。
///
/// - 受信した W3C `traceparent` を親として継続する（BFF などの上流とトレースをつなぐ）。
///   OTel 無効時は global propagator が no-op なので親は付かない。
/// - 属性は OTel semconv の HTTP server span に合わせる。`http.route` はルートの型
///   （`/files/{id}` など）で、生の ID を含む `url.path` とは別に持つ。
/// - **`url.query` は載せない。** WOPI の `access_token` のように資格情報がクエリで来る
///   経路があり、span 属性はそのまま Tempo に残るため（`axum-tracing-opentelemetry` の
///   既定 span はクエリを載せるので使わない）。
/// - `trace_id` は [`telemetry::observe`] が、status は [`record_response`] が後から記録する。
///
/// [`telemetry::observe`]: crate::telemetry::observe
pub(super) fn make_request_span(req: &Request) -> tracing::Span {
    let method = req.method().as_str();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    let span = tracing::info_span!(
        "http_request",
        otel.name = %span_name(method, route),
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = %method,
        http.route = route,
        http.response.status_code = Empty,
        url.path = %req.uri().path(),
        // 変更前からある名前。ログ（JSON の span 欄）の検索・集計が参照しうるので残す。
        method = %method,
        path = %req.uri().path(),
        trace_id = Empty,
    );
    span.set_parent(extract_parent(req.headers()));
    span
}

/// semconv の span 名（`{method} {route}`）。ルート不一致（404 など）は method だけにする
/// （生のパスを名前に入れると span 名の種類が際限なく増えるため）。
fn span_name(method: &str, route: Option<&str>) -> String {
    match route {
        Some(route) => format!("{method} {route}"),
        None => method.to_owned(),
    }
}

/// 受信ヘッダから親のトレース文脈を取り出す（global propagator 経由）。
///
/// trace_id・親 span id は継続するが、**sampled フラグは信用しない**。ブラウザなど外部の
/// クライアントが `traceparent` の flags を `00` にすると、ParentBased サンプラが従って
/// 5xx を含むリクエストの span が記録されなくなるため、受信した親は常に sampled として扱う
/// （記録するかどうかは api 側の方針で決め、上流に下げさせない）。
fn extract_parent(headers: &HeaderMap) -> opentelemetry::Context {
    let cx =
        opentelemetry::global::get_text_map_propagator(|p| p.extract(&HeaderExtractor(headers)));
    let remote = cx.span().span_context().clone();
    if !remote.is_valid() {
        return cx;
    }
    let sampled = SpanContext::new(
        remote.trace_id(),
        remote.span_id(),
        remote.trace_flags().with_sampled(true),
        true,
        remote.trace_state().clone(),
    );
    opentelemetry::Context::new().with_remote_span_context(sampled)
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}

/// 応答の status を span に記録する。5xx は span の status を ERROR にする
/// （semconv: server span では 4xx は ERROR にしない）。
pub(super) fn record_response(res: &Response, _latency: Duration, span: &tracing::Span) {
    let status = res.status();
    span.record("http.response.status_code", i64::from(status.as_u16()));
    if status.is_server_error() {
        span.record("otel.status_code", "error");
    }
}

#[cfg(test)]
#[path = "http_util_tests.rs"]
mod tests;
