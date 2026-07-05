"""共通フィクスチャ: モデルレジストリをフェイクへ差し替えた TestClient。"""

import hashlib

import pytest
from fastapi.testclient import TestClient

from ingestion_worker import model_registry
from ingestion_worker.main import create_app
from ingestion_worker.model_registry import ModelRegistry


class FakeEmbedding:
    """決定的フェイク（文字 n-gram ハッシュ）。モデル DL なしで API 経路を検証する。"""

    DIM = 8

    def encode(self, texts: list[str]) -> list[list[float]]:
        vectors = []
        for text in texts:
            digest = hashlib.sha256(text.encode()).digest()
            vec = [b / 255.0 for b in digest[: self.DIM]]
            norm = sum(v * v for v in vec) ** 0.5 or 1.0
            vectors.append([v / norm for v in vec])
        return vectors


class FakeReranker:
    def predict(self, pairs: list[tuple[str, str]]) -> list[float]:
        # クエリとパッセージの共通文字数を雑なスコアにする（決定的）。
        return [float(len(set(q) & set(p))) for q, p in pairs]


class FakeRegistry(ModelRegistry):
    def __init__(self) -> None:
        super().__init__()
        self._embedding = FakeEmbedding()
        self._reranker = FakeReranker()


@pytest.fixture
def client() -> TestClient:
    original = model_registry.get_registry()
    model_registry.set_registry_for_tests(FakeRegistry())
    try:
        yield TestClient(create_app())
    finally:
        model_registry.set_registry_for_tests(original)
