"""検索の指標（recall@k・MRR・nDCG）とブートストラップ信頼区間。"""

from __future__ import annotations

import math
import random


def rank_of(ranked: list[str], target: str) -> int | None:
    """1 始まりの順位（無ければ None）。"""
    try:
        return ranked.index(target) + 1
    except ValueError:
        return None


def summarize(ranks: list[int | None], ks: tuple[int, ...] = (1, 3, 5, 10)) -> dict:
    """正解が 1 つの検索の指標。nDCG は正解 1 つなので 1/log2(rank+1)。"""
    n = len(ranks)
    if n == 0:
        return {"n": 0}
    out: dict = {"n": n}
    for k in ks:
        out[f"recall@{k}"] = sum(1 for r in ranks if r is not None and r <= k) / n
    out["mrr"] = sum(1 / r for r in ranks if r is not None) / n
    out["ndcg@10"] = sum(1 / math.log2(r + 1) for r in ranks if r is not None and r <= 10) / n
    return out


def bootstrap_ci(
    ranks: list[int | None], metric: str, iters: int = 1000, seed: int = 0
) -> tuple[float, float]:
    """指標の 95% 信頼区間（クエリの復元抽出）。"""
    rng = random.Random(seed)
    vals = []
    for _ in range(iters):
        sample = [ranks[rng.randrange(len(ranks))] for _ in ranks]
        vals.append(summarize(sample)[metric])
    vals.sort()
    return vals[int(0.025 * iters)], vals[int(0.975 * iters) - 1]
