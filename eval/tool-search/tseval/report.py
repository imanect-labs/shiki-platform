"""評価結果の集計表（results/summary.md）。"""

from __future__ import annotations

import json
from collections import defaultdict
from pathlib import Path

RESULTS = Path(__file__).resolve().parent.parent / "results"
METHODS = ("bm25", "bm25_prod", "emb", "rrf", "rrf_rerank")


def _load(name: str):
    p = RESULTS / f"{name}.json"
    return json.loads(p.read_text()) if p.exists() else None


def _pct(x: float | None) -> str:
    return "—" if x is None else f"{x * 100:.1f}"


def _table(rows: list[list[str]], head: list[str]) -> str:
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out += ["| " + " | ".join(r) + " |" for r in rows]
    return "\n".join(out)


def growth(name: str, title: str) -> str:
    data = _load(name)
    if not data:
        return ""
    rows = []
    for row in data:
        m = row["metrics"]
        cells = [str(row["n"]), str(row["queries"])]
        for meth in METHODS:
            cells.append(_pct(m.get(meth, {}).get("recall@5")))
        cells.append(_pct(m["bm25"]["mrr"]))
        cells.append(_pct(m.get("rrf", {}).get("mrr")))
        rows.append(cells)
    head = ["N", "クエリ", *[f"{x} R@5" for x in METHODS], "bm25 MRR", "rrf MRR"]
    return f"## {title}\n\n" + _table(rows, head) + "\n"


def lang() -> str:
    data = _load("e3_lang")
    if not data:
        return ""
    rows = []
    for row in data:
        m = row["metrics"]
        rows.append(
            [
                row["catalog_lang"],
                row["query_kind"],
                str(row["queries"]),
                *[_pct(m[x]["recall@5"]) for x in METHODS if x in m],
                _pct(m["rrf"]["mrr"]),
            ]
        )
    head = ["カタログ", "クエリ", "件数", *[f"{x} R@5" for x in METHODS], "rrf MRR"]
    return "## E3 言語 × クエリ種別（全カタログ）\n\n" + _table(rows, head) + "\n"


def subsets() -> str:
    """サービスを名指しする依頼（＝正解が一意）とそれ以外に分けた集計（全カタログ）。"""
    from .catalog import load_catalog, names_service

    path = RESULTS / "ranks_full.json"
    if not path.exists():
        return ""
    from .metrics import summarize

    svc = {t.name: t.service for t in load_catalog()}
    queries = {
        q["id"]: q for q in json.loads((RESULTS.parent / "data" / "queries.json").read_text())
    }
    groups: dict = defaultdict(list)
    for r in json.loads(path.read_text()):
        clean = names_service(queries[r["id"]]["text"], svc[r["target"]])
        for kind in (r["kind"], "全種別"):
            groups[(r["catalog_lang"], kind, clean, r["method"])].append(r["rank"])
    rows = []
    for lang in ("ja", "en"):
        for kind in ("全種別", "ja", "en", "ja_para", "agent"):
            for clean in (True, False):
                n = len(groups[(lang, kind, clean, "bm25")])
                cells = [
                    _pct(summarize(groups[(lang, kind, clean, m)]).get("recall@5"))
                    for m in ("bm25", "emb", "rrf", "rrf_rerank")
                ]
                rows.append([lang, kind, "一意" if clean else "曖昧", str(n), *cells])
    return (
        "## 正解が一意な依頼 / 曖昧な依頼（全カタログ・R@5）\n\n"
        "「一意」= 依頼文がサービスを名指ししている（または製品の実ツール）。"
        "「曖昧」は別サービスの同等ツールでも満たせる依頼を含む。\n\n"
        + _table(rows, ["カタログ", "クエリ", "区分", "件数", "bm25", "emb", "rrf", "rrf_rerank"])
        + "\n"
    )


def limit() -> str:
    data = _load("e4_limit")
    if not data:
        return ""
    by = defaultdict(dict)
    for r in data:
        by[r["k"]][r["method"]] = r
    rows = []
    for k in sorted(by):
        cells = [str(k)]
        for meth in ("bm25", "emb", "rrf", "rrf_rerank"):
            r = by[k].get(meth)
            cells.append(
                "—" if not r else f"{r['recall'] * 100:.1f}（{r['loaded_tokens_mean']:.0f} tok）"
            )
        rows.append(cells)
    return (
        "## E4 読み込み件数 k（全カタログ・recall と読み込む定義の平均トークン）\n\n"
        + _table(rows, ["k", "bm25", "emb", "rrf", "rrf_rerank"])
        + "\n"
    )


