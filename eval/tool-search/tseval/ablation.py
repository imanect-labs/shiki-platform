"""融合のアブレーション（RRF の k・埋め込みの重み・埋め込みに渡す文書の形）。全カタログ・ja。

製品へ入れるときの既定値を決めるための補助実験（reranker は使わない）。
出力: results/ablation.json
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np

from . import bm25, worker
from .catalog import load_catalog, names_service
from .evaluate import load_queries, shuffled
from .metrics import rank_of, summarize

RESULTS = Path(__file__).resolve().parent.parent / "results"


def weighted_rrf(a: list[str], b: list[str], k: int, wb: float) -> list[str]:
    score: dict[str, float] = {}
    for i, n in enumerate(a):
        score[n] = score.get(n, 0.0) + 1.0 / (k + i + 1)
    for i, n in enumerate(b):
        score[n] = score.get(n, 0.0) + wb / (k + i + 1)
    return sorted(score, key=lambda n: (-score[n], n))


def main() -> None:
    catalog = shuffled(load_catalog())  # 定義順の同点解決で正解が有利にならないように。
    queries = load_queries()
    svc = {t.name: t.service for t in catalog}
    clean = {q["id"] for q in queries if names_service(q["text"], svc[q["target"]])}
    ranks, _ = bm25.rank([t.tooldef("ja") for t in catalog], queries, limit=5, depth=len(catalog))
    b = {q["id"]: [n for n, _ in ranks[q["id"]]["ranked"]] for q in queries}
    qv = worker.embed([q["text"] for q in queries], "query")
    names = [t.name for t in catalog]

    def emb_ranks(doc_texts: list[str]) -> dict[str, list[str]]:
        sims = qv @ worker.embed(doc_texts, "document").T
        return {
            q["id"]: [names[i] for i in np.argsort(-sims[qi], kind="stable")]
            for qi, q in enumerate(queries)
        }

    variants = {
        "name+desc+params": emb_ranks([t.doc_text("ja") for t in catalog]),
        "name+desc": emb_ranks([f"{t.name}: {t.tooldef('ja')['description']}" for t in catalog]),
        "desc": emb_ranks([t.tooldef("ja")["description"] for t in catalog]),
    }

    def measure(by_q: dict[str, list[str]]) -> dict:
        all_r = [rank_of(by_q[q["id"]], q["target"]) for q in queries]
        clean_r = [rank_of(by_q[q["id"]], q["target"]) for q in queries if q["id"] in clean]
        return {"all": summarize(all_r), "clean": summarize(clean_r)}

    rows = []
    for doc, e in variants.items():
        rows.append({"doc": doc, "method": "emb", **measure(e)})
        for k in (10, 30, 60, 100):
            for wb in (0.5, 1.0, 1.5, 2.0):
                fused = {q["id"]: weighted_rrf(b[q["id"]], e[q["id"]], k, wb) for q in queries}
                rows.append({"doc": doc, "method": "rrf", "k": k, "w_emb": wb, **measure(fused)})
    rows.append({"doc": "-", "method": "bm25", **measure(b)})
    RESULTS.mkdir(exist_ok=True)
    (RESULTS / "ablation.json").write_text(json.dumps(rows, ensure_ascii=False, indent=1))
    best = sorted((r for r in rows if r["method"] == "rrf"), key=lambda r: -r["all"]["recall@5"])[
        :5
    ]
    for r in best:
        print(
            r["doc"],
            r["k"],
            r["w_emb"],
            round(r["all"]["recall@5"] * 100, 1),
            round(r["clean"]["recall@5"] * 100, 1),
        )
    for r in rows:
        if r["method"] in ("emb", "bm25") or (r.get("k") == 60 and r.get("w_emb") == 1.0):
            print(
                r["doc"],
                r["method"],
                r.get("k"),
                round(r["all"]["recall@5"] * 100, 1),
                round(r["clean"]["recall@5"] * 100, 1),
            )


if __name__ == "__main__":
    main()
