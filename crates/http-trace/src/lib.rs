//! 内部サービス向けの外向き HTTP に、呼び出しごとの client span と W3C `traceparent` を付ける。
//!
//! - 使い方: `http.post(url).json(&body).send_traced().await` のように、`.send()` の代わりに
//!   [`TracedSend::send_traced`] を呼ぶ。
//! - **付けるのは内部サービス（OpenFGA・Qdrant・ingestion-worker・Langfuse）だけ。** 外部の
//!   LLM プロバイダ・公開 Web の取得・外部 OIDC はトレース ID を外へ出さないため `.send()` のまま
//!   にする（送り先の方針が決まるまでの暫定。呼び出し点で内部か外部かを明示する）。
//! - inject は [`TraceContextPropagator`] を直接使う。global propagator の設定有無に左右されず、
//!   span の文脈が無効（OTel 無効時）なら何も付けない。

use std::future::Future;

use opentelemetry::propagation::{Injector, TextMapPropagator};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tracing::{field::Empty, Instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// [`TracedSend::send_traced`] で使う内部サービス向けクライアントの builder。
///
/// reqwest は別 origin へのリダイレクトでも `traceparent` を消さずに付け直すため、既定の追従を
/// 許すと内部サービスが返した 3xx 経由でトレース ID が外部へ出る。同じ origin（scheme・host・port）
/// へのリダイレクトだけ追い（従来の挙動を保つ）、別 origin へは追わずに 3xx をそのまま返す。
pub fn internal_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().redirect(same_origin_redirect_policy())
}

/// 同じ origin へのリダイレクトだけ追う（上限は reqwest 既定と同じ 10 回）。
fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|origin| same_origin(origin, attempt.url()));
        if !same_origin {
            attempt.stop()
        } else if attempt.previous().len() > 10 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// `reqwest::RequestBuilder` を client span 付きで送る拡張。
///
/// クライアントは [`internal_client_builder`] で作ったものを使う（リダイレクト先への漏れ防止）。
pub trait TracedSend {
    /// client span を切り、その文脈を `traceparent` として載せて送る。
    fn send_traced(self) -> impl Future<Output = reqwest::Result<reqwest::Response>> + Send;
}

impl TracedSend for reqwest::RequestBuilder {
    fn send_traced(self) -> impl Future<Output = reqwest::Result<reqwest::Response>> + Send {
        let (client, request) = self.build_split();
        async move {
            let mut request = request?;
            let span = client_span(&request);
            inject(&span, request.headers_mut());
            let result = client.execute(request).instrument(span.clone()).await;
            record_outcome(&span, &result);
            result
        }
    }
}

/// OTel の HTTP client span の semconv（`http.request.method`・`server.address` 等）に沿う span。
/// span 名は semconv どおり HTTP メソッド（`otel.name`）。URL はクエリに秘密を含みうるので載せない。
fn client_span(request: &reqwest::Request) -> tracing::Span {
    let method = request.method().as_str();
    let url = request.url();
    tracing::info_span!(
        "http.client",
        otel.name = method,
        otel.kind = "client",
        otel.status_code = Empty,
        http.request.method = method,
        server.address = url.host_str().unwrap_or_default(),
        server.port = url.port_or_known_default(),
        http.response.status_code = Empty,
        "error.type" = Empty,
    )
}

fn inject(span: &tracing::Span, headers: &mut HeaderMap) {
    let cx = span.context();
    TraceContextPropagator::new().inject_context(&cx, &mut HeaderInjector(headers));
}

fn record_outcome(span: &tracing::Span, result: &reqwest::Result<reqwest::Response>) {
    match result {
        Ok(resp) => {
            let status = resp.status();
            span.record("http.response.status_code", status.as_u16());
            // semconv: client span は 4xx/5xx をエラーにする。
            if status.is_client_error() || status.is_server_error() {
                span.record("otel.status_code", "ERROR");
                span.record("error.type", status.as_str());
            }
        }
        Err(e) => {
            span.record("otel.status_code", "ERROR");
            let kind = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else {
                "request"
            };
            span.record("error.type", kind);
        }
    }
}

struct HeaderInjector<'a>(&'a mut HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            self.0.insert(name, value);
        }
    }
}