def latency() -> str:
    data = _load("e5_latency")
    if not data:
        return ""
    lat = data["latency"]
    rows = [[k, f"{v:.1f}" if isinstance(v, float) else str(v)] for k, v in lat.items()]
    growth_rows = [
        [str(r["n"]), str(r["tool_search_description_chars"])] for r in data["description_growth"]
    ]
    return (
        "## E5 レイテンシ\n\n" + _table(rows, ["項目", "値"]) + "\n\n"
        "### tool_search の説明（名前一覧）の長さ\n\n" + _table(growth_rows, ["N", "文字数"]) + "\n"
    )


def e2e(name: str = "e2e", title: str = "E2E（LLM が会話の中で正解ツールを呼べた率）") -> str:
    """E2E の集計。v1（最初のツール呼び出しで打ち切り）と v2 以降（モック結果で続ける）を読む。

    LLM 呼び出しが失敗した会話（利用枠超過など）は正解率から外し、件数を別に示す。
    """
    data = _load(name)
    if not data:
        return ""
    agg = defaultdict(list)
    for r in data:
        agg[(r["n"], r["condition"])].append(r)
    rows = []
    for (n, cond), all_rs in sorted(agg.items()):
        rs = [r for r in all_rs if not r["error"]]
        errors = len(all_rs) - len(rs)
        if not rs:
            rows.append([str(n), cond, "0", "—", "—", "—", "—", "—", str(errors)])
            continue
        v2 = "calls" in rs[0]
        ok = sum(r["correct"] for r in rs) / len(rs)
        first = sum(r["first_correct"] if v2 else r["correct"] for r in rs) / len(rs)
        none = sum(1 for r in rs if not (r["calls"] if v2 else r["called"])) / len(rs)
        tok = sum(r["prompt_tokens"] for r in rs) / len(rs)
        srch = sum(len(r["searches"]) for r in rs) / len(rs)
        rows.append(
            [
                str(n),
                cond,
                str(len(rs)),
                _pct(ok),
                _pct(first),
                _pct(none),
                f"{tok:.0f}",
                f"{srch:.2f}",
                str(errors),
            ]
        )
    return (
        f"## {title}\n\n"
        + _table(
            rows,
            [
                "N",
                "条件",
                "件数",
                "正解率",
                "最初の実ツール呼び出しで正解",
                "ツール呼び出しなし",
                "平均入力トークン",
                "検索回数",
                "失敗（除外）",
            ],
        )
        + "\n"
    )


def skills() -> str:
    data = _load("skills_e2e")
    if not data:
        return ""
    agg = defaultdict(list)
    for r in data:
        agg[(r["n"], r["condition"])].append(r)
    rows = []
    for (n, cond), all_rs in sorted(agg.items()):
        rs = [r for r in all_rs if not r["error"]]
        if not rs:
            continue
        called = [r for r in rs if r["called"]]
        ok = sum(r["correct"] for r in rs) / len(rs)
        cond_ok = (sum(r["correct"] for r in called) / len(called)) if called else None
        tok = sum(r["prompt_tokens"] for r in rs) / len(rs)
        rows.append(
            [
                str(n),
                cond,
                str(len(rs)),
                _pct(ok),
                _pct(len(called) / len(rs)),
                _pct(cond_ok),
                f"{tok:.0f}",
                str(len(all_rs) - len(rs)),
            ]
        )
    return (
        "## skill の発見方式（LLM が正解 skill を読み込めた率）\n\n"
        + _table(
            rows,
            [
                "N",
                "条件",
                "件数",
                "正解率",
                "読み込んだ率",
                "読み込んだうちの正解率",
                "平均入力トークン",
                "失敗（除外）",
            ],
        )
        + "\n"
    )


def main() -> None:
    parts = [
        "# tool_search 評価結果\n",
        growth("e1_growth", "E1 カタログ規模（サービス単位で増加）"),
        growth("e2_fixed", "E2 妨害ツールだけ増加（評価クエリ固定）"),
        lang(),
        subsets(),
        limit(),
        latency(),
        e2e(),
        skills(),
    ]
    (RESULTS / "summary.md").write_text("\n".join(p for p in parts if p))
    print((RESULTS / "summary.md").read_text())


if __name__ == "__main__":
    main()
