//! 可観測性の初期化（docs/roadmap phase-0 Task 0.8, docs/design.md §4.9）。
//!
//! - `tracing` + OpenTelemetry を OTLP(gRPC) でエクスポート（オンプレ既定の
//!   Tempo/Prometheus へ collector 経由）。
//! - リクエスト span に trace_id を載せ、ログ行にも trace_id を注入して突合可能にする。
//! - `otlp_endpoint` 未設定や collector 未起動でも**起動を妨げない**（ログのみ）。

use std::{sync::OnceLock, time::Instant};

use axum::{extract::Request, middleware::Next, response::Response};
use opentelemetry::{
    global,
    metrics::{Counter, Histogram},
    trace::{TraceContextExt, TracerProvider as _},
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    metrics::SdkMeterProvider, propagation::TraceContextPropagator, trace::SdkTracerProvider,
    Resource,
};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

use crate::config::{LogFormat, TelemetryConfig};

/// プロセス終了時に span/metric をフラッシュするためのガード。
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.tracer_provider {
            let _ = p.shutdown();
        }
        if let Some(p) = &self.meter_provider {
            let _ = p.shutdown();
        }
    }
}

/// tracing / OpenTelemetry を初期化する。
pub fn init(cfg: &TelemetryConfig) -> anyhow::Result<TelemetryGuard> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,shiki_api=debug,authz=debug"));

    let fmt_layer = match cfg.log_format {
        LogFormat::Json => tracing_subscriber::fmt::layer().json().boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer().pretty().boxed(),
    };

    let mut guard = TelemetryGuard {
        tracer_provider: None,
        meter_provider: None,
    };

    // 空文字列は「無効」扱い（compose の `${OTLP_ENDPOINT:-}` のような env 経由の
    // 未設定表現を許す。監視スタックは observability profile でオプトイン起動）。
    let otlp_endpoint = cfg
        .otlp_endpoint
        .as_deref()
        .filter(|e| !e.trim().is_empty());
    let otel_layer = if let Some(endpoint) = otlp_endpoint {
        let resource = Resource::builder()
            .with_service_name(cfg.service_name.clone())
            .build();

        // トレース
        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();
        let tracer = tracer_provider.tracer(cfg.service_name.clone());
        global::set_tracer_provider(tracer_provider.clone());
        global::set_text_map_propagator(TraceContextPropagator::new());

        // メトリクス
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()?;
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource)
            .build();
        global::set_meter_provider(meter_provider.clone());

        guard.tracer_provider = Some(tracer_provider);
        guard.meter_provider = Some(meter_provider);

        Some(tracing_opentelemetry::layer().with_tracer(tracer))
    } else {
        tracing::warn!("telemetry.otlp_endpoint 未設定: OpenTelemetry エクスポートは無効");
        None
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    Ok(guard)
}

struct HttpMetrics {
    requests: Counter<u64>,
    duration: Histogram<f64>,
}

fn http_metrics() -> &'static HttpMetrics {
    static METRICS: OnceLock<HttpMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("shiki-server");
        HttpMetrics {
            requests: meter.u64_counter("http.server.requests").build(),
            duration: meter
                .f64_histogram("http.server.duration")
                .with_unit("s")
                .build(),
        }
    })
}

/// リクエスト span に trace_id を記録し、基本メトリクス（件数/レイテンシ）を計上する。
/// span は `make_request_span` で `trace_id` フィールドを Empty 宣言済みである前提。
pub async fn observe(mut req: Request, next: Next) -> Response {
    let span = tracing::Span::current();
    let trace_id = span.context().span().span_context().trace_id();
    if trace_id != opentelemetry::trace::TraceId::INVALID {
        span.record("trace_id", tracing::field::display(trace_id));
        // ハンドラ（監査ログ）が trace_id を参照できるよう extension に載せる。
        req.extensions_mut()
            .insert(crate::extract::TraceId(trace_id.to_string()));
    }

    let method = req.method().as_str().to_owned();
    let path = req.uri().path().to_owned();
    let start = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16();
    let latency_ms = start.elapsed().as_secs_f64() * 1000.0;

    let metrics = http_metrics();
    let attrs = [
        KeyValue::new("method", method.clone()),
        KeyValue::new("status", i64::from(status)),
    ];
    metrics.requests.add(1, &attrs);
    metrics
        .duration
        .record(start.elapsed().as_secs_f64(), &attrs);

    // ログにも trace_id を直接載せ、Tempo のトレースと突合できるようにする。
    tracing::info!(
        trace_id = %trace_id,
        method = %method,
        path = %path,
        status,
        latency_ms,
        "request"
    );

    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `init` 内の fmt レイヤ選択ロジックを、グローバル初期化を伴わずに再現する。
    /// `.init()` はプロセス共有のため呼ばず、レイヤ構築（=分岐到達）のみ検証する。
    /// subscriber 型 `S` は実コード同様 registry に対するもので、ここでは明示する。
    fn build_fmt_layer(
        format: LogFormat,
    ) -> Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync> {
        match format {
            LogFormat::Json => tracing_subscriber::fmt::layer().json().boxed(),
            LogFormat::Pretty => tracing_subscriber::fmt::layer().pretty().boxed(),
        }
    }

    #[test]
    fn fmt_layer_builds_for_both_formats() {
        // json / pretty いずれの選択でもレイヤ構築がパニックしないこと。
        let _json = build_fmt_layer(LogFormat::Json);
        let _pretty = build_fmt_layer(LogFormat::Pretty);
    }

    #[test]
    fn telemetry_guard_drop_is_safe_when_empty() {
        // OTLP 未設定相当（provider 無し）のガード drop が安全（no-op）であること。
        let guard = TelemetryGuard {
            tracer_provider: None,
            meter_provider: None,
        };
        drop(guard);
    }

    fn telemetry_config(otlp_endpoint: Option<&str>, log_format: LogFormat) -> TelemetryConfig {
        TelemetryConfig {
            otlp_endpoint: otlp_endpoint.map(str::to_string),
            service_name: "shiki-server-test".into(),
            log_format,
        }
    }

    #[test]
    fn telemetry_config_otlp_absent_is_none() {
        // otlp_endpoint 未設定（OTel 無効）の構成を表現できること。
        let cfg = telemetry_config(None, LogFormat::Json);
        assert!(cfg.otlp_endpoint.is_none());
        assert_eq!(cfg.log_format, LogFormat::Json);
    }
}
