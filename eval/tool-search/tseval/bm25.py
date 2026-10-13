"""製品の tool_search 順位（Rust の `tool_search_rank` example を呼ぶ）。

評価が測るのは本番の索引・順位付けそのもの（`agent_core::CatalogSearch`）。Python で
BM25 を書き直さない。
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
_BIN = ROOT / "target" / "release" / "examples" / "tool_search_rank"
_built = False


def build() -> None:
    """Cargo にビルド要否を判定させる（最新なら即終わる）。古いバイナリで測らないため、
    プロセスごとに 1 回は必ず通す。"""
    global _built
    subprocess.run(
        [
            "cargo",
            "build",
            "--release",
            "-q",
            "-p",
            "shiki-agent-core",
            "--example",
            "tool_search_rank",
        ],
        cwd=ROOT,
        check=True,
    )
    _built = True


def rank(
    catalog: list[dict], queries: list[dict], limit: int = 5, depth: int = 100
) -> tuple[dict[str, dict], dict]:
    """クエリ id → {search: 本番の読み込み順位, ranked: [(name, score)]}、と `tool_search` の定義。"""
    if not _built:
        build()
    payload = json.dumps(
        {
            "catalog": catalog,
            "queries": [{"id": q["id"], "text": q["text"]} for q in queries],
            "limit": limit,
            "depth": depth,
        },
        ensure_ascii=False,
    )
    out = subprocess.run([str(_BIN)], input=payload, capture_output=True, text=True, check=True)
    lines = [json.loads(line) for line in out.stdout.splitlines()]
    tail = lines.pop()
    definition = {
        "description": tail["tool_search_description"],
        "parameters": tail["tool_search_schema"],
    }
    return {r["id"]: r for r in lines}, definition


def loaded_message(catalog: list[dict], query: str, limit: object, names: list[str]) -> str:
    """`query`（とモデルが渡した `limit`）の検索で `names` を読み込んだときの製品の
    `tool_search` 結果本文（`CatalogSearch::loaded_message`・名指しと入力の誤りは製品の処理
    そのもの）。`limit` はモデルが渡した値（製品と同じに丸める）。"""
    if not _built:
        build()
    # 製品は整数（0 以上）でない limit を既定値として扱う。CLI の型（u64）の範囲外は渡さない（製品の `as_u64` と同じ範囲）。
    if not (isinstance(limit, int) and not isinstance(limit, bool) and 0 <= limit < 2**64):
        limit = None
    payload = json.dumps(
        {
            "catalog": catalog,
            "queries": [],
            "loads": [{"id": "l", "query": query, "limit": limit, "names": names}],
        },
        ensure_ascii=False,
    )
    out = subprocess.run([str(_BIN)], input=payload, capture_output=True, text=True, check=True)
    return json.loads(out.stdout.splitlines()[0])["content"]


def fuse(catalog: list[dict], query: str, embedding: list[str], limit: int) -> list[str]:
    """埋め込みの順位（名前・類似度の降順）を渡し、製品の融合（`CatalogSearch::fused`・BM25 との
    RRF・`+語` の絞り込み・名前の完全一致を先頭）で `limit` 件を返す。"""
    if not _built:
        build()
    payload = json.dumps(
        {
            "catalog": catalog,
            "queries": [],
            "fuses": [{"id": "f", "query": query, "embedding": embedding, "limit": limit}],
        },
        ensure_ascii=False,
    )
    out = subprocess.run([str(_BIN)], input=payload, capture_output=True, text=True, check=True)
    return json.loads(out.stdout.splitlines()[0])["names"]
