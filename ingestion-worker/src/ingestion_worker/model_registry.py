"""モデルの遅延ロード・シングルトン。

初回リクエスト（またはバックグラウンドの warmup）で HF Hub からダウンロードし
（`HF_HOME` の volume にキャッシュ）、以降はプロセス内で使い回す。テストは
`set_registry_for_tests` で全体をフェイクに差し替えられる。
"""

from __future__ import annotations

import threading
from typing import Protocol

from .settings import get_settings


class EmbeddingModel(Protocol):
    def encode(self, texts: list[str]) -> list[list[float]]: ...


class RerankModel(Protocol):
    def predict(self, pairs: list[tuple[str, str]]) -> list[float]: ...


class _SentenceTransformerEmbedding:
    def __init__(self, model_id: str) -> None:
        # import は遅延させる（テストでフェイク注入時に torch を要求しない）。
        from sentence_transformers import SentenceTransformer

        self._model = SentenceTransformer(model_id, device="cpu")

    def encode(self, texts: list[str]) -> list[list[float]]:
        # cosine 類似で使うため正規化して返す（Qdrant 側 distance=Cosine と対）。
        vectors = self._model.encode(
            texts,
            batch_size=get_settings().embed_batch_size,
            normalize_embeddings=True,
            convert_to_numpy=True,
        )
        return vectors.tolist()


class _CrossEncoderReranker:
    def __init__(self, model_id: str) -> None:
        from sentence_transformers import CrossEncoder

        self._model = CrossEncoder(model_id, device="cpu", max_length=512)

    def predict(self, pairs: list[tuple[str, str]]) -> list[float]:
        # 順位付けに使うため活性化関数は問わない（単調変換は順序を変えない）。
        return [float(s) for s in self._model.predict(pairs)]


class ModelRegistry:
    """スレッドセーフな遅延ロード。ロード状態は /healthz で可視化する。"""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._embedding: EmbeddingModel | None = None
        self._reranker: RerankModel | None = None

    @property
    def embedding_loaded(self) -> bool:
        return self._embedding is not None

    @property
    def reranker_loaded(self) -> bool:
        return self._reranker is not None

    def embedding(self) -> EmbeddingModel:
        if self._embedding is None:
            with self._lock:
                if self._embedding is None:
                    self._embedding = _SentenceTransformerEmbedding(get_settings().embed_model)
        return self._embedding

    def reranker(self) -> RerankModel:
        if self._reranker is None:
            with self._lock:
                if self._reranker is None:
                    self._reranker = _CrossEncoderReranker(get_settings().rerank_model)
        return self._reranker


_registry = ModelRegistry()


def get_registry() -> ModelRegistry:
    return _registry


def set_registry_for_tests(registry: ModelRegistry) -> None:
    """テスト専用: レジストリ全体を差し替える。"""
    global _registry  # noqa: PLW0603
    _registry = registry
