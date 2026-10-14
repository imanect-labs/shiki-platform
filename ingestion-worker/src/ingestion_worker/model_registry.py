"""モデルの遅延ロード・シングルトン。

初回リクエスト（またはバックグラウンドの warmup）で HF Hub からダウンロードし
（`HF_HOME` の volume にキャッシュ）、以降はプロセス内で使い回す。テストは
`set_registry_for_tests` で全体をフェイクに差し替えられる。
"""

from __future__ import annotations

import logging
import threading
import time
from typing import Protocol

from .settings import get_settings

logger = logging.getLogger(__name__)


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
    """スレッドセーフな遅延ロード。ロード状態は /healthz で可視化する。

    モデルは**作ってから 1 回空打ちしたうえで**公開する（最初の推論だけ遅い分を、公開前に払う）。
    ロックはモデルごとに分ける（埋め込みのダウンロード中に reranker を待たせない）。
    読み込み中に来た推論は、そのモデルのロックで読み込みと空打ちの終わりを待つ。
    """

    def __init__(self) -> None:
        self._embed_lock = threading.Lock()
        self._rerank_lock = threading.Lock()
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
            with self._embed_lock:
                if self._embedding is None:
                    model = _SentenceTransformerEmbedding(get_settings().embed_model)
                    model.encode(["warmup"])
                    self._embedding = model
        return self._embedding

    def reranker(self) -> RerankModel:
        if self._reranker is None:
            with self._rerank_lock:
                if self._reranker is None:
                    model = _CrossEncoderReranker(get_settings().rerank_model)
                    model.predict([("warmup", "warmup")])
                    self._reranker = model
        return self._reranker

    def warmup(self) -> None:
        """両モデルを並行して読み込む（起動時にバックグラウンドで呼ぶ・`warmup_models`）。

        失敗しても落とさない（その場合は従来どおり最初のリクエストで読み込む）。
        """
        threads = [
            threading.Thread(target=self._warm, args=(name, load), daemon=True)
            for name, load in (("embed", self.embedding), ("rerank", self.reranker))
        ]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

    @staticmethod
    def _warm(name: str, load: object) -> None:
        started = time.monotonic()
        try:
            load()  # type: ignore[operator]
        except Exception:  # noqa: BLE001 — warmup の失敗で worker を落とさない
            logger.warning("warmup %s failed", name, exc_info=True)
            return
        logger.info("warmup %s done in %.1fs", name, time.monotonic() - started)


_registry = ModelRegistry()


def get_registry() -> ModelRegistry:
    return _registry


def set_registry_for_tests(registry: ModelRegistry) -> None:
    """テスト専用: レジストリ全体を差し替える。"""
    global _registry  # noqa: PLW0603
    _registry = registry
