"""OpenTelemetry の初期化（トレース）。

- shiki-server（`crates/api/src/telemetry.rs`）と同じく、`OTEL_EXPORTER_OTLP_ENDPOINT` が
  未設定・空ならエクスポートしない。collector が居なくても起動を妨げない（警告ログのみ）。
- 受信した `traceparent`（W3C TraceContext・OTel の既定 propagator）を継続する。Rust 側の
  呼び出し span の下に、worker のサーバ span と推論 span がつながる。
- `/healthz`（compose の healthcheck が数秒おきに叩く）はトレースから外す。
- 推論の span（`inference_span`）に載せるのは件数・ページ数・モデル名・バッチサイズ等の
  数値と識別子だけ。文書の中身・クエリ・ファイル名は載せない。
- 例外のメッセージ（Docling のエラー詳細・HTTPException の detail 等）には文書の断片が
  混ざり得る。送る前に `RedactingSpanProcessor` で全 span から取り除き、型名だけを残す。
"""

import logging
import os
from collections.abc import Iterator, Mapping
from contextlib import contextmanager
from contextvars import ContextVar

from fastapi import FastAPI
from opentelemetry import trace
from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import Event, ReadableSpan, SpanProcessor, TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.trace import Status, StatusCode
from starlette.types import ASGIApp, Receive, Scope, Send

logger = logging.getLogger(__name__)

SERVICE_NAME = "ingestion-worker"
# FastAPIInstrumentor の excluded_urls は正規表現の部分一致（カンマ区切り）。
_EXCLUDED_URLS = r"/healthz$"
_TRACER_NAME = "ingestion_worker"

# 処理中のリクエストを受けたアプリの tracer。アプリの外（リクエスト外）では no-op。
# モジュール変数にしないのは、同じプロセスで別のアプリを作っても既存アプリの送り先が
# 変わらないようにするため。
_NOOP_TRACER = trace.NoOpTracer()
_current_tracer: ContextVar[trace.Tracer] = ContextVar("worker_tracer", default=_NOOP_TRACER)


def otlp_endpoint() -> str | None:
    """エクスポート先。空文字列は「無効」扱い（compose の `${OTLP_ENDPOINT:-}` を許す）。"""
    endpoint = os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT", "").strip()
    return endpoint or None


def _redact(span: ReadableSpan) -> ReadableSpan:
    """例外イベントのメッセージ・スタックと、status の description を落とした写しを返す。"""
    attributes = dict(span.attributes or {})
    events: list[Event] = []
    for event in span.events:
        if event.name != "exception":
            events.append(event)
            continue
        exc_type = (event.attributes or {}).get("exception.type")
        if exc_type is not None:
            attributes.setdefault("error.type", str(exc_type))
    status = span.status
    if status.description:
        status = Status(status.status_code)
    return ReadableSpan(
        name=span.name,
        context=span.context,
        parent=span.parent,
        resource=span.resource,
        attributes=attributes,
        events=events,
        links=span.links,
        kind=span.kind,
        status=status,
        start_time=span.start_time,
        end_time=span.end_time,
        instrumentation_scope=span.instrumentation_scope,
    )


class RedactingSpanProcessor(SpanProcessor):
    """終わった span から例外のメッセージを除いてから、下流（エクスポート）へ渡す。

    FastAPI 自動計装のサーバ span は、未処理例外を既定で `record_exception` する（メッセージと
    スタックトレース入り）。計装側では止められないので、送る直前のここで全 span をそろえる。
    """

    def __init__(self, inner: SpanProcessor) -> None:
        self._inner = inner

    def on_start(self, span, parent_context=None) -> None:
        self._inner.on_start(span, parent_context=parent_context)

    def on_end(self, span: ReadableSpan) -> None:
        self._inner.on_end(_redact(span))

    def shutdown(self) -> None:
        self._inner.shutdown()

    def force_flush(self, timeout_millis: int = 30000) -> bool:
        return self._inner.force_flush(timeout_millis)


def _build_provider() -> TracerProvider | None:
    if otlp_endpoint() is None:
        logger.warning("OTEL_EXPORTER_OTLP_ENDPOINT 未設定: OpenTelemetry エクスポートは無効")
        return None
    # 送り先・プロトコル以外の設定（ヘッダ・タイムアウト等）は OTEL_* 環境変数で exporter が読む。
    from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter

    service_name = os.environ.get("OTEL_SERVICE_NAME", "").strip() or SERVICE_NAME
    provider = TracerProvider(resource=Resource.create({"service.name": service_name}))
    # BatchSpanProcessor はバックグラウンドで送る。collector 不在でも推論は止まらない。
    provider.add_span_processor(RedactingSpanProcessor(BatchSpanProcessor(OTLPSpanExporter())))
    # グローバルには登録しない。同じプロセスの別の計装（ライブラリの自動計装など）が
    # worker の送り先を勝手に使わないよう、provider はこのアプリの計装にだけ渡す。
    return provider


class _BindTracer:
    """リクエストの間だけ、このアプリの tracer を `_current_tracer` に入れる（素の ASGI）。

    contextvars はリクエストのタスクと、そこから逃がすスレッド（Starlette のスレッドプール・
    `asyncio.to_thread`）に引き継がれる。
    """

    def __init__(self, app: ASGIApp, tracer: trace.Tracer) -> None:
        self.app = app
        self.tracer = tracer

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        token = _current_tracer.set(self.tracer)
        try:
            await self.app(scope, receive, send)
        finally:
            _current_tracer.reset(token)


def setup_tracing(app: FastAPI, tracer_provider: trace.TracerProvider | None = None) -> None:
    """アプリにトレースを仕掛ける。`tracer_provider` はテスト用（未指定なら環境変数から作る）。

    送り先が無いアプリの推論 span は no-op（グローバルに登録済みの provider へ落ちない）。
    """
    provider = tracer_provider if tracer_provider is not None else _build_provider()
    tracer = provider.get_tracer(_TRACER_NAME) if provider is not None else _NOOP_TRACER
    app.add_middleware(_BindTracer, tracer=tracer)
    if provider is None:
        return
    # ASGI の receive/send ごとの子 span は量が多く役に立たないので作らない。
    FastAPIInstrumentor.instrument_app(
        app,
        tracer_provider=provider,
        excluded_urls=_EXCLUDED_URLS,
        exclude_spans=["receive", "send"],
    )


@contextmanager
def inference_span(name: str, attributes: Mapping[str, str | int | bool]) -> Iterator[trace.Span]:
    """推論（パース・埋め込み・リランク）を囲む span。現在の span（サーバ span）の子になる。

    スレッドへ逃がす処理は、`asyncio.to_thread` / Starlette のスレッドプールが contextvars を
    引き継ぐので、その中で開いても親子がつながる。
    """
    with _current_tracer.get().start_as_current_span(
        name,
        attributes=dict(attributes),
        # 例外のメッセージは載せない（文書の断片が混ざり得る）。型名と ERROR status だけ残す。
        record_exception=False,
        set_status_on_exception=False,
    ) as span:
        try:
            yield span
        except BaseException as exc:
            span.set_attribute("error.type", type(exc).__name__)
            span.set_status(StatusCode.ERROR)
            raise
