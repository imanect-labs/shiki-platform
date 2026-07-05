"""/healthz /embed /rerank のフェイクモデル経路テスト（モデル DL 不要・CI 常時実行）。"""

from fastapi.testclient import TestClient


def test_healthz_reports_model_state(client: TestClient) -> None:
    resp = client.get("/healthz")
    assert resp.status_code == 200
    body = resp.json()
    assert body["status"] == "ok"
    assert "embed" in body["models"] and "rerank" in body["models"]


def test_embed_returns_normalized_vectors_and_version(client: TestClient) -> None:
    resp = client.post(
        "/embed",
        json={
            "tenant_id": "a-corp",
            "input_type": "document",
            "texts": ["春の売上報告", "夏の売上報告"],
        },
    )
    assert resp.status_code == 200
    body = resp.json()
    assert len(body["vectors"]) == 2
    assert body["dimension"] == len(body["vectors"][0])
    assert body["model_version"]  # Rust 側の version 突合ガードの根拠
    # 正規化済み（フェイクも本物も cosine 用に L2 正規化して返す契約）。
    norm = sum(v * v for v in body["vectors"][0]) ** 0.5
    assert abs(norm - 1.0) < 1e-6


def test_embed_query_and_document_prefixes_differ(client: TestClient) -> None:
    """同一テキストでも query/document でプレフィックスが違うためベクトルが変わる。"""
    q = client.post(
        "/embed",
        json={"tenant_id": "a", "input_type": "query", "texts": ["売上"]},
    ).json()
    d = client.post(
        "/embed",
        json={"tenant_id": "a", "input_type": "document", "texts": ["売上"]},
    ).json()
    assert q["vectors"][0] != d["vectors"][0]


def test_rerank_returns_score_per_passage(client: TestClient) -> None:
    resp = client.post(
        "/rerank",
        json={
            "tenant_id": "a-corp",
            "query": "経費精算の締め切り",
            "passages": [
                {"id": "c1", "text": "経費精算の締め切りは毎月25日です"},
                {"id": "c2", "text": "本日の天気は晴れです"},
            ],
        },
    )
    assert resp.status_code == 200
    body = resp.json()
    assert [s["id"] for s in body["scores"]] == ["c1", "c2"]
    # 関連パッセージのスコアが高い（フェイクは共通文字数ベースだが同傾向）。
    assert body["scores"][0]["score"] > body["scores"][1]["score"]
    assert body["model_version"]
