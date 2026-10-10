"""レポートのデータを results/ から組み立て、template.html に埋めて tool-search-eval.html を作る。

eval/tool-search で `PYTHONPATH=. uv run python report/build.py` として実行する。
"""

import collections
import glob
import json
from pathlib import Path

from tseval.catalog import load_catalog, names_service
from tseval.metrics import summarize

HERE = Path(__file__).resolve().parent
R = Path("results")
M = ("bm25", "emb", "rrf", "rrf_rerank")


def load(name):
    for p in (R / f"{name}.json", R / "superseded" / f"{name}.json"):
        if p.exists():
            return json.loads(p.read_text())
    return None


out = {}
out["e1"] = [
    {
        "n": r["n"],
        "q": r["queries"],
        **{m: r["metrics"].get(m, {}).get("recall@5") for m in M},
        "bm25_prod": r["metrics"]["bm25_prod"]["recall@5"],
        **{m + "_mrr": r["metrics"].get(m, {}).get("mrr") for m in M},
    }
    for r in load("e1_growth")
]
out["e2"] = [
    {"n": r["n"], "q": r["queries"], **{m: r["metrics"].get(m, {}).get("recall@5") for m in M}}
    for r in load("e2_fixed")
]
out["e3"] = [
    {
        "lang": r["catalog_lang"],
        "kind": r["query_kind"],
        "q": r["queries"],
        **{m: r["metrics"][m]["recall@5"] for m in M},
    }
    for r in load("e3_lang")
]
out["e4"] = load("e4_limit")
out["e5"] = load("e5_latency")

svc = {t.name: t.service for t in load_catalog()}
qs = {q["id"]: q for q in json.loads(Path("data/queries.json").read_text())}
g = collections.defaultdict(list)
for r in load("ranks_full"):
    clean = names_service(qs[r["id"]]["text"], svc[r["target"]])
    for kind in (r["kind"], "all"):
        g[(r["catalog_lang"], kind, clean, r["method"])].append(r["rank"])
out["subset"] = [
    {
        "lang": lang,
        "kind": k,
        "clean": c,
        "q": len(g[(lang, k, c, "bm25")]),
        **{m: summarize(g[(lang, k, c, m)])["recall@5"] for m in M},
    }
    for lang in ("ja", "en")
    for k in ("all", "ja", "en", "ja_para", "agent")
    for c in (True, False)
]
out["ablation"] = [
    {
        "lang": r.get("lang", "ja"),
        "doc": r["doc"],
        "method": r["method"],
        "k": r.get("k"),
        "w": r.get("w_emb"),
        "all": r["all"]["recall@5"],
        "clean": r["clean"]["recall@5"],
    }
    for r in load("ablation")
]


def agg_e2e(rows):
    a = collections.defaultdict(list)
    for r in rows:
        if not r.get("error"):
            a[(r["n"], r["condition"])].append(r)
    res = []
    for (n, c), rs in sorted(a.items()):
        v2 = "calls" in rs[0]
        k = len(rs)
        res.append(
            {
                "n": n,
                "cond": c,
                "k": k,
                "ok": sum(r["correct"] for r in rs) / k,
                "first": sum((r["first_correct"] if v2 else r["correct"]) for r in rs) / k,
                "none": sum(1 for r in rs if not (r["calls"] if v2 else r["called"])) / k,
                "tok": sum(r["prompt_tokens"] for r in rs) / k,
                "search": sum(len(r["searches"]) for r in rs) / k,
            }
        )
    return res


out["e2e_v1"] = agg_e2e(load("e2e_v1_first_call") or [])
v2 = load("e2e") or [json.loads(Path(p).read_text()) for p in glob.glob("data/cache/e2e/v3-*.json")]
out["e2e_v2"] = agg_e2e(v2)
sk = load("skills_e2e") or []
a = collections.defaultdict(list)
for r in sk:
    if not r.get("error"):
        a[(r["n"], r["condition"])].append(r)
out["skills"] = [
    {
        "n": n,
        "cond": c,
        "k": len(rs),
        "ok": sum(r["correct"] for r in rs) / len(rs),
        "invoked": sum(1 for r in rs if r["called"]) / len(rs),
        "tok": sum(r["prompt_tokens"] for r in rs) / len(rs),
    }
    for (n, c), rs in sorted(a.items())
]
out["skills_final"] = (R / "skills_e2e.json").exists()

t = (HERE / "template.html").read_text()
(HERE / "tool-search-eval.html").write_text(
    t.replace("__DATA__", json.dumps(out, ensure_ascii=False))
)
print("built", {k: (len(v) if isinstance(v, list) else v) for k, v in out.items() if k != "e5"})
