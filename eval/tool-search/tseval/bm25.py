"""製品の tool_search 順位（Rust の `tool_search_rank` example を呼ぶ）。

評価が測るのは本番の索引・順位付けそのもの（`agent_core::EvalCatalog`）。Python で
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
