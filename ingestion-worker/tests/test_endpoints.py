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


def test_warmup_loads_both_models_and_survives_failures() -> None:
    from ingestion_worker.model_registry import ModelRegistry

    calls: list[str] = []

    class Registry(ModelRegistry):
        def embedding(self):  # type: ignore[override]
            calls.append("embed")
            return object()

        def reranker(self):  # type: ignore[override]
            calls.append("rerank")
            raise RuntimeError("boom")

    Registry().warmup()  # reranker が失敗しても例外を投げない
    assert sorted(calls) == ["embed", "rerank"]


def test_model_is_published_only_after_first_inference(monkeypatch) -> None:
    """空打ちが終わるまでモデルを公開しない（途中で来た推論が並行して走らない）。"""
    from ingestion_worker import model_registry
    from ingestion_worker.model_registry import ModelRegistry

    order: list[str] = []
    registry = ModelRegistry()

    class Reranker:
        def __init__(self, _model_id: str) -> None:
            order.append("init")

        def predict(self, pairs: list[tuple[str, str]]) -> list[float]:
            # 空打ちの最中はまだ公開されていない。
            order.append(f"predict loaded={registry.reranker_loaded}")
            return [0.0 for _ in pairs]

    monkeypatch.setattr(model_registry, "_CrossEncoderReranker", Reranker)
    registry.reranker().predict([("q", "p")])
    assert order == ["init", "predict loaded=False", "predict loaded=True"]


def test_startup_starts_warmup_in_background(monkeypatch) -> None:
    import threading

    from ingestion_worker import main, model_registry
    from ingestion_worker.model_registry import ModelRegistry

    done = threading.Event()

    class Recording(ModelRegistry):
        def warmup(self) -> None:
            done.set()

    original = model_registry.get_registry()
    model_registry.set_registry_for_tests(Recording())
    try:
        # lifespan は TestClient を with で使った時だけ走る。
        with TestClient(main.create_app()) as c:
            assert c.get("/healthz").status_code == 200
            assert done.wait(5), "起動時に warmup が始まる"
    finally:
        model_registry.set_registry_for_tests(original)
