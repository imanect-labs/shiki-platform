"""OpenTelemetry の初期化（トレース）。

- shiki-server（`crates/api/src/telemetry.rs`）と同じく、`OTEL_EXPORTER_OTLP_ENDPOINT` が
  未設定・空ならエクスポートしない。collector が居なくても起動を妨げない（警告ログのみ）。
- 受信した `traceparent`（W3C TraceContext・OTel の既定 propagator）を継続する。Rust 側の
  呼び出し span の下に、worker のサーバ span と推論 span がつながる。
- `/healthz`（compose の healthcheck が数秒おきに叩く）はトレースから外す。
- 推論の span（`inference_span`）に載せるのは件数・ページ数・モデル名・バッチサイズ等の
  数値と識別子だけ。文書の中身・クエリ・ファイル名は載せない。
"""

import logging
import os
from collections.abc import Iterator, Mapping
from contextlib import contextmanager

from fastapi import FastAPI
from opentelemetry import trace
from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.trace import StatusCode

logger = logging.getLogger(__name__)

SERVICE_NAME = "ingestion-worker"
# FastAPIInstrumentor の excluded_urls は正規表現の部分一致（カンマ区切り）。
_EXCLUDED_URLS = r"/healthz$"
_TRACER_NAME = "ingestion_worker"

# 推論 span が使う provider。None ならグローバル（未設定なら no-op）を使う。
_provider: trace.TracerProvider | None = None


def otlp_endpoint() -> str | None:
    """エクスポート先。空文字列は「無効」扱い（compose の `${OTLP_ENDPOINT:-}` を許す）。"""
    endpoint = os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT", "").strip()
    return endpoint or None


def _build_provider() -> TracerProvider | None:
    if otlp_endpoint() is None:
        logger.warning("OTEL_EXPORTER_OTLP_ENDPOINT 未設定: OpenTelemetry エクスポートは無効")
        return None
    # 送り先・プロトコル以外の設定（ヘッダ・タイムアウト等）は OTEL_* 環境変数で exporter が読む。
    from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter

    service_name = os.environ.get("OTEL_SERVICE_NAME", "").strip() or SERVICE_NAME
    provider = TracerProvider(resource=Resource.create({"service.name": service_name}))
    # BatchSpanProcessor はバックグラウンドで送る。collector 不在でも推論は止まらない。
    provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))
    trace.set_tracer_provider(provider)
    return provider


def setup_tracing(app: FastAPI, tracer_provider: trace.TracerProvider | None = None) -> None:
    """アプリにトレースを仕掛ける。`tracer_provider` はテスト用（未指定なら環境変数から作る）。"""
    global _provider  # noqa: PLW0603
    provider = tracer_provider if tracer_provider is not None else _build_provider()
    _provider = provider
    if provider is None:
        return
    # ASGI の receive/send ごとの子 span は量が多く役に立たないので作らない。
    FastAPIInstrumentor.instrument_app(
        app,
        tracer_provider=provider,
        excluded_urls=_EXCLUDED_URLS,
        exclude_spans=["receive", "send"],
    )


def _tracer() -> trace.Tracer:
    # 送り先が無いアプリでは、グローバルに登録済みの provider（同じプロセスで先に作られた
    # アプリのもの）へ落ちないよう、明示的に no-op にする。
    if _provider is None:
        return trace.NoOpTracer()
    return _provider.get_tracer(_TRACER_NAME)


@contextmanager
def inference_span(name: str, attributes: Mapping[str, str | int | bool]) -> Iterator[trace.Span]:
    """推論（パース・埋め込み・リランク）を囲む span。現在の span（サーバ span）の子になる。

    スレッドへ逃がす処理は、`asyncio.to_thread` / Starlette のスレッドプールが contextvars を
    引き継ぐので、その中で開いても親子がつながる。
    """
    with _tracer().start_as_current_span(
        name,
        attributes=dict(attributes),
        # 例外のメッセージ（Docling のエラー詳細や HTTPException の detail）には文書の断片が
        # 混ざり得る。既定の自動記録（exception イベント・status の description）は使わず、
        # 例外の型名だけを残す。
        record_exception=False,
        set_status_on_exception=False,
    ) as span:
        try:
            yield span
        except BaseException as exc:
            span.set_attribute("error.type", type(exc).__name__)
            span.set_status(StatusCode.ERROR)
            raise
