"""OTel 計装: 受信 traceparent の継続・推論 span・/healthz の除外・未設定時の無効化。"""

import base64
from types import SimpleNamespace

import pytest
from conftest import TEST_WORKER_TOKEN, FakeRegistry
from fastapi.testclient import TestClient
from opentelemetry.sdk.trace import ReadableSpan, TracerProvider
from opentelemetry.sdk.trace.export import SimpleSpanProcessor
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.trace import SpanKind, StatusCode

from ingestion_worker import model_registry, telemetry
from ingestion_worker import parse as parse_mod
from ingestion_worker.main import create_app
from ingestion_worker.schemas import BlockType, ParsedBlock

# Rust 側（shiki-server）が inject する想定の W3C traceparent。
TRACE_ID = "4bf92f3577b34da6a3ce929d0e0e4736"
PARENT_SPAN_ID = "00f067aa0ba902b7"
TRACEPARENT = f"00-{TRACE_ID}-{PARENT_SPAN_ID}-01"


@pytest.fixture
def traced(worker_env: None):
    exporter = InMemorySpanExporter()
    provider = TracerProvider()
    provider.add_span_processor(SimpleSpanProcessor(exporter))
    original = model_registry.get_registry()
    model_registry.set_registry_for_tests(FakeRegistry())
    client = TestClient(
        create_app(tracer_provider=provider),
        headers={"Authorization": f"Bearer {TEST_WORKER_TOKEN}", "traceparent": TRACEPARENT},
    )
    try:
        yield client, exporter
    finally:
        model_registry.set_registry_for_tests(original)
        provider.shutdown()


def _server_span(spans: tuple[ReadableSpan, ...]) -> ReadableSpan:
    [server] = [s for s in spans if s.kind == SpanKind.SERVER]
    return server


def _span(spans: tuple[ReadableSpan, ...], name: str) -> ReadableSpan:
    [span] = [s for s in spans if s.name == name]
    return span


def _assert_continues_caller(spans: tuple[ReadableSpan, ...], inference: ReadableSpan) -> None:
    """サーバ span が受信 traceparent の子で、推論 span がサーバ span の子であること。"""
    server = _server_span(spans)
    assert format(server.context.trace_id, "032x") == TRACE_ID
    assert server.parent is not None
    assert format(server.parent.span_id, "016x") == PARENT_SPAN_ID
    assert server.parent.is_remote
    assert inference.context.trace_id == server.context.trace_id
    assert inference.parent is not None
    assert inference.parent.span_id == server.context.span_id
    # ASGI の receive/send の子 span は作らない（サーバ span と推論 span だけ）。
    assert {s.name for s in spans} == {server.name, inference.name}


def _all_attribute_values(spans: tuple[ReadableSpan, ...]) -> str:
    return " ".join(str(v) for s in spans for v in (s.attributes or {}).values())


def test_embed_span_continues_traceparent_with_counts(traced) -> None:
    client, exporter = traced
    resp = client.post(
        "/embed",
        json={"tenant_id": "a-corp", "input_type": "document", "texts": ["春の売上報告", "夏"]},
    )
    assert resp.status_code == 200
    spans = exporter.get_finished_spans()
    span = _span(spans, "worker.embed.encode")
    _assert_continues_caller(spans, span)
    attrs = dict(span.attributes or {})
    assert attrs["worker.model"] == "cl-nagoya/ruri-v3-30m"
    assert attrs["worker.texts"] == 2
    assert attrs["worker.batch_size"] == 16
    assert attrs["worker.input_type"] == "document"
    assert attrs["worker.dimension"] == 8
    # 文書の中身は span に載せない。
    assert "売上" not in _all_attribute_values(spans)


def test_rerank_span_continues_traceparent_without_query_text(traced) -> None:
    client, exporter = traced
    resp = client.post(
        "/rerank",
        json={
            "tenant_id": "a-corp",
            "query": "経費精算の締め切り",
            "passages": [{"id": "c1", "text": "毎月25日です"}, {"id": "c2", "text": "晴れ"}],
        },
    )
    assert resp.status_code == 200
    spans = exporter.get_finished_spans()
    span = _span(spans, "worker.rerank.predict")
    _assert_continues_caller(spans, span)
    attrs = dict(span.attributes or {})
    assert attrs["worker.passages"] == 2
    assert attrs["worker.model"] == "hotchpotch/japanese-reranker-cross-encoder-xsmall-v1"
    values = _all_attribute_values(spans)
    assert "経費精算" not in values
    assert "25日" not in values


def test_parse_span_records_size_and_blocks_not_content(traced) -> None:
    client, exporter = traced
    payload = base64.b64encode("本文の段落。\n\n次の段落。".encode()).decode()
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "content_base64": payload,
            "content_type": "text/plain",
            "file_name": "秘密の議事録.txt",
        },
    )
    assert resp.status_code == 200
    spans = exporter.get_finished_spans()
    span = _span(spans, "worker.parse")
    _assert_continues_caller(spans, span)
    attrs = dict(span.attributes or {})
    assert attrs["worker.content_type"] == "text/plain"
    assert attrs["worker.blocks"] == 2
    assert attrs["worker.bytes"] == len("本文の段落。\n\n次の段落。".encode())
    values = _all_attribute_values(spans)
    assert "本文" not in values
    assert "秘密の議事録" not in values


