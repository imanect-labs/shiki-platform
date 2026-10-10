"""検索精度の評価（規模・融合・言語・件数・レイテンシ）。

方式:
- `bm25`        … 製品の BM25F の打ち切り前の順位（`EvalCatalog::ranked`）
- `bm25_prod`   … 製品が実際に読み込む順位（limit 5・相対カットオフ込み・`EvalCatalog::search`）
- `emb`         … Ruri v3 の埋め込みのコサイン類似度
- `rrf`         … bm25 と emb の Reciprocal Rank Fusion（k=60・`crates/rag/src/fusion.rs` と同じ定数）
- `rrf_rerank`  … rrf の上位 20 件を cross-encoder で並べ替え

実験:
- E1 growth   … サービス単位でカタログが増えるときの劣化（catalog.growth_order）
- E2 fixed    … 評価クエリを固定し、妨害ツールだけを増やしたときの劣化
- E3 lang     … カタログ言語（ja/en）× クエリ種別（ja/en/ja_para/agent）
- E4 limit    … 読み込み件数 k と recall・読み込む定義のトークンの関係
- E5 latency  … 1 クエリあたりの処理時間（BM25 / 埋め込み / rerank）

出力: results/<実験>.json
"""

from __future__ import annotations

import json
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

from . import bm25, worker
from .catalog import Tool, fixed_target_subsets, growth_order, load_catalog
from .metrics import bootstrap_ci, rank_of, summarize

ROOT = Path(__file__).resolve().parent.parent
RESULTS = ROOT / "results"
RRF_K = 60
RERANK_DEPTH = 20
METHODS = ("bm25", "bm25_prod", "emb", "rrf", "rrf_rerank")


def load_queries() -> list[dict]:
    return json.loads((ROOT / "data" / "queries.json").read_text())


def rrf(*rankings: list[str], k: int = RRF_K) -> list[str]:
    score: dict[str, float] = {}
    for ranking in rankings:
        for i, name in enumerate(ranking):
            score[name] = score.get(name, 0.0) + 1.0 / (k + i + 1)
    return sorted(score, key=lambda n: (-score[n], n))


def run_methods(
    tools: list[Tool], queries: list[dict], lang: str = "ja", rerank: bool = False
) -> dict[str, dict[str, list[str]]]:
    """方式 → クエリ id → 順位（ツール名の列）。

    reranker（cross-encoder）は CPU で 1 クエリ数秒かかるため、`rerank=True` の時だけ回す
    （全カタログの評価に限る）。
    """
    methods = METHODS if rerank else tuple(m for m in METHODS if m != "rrf_rerank")
    out: dict[str, dict[str, list[str]]] = {m: {} for m in methods}
    defs = [t.tooldef(lang) for t in tools]
    ranks, _ = bm25.rank(defs, queries, limit=5, depth=100)
    for q in queries:
        out.setdefault("bm25", {})[q["id"]] = [n for n, _ in ranks[q["id"]]["ranked"]]
        out.setdefault("bm25_prod", {})[q["id"]] = ranks[q["id"]]["search"]
    if not any(m in methods for m in ("emb", "rrf", "rrf_rerank")):
        return out
    names = [t.name for t in tools]
    docs = worker.embed([t.doc_text(lang) for t in tools], "document")
    qv = worker.embed([q["text"] for q in queries], "query")
    sims = qv @ docs.T
    by_name = {t.name: t for t in tools}
    for qi, q in enumerate(queries):
        order = np.argsort(-sims[qi], kind="stable")[:100]
        emb = [names[i] for i in order]
        out["emb"][q["id"]] = emb
        out["rrf"][q["id"]] = rrf(out["bm25"][q["id"]], emb)
    if rerank:

        def one(q: dict) -> tuple[str, list[str]]:
            fused = out["rrf"][q["id"]]
            head = fused[:RERANK_DEPTH]
            # 引数まで入れると cross-encoder が遅く、順位もほぼ変わらない。名前と説明だけ渡す。
            passages = [(n, f"{n}: {by_name[n].tooldef(lang)['description']}") for n in head]
            scores = worker.rerank(q["text"], passages)
            return q["id"], sorted(head, key=lambda n: -scores[n]) + fused[RERANK_DEPTH:]

        with ThreadPoolExecutor(max_workers=3) as pool:
            for qid, ranked in pool.map(one, queries):
                out["rrf_rerank"][qid] = ranked
    return out


def score(rankings: dict[str, dict[str, list[str]]], queries: list[dict]) -> dict:
    res = {}
    for m, by_q in rankings.items():
        rs = [rank_of(by_q[q["id"]], q["target"]) for q in queries]
        s = summarize(rs)
        s["recall@5_ci"] = bootstrap_ci(rs, "recall@5")
        s["mrr_ci"] = bootstrap_ci(rs, "mrr")
        res[m] = s
    return res


def e1_growth(catalog: list[Tool], queries: list[dict], sizes: list[int]) -> list[dict]:
    order = growth_order(catalog)
    rows = []
    for n in sizes:
        tools = order[:n]
        present = {t.name for t in tools}
        qs = [q for q in queries if q["target"] in present]
        full = len(tools) == len(catalog)
        rows.append(
            {
                "n": len(tools),
                "queries": len(qs),
                "metrics": score(run_methods(tools, qs, rerank=full), qs),
            }
        )
        print(f"E1 n={len(tools)} q={len(qs)}", flush=True)
    return rows


