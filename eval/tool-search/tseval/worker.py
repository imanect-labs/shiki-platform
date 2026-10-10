"""ingestion-worker（Ruri 埋め込み・cross-encoder reranker）のクライアント。

製品と同じエンドポイント・同じモデルを叩く（`crates/rag/src/embedding.rs` と同じ契約）。
既定はプレビュー環境（LAN 内・認証なし）。結果はモデル版ごとにディスクへキャッシュする
（同じ文書を規模違いで何度も埋め込まない）。
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

import httpx
import numpy as np

BASE_URL = os.environ.get("TSEVAL_WORKER_URL", "http://192.168.1.201:8000")
CACHE = Path(__file__).resolve().parent.parent / "data" / "cache" / "embed"
_BATCH = 64


def _key(model: str, kind: str, text: str) -> str:
    return hashlib.sha256(f"{model}\0{kind}\0{text}".encode()).hexdigest()


def models() -> dict:
    return httpx.get(f"{BASE_URL}/healthz", timeout=30).json()["models"]


def embed(texts: list[str], kind: str) -> np.ndarray:
    """L2 正規化済みベクトル（行 = texts の順）。kind は query / document。"""
    model = models()["embed"]["id"]
    CACHE.mkdir(parents=True, exist_ok=True)
    store = CACHE / f"{model.replace('/', '__')}.jsonl"
    cached: dict[str, list[float]] = {}
    if store.exists():
        for line in store.read_text().splitlines():
            k, v = json.loads(line)
            cached[k] = v
    missing = [t for t in dict.fromkeys(texts) if _key(model, kind, t) not in cached]
    with store.open("a") as f:
        for i in range(0, len(missing), _BATCH):
            chunk = missing[i : i + _BATCH]
            r = httpx.post(
                f"{BASE_URL}/embed",
                json={"tenant_id": "tseval", "input_type": kind, "texts": chunk},
                timeout=600,
            )
            r.raise_for_status()
            body = r.json()
            if body["model_version"] != model or len(body["vectors"]) != len(chunk):
                raise RuntimeError("埋め込みの応答がリクエストと合わない")
            for t, v in zip(chunk, body["vectors"], strict=True):
                k = _key(model, kind, t)
                cached[k] = v
                f.write(json.dumps([k, v]) + "\n")
    return np.array([cached[_key(model, kind, t)] for t in texts], dtype=np.float32)


def rerank(query: str, passages: list[tuple[str, str]]) -> dict[str, float]:
    """cross-encoder のスコア（id → score）。passages は (id, text)。結果はディスクにキャッシュする。"""
    model = models()["rerank"]["id"]
    key = hashlib.sha256(
        json.dumps([model, query, passages], ensure_ascii=False).encode()
    ).hexdigest()
    path = CACHE.parent / "rerank" / f"{key}.json"
    if path.exists():
        return json.loads(path.read_text())
    r = httpx.post(
        f"{BASE_URL}/rerank",
        json={
            "tenant_id": "tseval",
            "query": query,
            "passages": [{"id": i, "text": t} for i, t in passages],
        },
        timeout=600,
    )
    r.raise_for_status()
    out = {s["id"]: s["score"] for s in r.json()["scores"]}
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out))
    return out