def test_docling_parse_span_records_page_count(traced, monkeypatch: pytest.MonkeyPatch) -> None:
    """Docling 経路はスレッドへ逃がしても親子がつながり、文書のページ数が載る。"""
    from docling.datamodel.base_models import ConversionStatus

    class FakeConverter:
        def convert(self, *_args, **_kwargs):
            document = SimpleNamespace(pages={1: object(), 2: object(), 3: object()})
            return SimpleNamespace(status=ConversionStatus.SUCCESS, document=document, errors=[])

    holder = SimpleNamespace(get=lambda: FakeConverter())
    monkeypatch.setattr(parse_mod, "get_converter_holder", lambda: holder)
    monkeypatch.setattr(
        parse_mod,
        "_docling_blocks",
        lambda _doc: [ParsedBlock(type=BlockType.PARAGRAPH, text="段落", page=1)],
    )
    client, exporter = traced
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "content_base64": base64.b64encode(b"%PDF-1.7").decode(),
            "content_type": "application/pdf",
            "file_name": "a.pdf",
        },
    )
    assert resp.status_code == 200
    spans = exporter.get_finished_spans()
    span = _span(spans, "worker.parse")
    _assert_continues_caller(spans, span)
    attrs = dict(span.attributes or {})
    assert attrs["worker.pages"] == 3
    assert attrs["worker.blocks"] == 1
    assert attrs["worker.used_ocr"] is True


def test_healthz_is_not_traced(traced) -> None:
    client, exporter = traced
    assert client.get("/healthz").status_code == 200
    assert exporter.get_finished_spans() == ()


def test_tracing_disabled_without_endpoint(monkeypatch: pytest.MonkeyPatch, worker_env) -> None:
    """OTEL_EXPORTER_OTLP_ENDPOINT が未設定・空なら provider を作らず、起動も推論も妨げない。"""
    built: list[object] = []
    monkeypatch.setattr(telemetry, "TracerProvider", lambda **kw: built.append(kw))
    for value in (None, "", "  "):
        if value is None:
            monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
        else:
            monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", value)
        assert telemetry.otlp_endpoint() is None
        app = create_app()
        client = TestClient(app, headers={"Authorization": f"Bearer {TEST_WORKER_TOKEN}"})
        original = model_registry.get_registry()
        model_registry.set_registry_for_tests(FakeRegistry())
        try:
            resp = client.post(
                "/embed", json={"tenant_id": "a-corp", "input_type": "query", "texts": ["x"]}
            )
        finally:
            model_registry.set_registry_for_tests(original)
        assert resp.status_code == 200
    assert built == []


def test_endpoint_builds_otlp_exporter(monkeypatch: pytest.MonkeyPatch) -> None:
    """送り先があれば OTLP exporter 付きの provider を作り、service.name を付ける。"""
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otel-collector:4317")
    monkeypatch.delenv("OTEL_SERVICE_NAME", raising=False)
    registered: list[TracerProvider] = []
    monkeypatch.setattr(telemetry.trace, "set_tracer_provider", registered.append)
    provider = telemetry._build_provider()
    try:
        assert provider is not None
        assert registered == [provider]
        assert provider.resource.attributes["service.name"] == "ingestion-worker"
    finally:
        assert provider is not None
        provider.shutdown()


def test_parse_failure_records_only_error_type_not_message(
    traced, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Docling のエラー詳細（文書の断片が混ざり得る）は span のイベント・status・属性に載せない。"""
    secret = "社外秘の本文断片"

    class FailingConverter:
        def convert(self, *_args, **_kwargs):
            raise RuntimeError(f"broken page: {secret}")

    holder = SimpleNamespace(get=lambda: FailingConverter())
    monkeypatch.setattr(parse_mod, "get_converter_holder", lambda: holder)
    client, exporter = traced
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "content_base64": base64.b64encode(b"%PDF-1.7").decode(),
            "content_type": "application/pdf",
            "file_name": "a.pdf",
        },
    )
    assert resp.status_code == 422
    spans = exporter.get_finished_spans()
    span = _span(spans, "worker.parse")
    assert span.status.status_code == StatusCode.ERROR
    assert dict(span.attributes or {})["error.type"] == "HTTPException"
    for s in spans:
        assert secret not in str(s.status.description or "")
        assert secret not in _all_attribute_values((s,))
        for event in s.events:
            assert secret not in " ".join(str(v) for v in (event.attributes or {}).values())


def test_app_without_endpoint_ignores_previously_registered_global_provider(
    monkeypatch: pytest.MonkeyPatch, worker_env
) -> None:
    """送り先なしで作り直したアプリの推論 span は、登録済みのグローバル provider へ流れない。"""
    exporter = InMemorySpanExporter()
    global_provider = TracerProvider()
    global_provider.add_span_processor(SimpleSpanProcessor(exporter))
    monkeypatch.setattr(telemetry.trace, "get_tracer_provider", lambda: global_provider)
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
    client = TestClient(
        create_app(),
        headers={"Authorization": f"Bearer {TEST_WORKER_TOKEN}", "traceparent": TRACEPARENT},
    )
    original = model_registry.get_registry()
    model_registry.set_registry_for_tests(FakeRegistry())
    try:
        resp = client.post(
            "/embed", json={"tenant_id": "a-corp", "input_type": "query", "texts": ["x"]}
        )
    finally:
        model_registry.set_registry_for_tests(original)
        global_provider.shutdown()
    assert resp.status_code == 200
    assert exporter.get_finished_spans() == ()