def e2_fixed(catalog: list[Tool], queries: list[dict], sizes: list[int]) -> list[dict]:
    targets = {q["target"] for q in queries}
    rows = []
    for tools in fixed_target_subsets(catalog, targets, sizes).values():
        full = len(tools) == len(catalog)
        rows.append(
            {
                "n": len(tools),
                "queries": len(queries),
                "metrics": score(run_methods(tools, queries, rerank=full), queries),
            }
        )
        print(f"E2 n={len(tools)}", flush=True)
    return rows


def e3_lang(catalog: list[Tool], queries: list[dict]) -> list[dict]:
    rows = []
    per_query = []
    for lang in ("ja", "en"):
        r = run_methods(catalog, queries, lang=lang, rerank=True)
        for m, by_q in r.items():
            for q in queries:
                per_query.append(
                    {
                        "catalog_lang": lang,
                        "method": m,
                        "id": q["id"],
                        "kind": q["kind"],
                        "target": q["target"],
                        "rank": rank_of(by_q[q["id"]], q["target"]),
                    }
                )
        for kind in ("ja", "en", "ja_para", "agent"):
            qs = [q for q in queries if q["kind"] == kind]
            rows.append(
                {
                    "catalog_lang": lang,
                    "query_kind": kind,
                    "queries": len(qs),
                    "metrics": score({m: r[m] for m in r}, qs),
                }
            )
        print(f"E3 lang={lang}", flush=True)
    # クエリ単位の順位（部分集合での再集計・失敗分析に使う）。
    (RESULTS / "ranks_full.json").write_text(json.dumps(per_query, ensure_ascii=False))
    return rows


def e4_limit(catalog: list[Tool], queries: list[dict]) -> list[dict]:
    r = run_methods(catalog, queries, rerank=True)
    tokens = {
        t.name: len(json.dumps(t.tooldef("ja"), ensure_ascii=False).encode()) // 4 for t in catalog
    }
    rows = []
    for m in ("bm25", "emb", "rrf", "rrf_rerank"):
        for k in range(1, 11):
            rs = [rank_of(r[m][q["id"]], q["target"]) for q in queries]
            loaded = [sum(tokens[n] for n in r[m][q["id"]][:k]) for q in queries]
            rows.append(
                {
                    "method": m,
                    "k": k,
                    "recall": sum(1 for x in rs if x is not None and x <= k) / len(rs),
                    "loaded_tokens_mean": float(np.mean(loaded)),
                }
            )
    return rows


def e5_latency(catalog: list[Tool], queries: list[dict], sample: int = 50) -> dict:
    qs = queries[:sample]
    defs = [t.tooldef("ja") for t in catalog]
    t0 = time.perf_counter()
    _, definition = bm25.rank(defs, qs, limit=5, depth=100)
    desc_chars = len(definition["description"])
    bm25_ms = (time.perf_counter() - t0) * 1000 / len(qs)
    # 埋め込みはキャッシュを避けて 1 件ずつ往復を測る（クエリ時の実コスト）。
    t0 = time.perf_counter()
    for q in qs[:20]:
        worker.embed([q["text"] + f" #{time.time_ns()}"], "query")
    emb_ms = (time.perf_counter() - t0) * 1000 / 20
    head = [(t.name, t.doc_text("ja")) for t in catalog[:RERANK_DEPTH]]
    t0 = time.perf_counter()
    for q in qs[:10]:
        worker.rerank(q["text"], head)
    rerank_ms = (time.perf_counter() - t0) * 1000 / 10
    return {
        "catalog": len(catalog),
        "bm25_ms_per_query_incl_index_build_amortized": bm25_ms,
        "embed_query_ms": emb_ms,
        "rerank20_ms": rerank_ms,
        "tool_search_description_chars": desc_chars,
    }


def e5_description_growth(catalog: list[Tool], sizes: list[int]) -> list[dict]:
    order = growth_order(catalog)
    rows = []
    for n in sizes:
        _, definition = bm25.rank([t.tooldef("ja") for t in order[:n]], [], limit=5)
        rows.append(
            {
                "n": min(n, len(order)),
                "tool_search_description_chars": len(definition["description"]),
            }
        )
    return rows


def main() -> None:
    RESULTS.mkdir(exist_ok=True)
    catalog = load_catalog()
    queries = load_queries()
    sizes = [16, 32, 64, 128, 256, 512, len(catalog)]
    print(f"catalog={len(catalog)} queries={len(queries)}", flush=True)
    only = set(sys.argv[1:])
    for name, fn in (
        ("e1_growth", lambda: e1_growth(catalog, queries, sizes)),
        (
            "e2_fixed",
            lambda: e2_fixed(
                catalog, queries, [len({q["target"] for q in queries}), 256, 512, len(catalog)]
            ),
        ),
        ("e3_lang", lambda: e3_lang(catalog, queries)),
        ("e4_limit", lambda: e4_limit(catalog, queries)),
        (
            "e5_latency",
            lambda: {
                "latency": e5_latency(catalog, queries),
                "description_growth": e5_description_growth(catalog, sizes),
            },
        ),
    ):
        if only and name not in only:
            continue
        (RESULTS / f"{name}.json").write_text(json.dumps(fn(), ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
