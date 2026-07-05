"""/parse のテスト。

- 軽量経路（text/plain・エラー系）はフェイクダウンロードで常時実行。
- Docling 実走（markdown の表構造・PDF+日本語 OCR）は重いので slow マーク
  （CI 既定は除外、`pytest -m slow` またはローカル/コンテナ検証で実行）。
"""

import os
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from ingestion_worker import parse as parse_mod

FIXTURES = Path(__file__).parent / "fixtures"


def _stub_download(monkeypatch: pytest.MonkeyPatch, data: bytes) -> None:
    async def fake_download(_url: str) -> bytes:
        return data

    monkeypatch.setattr(parse_mod, "_download", fake_download)


def test_parse_plain_text_splits_paragraphs(
    client: TestClient, monkeypatch: pytest.MonkeyPatch
) -> None:
    _stub_download(monkeypatch, "最初の段落。\n\n次の段落。".encode())
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "source_url": "http://minio:9000/blob",
            "content_type": "text/plain",
            "file_name": "memo.txt",
        },
    )
    assert resp.status_code == 200
    blocks = resp.json()["blocks"]
    assert [b["text"] for b in blocks] == ["最初の段落。", "次の段落。"]
    assert all(b["type"] == "paragraph" for b in blocks)


def test_parse_unsupported_content_type_is_structured_422(
    client: TestClient, monkeypatch: pytest.MonkeyPatch
) -> None:
    _stub_download(monkeypatch, b"binary")
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "source_url": "http://minio:9000/blob",
            "content_type": "application/zip",
            "file_name": "a.zip",
        },
    )
    assert resp.status_code == 422
    assert resp.json()["detail"]["error"] == "unsupported_content_type"


@pytest.mark.slow
def test_parse_markdown_preserves_table_structure(
    client: TestClient, monkeypatch: pytest.MonkeyPatch
) -> None:
    """表が表単位のブロックとして Markdown 構造を保って抽出される（Task 2.1 受入条件）。"""
    _stub_download(monkeypatch, (FIXTURES / "sample.md").read_bytes())
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "source_url": "http://minio:9000/blob",
            "content_type": "text/markdown",
            "file_name": "sample.md",
        },
    )
    assert resp.status_code == 200
    blocks = resp.json()["blocks"]
    types = [b["type"] for b in blocks]
    assert "heading" in types
    tables = [b for b in blocks if b["type"] == "table"]
    assert len(tables) == 1
    # 表のセル内容が Markdown 表として残る。
    assert "東京" in tables[0]["text"] and "1200" in tables[0]["text"]


@pytest.mark.slow
@pytest.mark.skipif(
    os.environ.get("WORKER_MODEL_TESTS") != "1",
    reason="PDF 変換はレイアウトモデル DL と tesseract を要するため opt-in",
)
def test_parse_pdf_smoke(client: TestClient, monkeypatch: pytest.MonkeyPatch) -> None:
    _stub_download(monkeypatch, (FIXTURES / "sample.pdf").read_bytes())
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "source_url": "http://minio:9000/blob",
            "content_type": "application/pdf",
            "file_name": "sample.pdf",
        },
    )
    assert resp.status_code == 200
    assert resp.json()["used_ocr"] is True
    assert resp.json()["blocks"]
