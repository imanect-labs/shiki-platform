"""DTO の必須フィールド検証: tenant_id は全エンドポイントで必須（design §4.3）。"""

from fastapi.testclient import TestClient


def test_embed_requires_tenant_id(client: TestClient) -> None:
    resp = client.post("/embed", json={"input_type": "query", "texts": ["こんにちは"]})
    assert resp.status_code == 422


def test_rerank_requires_tenant_id(client: TestClient) -> None:
    resp = client.post(
        "/rerank",
        json={"query": "q", "passages": [{"id": "1", "text": "t"}]},
    )
    assert resp.status_code == 422


def test_parse_requires_tenant_id(client: TestClient) -> None:
    resp = client.post(
        "/parse",
        json={
            "source_url": "http://minio:9000/x",
            "content_type": "application/pdf",
            "file_name": "a.pdf",
        },
    )
    assert resp.status_code == 422


def test_embed_rejects_empty_texts(client: TestClient) -> None:
    resp = client.post(
        "/embed", json={"tenant_id": "a-corp", "input_type": "query", "texts": []}
    )
    assert resp.status_code == 422
