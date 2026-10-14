"""E2E v2（#547）: 必要な値が揃う依頼（`data/e2e_queries_v2.json`）で、全ツール提示と tool_search を比べる。

v1（`e2e.py`）との違い（v1 の結果とは比べられない）:
- 依頼は `gen_e2e_queries` で作り直したもの（正解ツールとカタログは v1 と同じ・依頼は規模ごと）。
- system prompt の末尾に固定の「今日」を入れる（製品の `today_note` と同じ文面・#546）。
- ツールの応答は依頼に沿ったモック（`e2e.llm_mock`）。下調べの連鎖を最後まで進めるため。
- 1 会話の応答の上限は `MAX_STEPS` 回（検索 → 一覧 → 検索 → 取得 → 実行 のような連鎖が入るため）。
- 情報がわざと足りない依頼（`*_missing`）は、正解ツールを呼ばずに足りない値を尋ねたら正解。

判定:
- 十分な依頼で、正解ツールを呼ばずに文章で終えた会話は #545 と同じ分類（`ask_probe.judge_one`・
  avoidable / legit / other）に通す。
- 足りない依頼で文章で終えた会話は、足りない値を尋ねているかを LLM で判定する（[`asked_missing`]）。

出力: results/e2e_v2.json（会話）・results/e2e_v2_judge.json（分類）・results/e2e_v2_summary.json（集計）
"""

from __future__ import annotations

import hashlib
import inspect
import json
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from . import ask_probe, e2e, llm
from .gen_e2e_queries import LOOKUP_OUT as LOOKUP_QUERIES
from .gen_e2e_queries import OUT as QUERIES
from .gen_e2e_queries import TODAY_NOTE
from .llm import chat_json

ROOT = Path(__file__).resolve().parent.parent
JUDGE_CACHE = ROOT / "data" / "cache" / "missing_judge"
SYSTEM = e2e.SYSTEM + TODAY_NOTE
MOCK = "llm"
MAX_STEPS = 10
# (条件, カタログの規模)。1,000 件の依頼は v1 と同じく正解ツールの半分（ask_probe.sample の big）。
# `search_rrf_comp` は、読み込むツールの ID 引数を調べるツールも一緒に読み込む案（#565）。
RUNS = (
    ("all", 200),
    ("search_rrf_prod", 200),
    ("search_rrf_comp", 200),
    ("all", 1000),
    ("search_rrf_prod", 1000),
    ("search_rrf_comp", 1000),
)

# 下調べが要る依頼だけの評価セット（#565・1,000 件のカタログ）。全ツール提示は回さず、
# tool_search の現行と、ID を調べるツールも一緒に読み込む案を比べる。
LOOKUP_RUNS = (("search_rrf_prod", 1000), ("search_rrf_comp", 1000))
DATASETS = {
    "v2": {"queries": QUERIES, "runs": RUNS, "prefix": "e2e_v2"},
    "lookup": {"queries": LOOKUP_QUERIES, "runs": LOOKUP_RUNS, "prefix": "e2e_lookup"},
}

MISSING_JUDGE = """AI アシスタントが利用者の依頼に対し、ツールを実行せずに応答を終えました。
依頼には、実行に必要な「{arg}」（{desc}）が書かれていません。
アシスタントの最後の応答は、その値を利用者に尋ねていますか（ほかの情報も一緒に尋ねていてもよい）。

依頼:
{query}

最後の応答:
{final}

JSON だけを返してください: {{"asked": true または false, "reason": "短く"}}"""


def load(ds: str = "v2") -> tuple[list[dict], dict[int, list]]:
    """評価セットの依頼と、v1 と同じカタログの部分集合。"""
    queries = json.loads(DATASETS[ds]["queries"].read_text())
    _, _, subsets = ask_probe.sample()
    return queries, subsets


def run(ds: str = "v2") -> list[dict]:
    queries, subsets = load(ds)
    rows: list[dict] = []
    for condition, n in DATASETS[ds]["runs"]:
        tools = subsets[n]
        searcher = e2e.Searcher(tools, condition) if condition != "all" else None
        batch = [q for q in queries if q["n"] == n]
        with ThreadPoolExecutor(max_workers=4) as pool:
            out = list(
                pool.map(
                    lambda q, ts=tools, c=condition, se=searcher: e2e.run_one(
                        ts,
                        c,
                        q,
                        se,
                        system=SYSTEM,
                        mock=MOCK,
                        max_steps=MAX_STEPS,
                        # 足りない依頼は、正解ツールに足りない値を渡したかを見る。
                        record_args=q["missing"] is not None,
                    ),
                    batch,
                )
            )
        kinds = {q["id"]: q for q in batch}
        rows += [
            {**r, "kind": kinds[r["id"]]["kind"], "missing": kinds[r["id"]]["missing"]} for r in out
        ]
        errors = sum(1 for r in out if r["error"])
        print(f"E2E {ds} n={n} {condition} done（失敗 {errors} 件）", flush=True)
    ask_probe.write_results(f"{DATASETS[ds]['prefix']}.json", rows)
    return rows


def asked_missing(row: dict, query: dict, desc: str) -> dict:
    """足りない値を尋ねて終えたか（同じ入力の判定は使い回す）。"""
    prompt = MISSING_JUDGE.format(
        arg=query["missing"],
        desc=desc,
        query=query["text"],
        final=ask_probe.clip_middle(row["final_text"]) or "（本文なし）",
    )
    src = llm.MODEL + inspect.getsource(asked_missing)
    key = hashlib.sha256((src + prompt).encode()).hexdigest()[:20]
    path = JUDGE_CACHE / f"{key}.json"
    if path.exists():
        return json.loads(path.read_text())
    try:
        out = chat_json(prompt, temperature=0.0)
    except RuntimeError as e:  # 失敗は記録して続ける（キャッシュしない）。
        return {"asked": None, "reason": str(e)[:200]}
    if not isinstance(out, dict) or not isinstance(out.get("asked"), bool):
        return {"asked": None, "reason": f"形式が不正: {out!r}"[:200]}
    JUDGE_CACHE.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out, ensure_ascii=False))
    return out


def judge(rows: list[dict], ds: str = "v2") -> list[dict]:
    queries, subsets = load(ds)
    by_id = {q["id"]: q for q in queries}
    catalogs = {n: {t.name: t.tooldef("ja") for t in tools} for n, tools in subsets.items()}
    replies = [
        {**r, "query_text": by_id[r["id"]]["text"]}
        for r in rows
        if not r["error"] and r["stop"] == "reply"
    ]

    def one(r: dict) -> dict:
        q = by_id[r["id"]]
        if q["missing"] is None:
            return ask_probe.judge_one(r, catalogs[r["n"]])
        target = catalogs[r["n"]][r["target"]]
        desc = target["input_schema"]["properties"][q["missing"]].get("description", "")
        return asked_missing(r, q, desc)

    with ThreadPoolExecutor(max_workers=4) as pool:
        verdicts = list(pool.map(one, replies))
    out = [
        {
            "id": r["id"],
            "condition": r["condition"],
            "n": r["n"],
            "calls": r["calls"],
            "final_text": r["final_text"],
            **v,
        }
        for r, v in zip(replies, verdicts, strict=True)
    ]
    ask_probe.write_results(f"{DATASETS[ds]['prefix']}_judge.json", out)
    return out


def target_args(r: dict) -> dict | None:
    """正解ツールに渡した引数（記録が無い・読めないときは None）。"""
    args = r.get("call_args")
    if not args or r["target"] not in r["calls"]:
        return None
    try:
        out = json.loads(args[r["calls"].index(r["target"])] or "{}")
    except json.JSONDecodeError:
        return None
    return out if isinstance(out, dict) else None


def summarize(rows: list[dict], verdicts: list[dict], ds: str = "v2") -> list[dict]:
    """条件ごとの集計。十分な依頼は終わり方の内訳（目視の分類 `e2e_v2_manual.json` があれば
    LLM の判定より優先する）、足りない依頼は聞き返せたか・正解ツールに何を渡したか。"""
    v = {(x["id"], x["condition"], x["n"]): x for x in verdicts}
    prefix = DATASETS[ds]["prefix"]
    manual_path = ROOT / "results" / f"{prefix}_manual.json"
    manual = json.loads(manual_path.read_text()) if manual_path.exists() else {}
    out = []
    for condition, n in DATASETS[ds]["runs"]:
        mine = [r for r in rows if r["condition"] == condition and r["n"] == n and not r["error"]]
        full = [r for r in mine if r["missing"] is None]
        short = [r for r in mine if r["missing"] is not None]
        hand = {k: c for k, (c, _) in manual.get(f"{condition}@{n}", {}).items()}

        def cat(r: dict, hand: dict = hand) -> str:
            if r["correct"]:
                return "correct"
            if r["stop"] != "reply":
                return "max_steps"
            if r["id"] in hand:
                return hand[r["id"]]
            # 判定の失敗・未実行は未判定（内訳を足して全件にする）。
            got = v.get((r["id"], r["condition"], r["n"]), {}).get("category")
            return got if got in ("avoidable", "legit", "other") else "unjudged"

        counts = {
            k: sum(1 for r in full if cat(r) == k)
            for k in ("correct", "avoidable", "legit", "other", "max_steps", "unjudged")
        }
        called = [r for r in short if r["correct"]]
        passed = [target_args(r) for r in called]
        out.append(
            {
                "condition": condition,
                "n": n,
                "full": len(full),
                **counts,
                # 正解ツールの前に下調べのツールを呼んで正解した会話。
                "chain": sum(1 for r in full if r["correct"] and r["calls"][:-1]),
                "missing": len(short),
                "missing_asked": sum(
                    1
                    for r in short
                    if not r["correct"]
                    and r["stop"] == "reply"
                    and v.get((r["id"], r["condition"], r["n"]), {}).get("asked") is True
                ),
                # 尋ねずに正解ツールを呼んだ会話の内訳: 足りない値を渡した／渡さなかった／記録なし。
                "missing_called_target": len(called),
                "missing_filled": sum(
                    1
                    for r, a in zip(called, passed, strict=True)
                    if a is not None and a.get(r["missing"]) not in (None, "", [], {})
                ),
                "missing_omitted": sum(
                    1
                    for r, a in zip(called, passed, strict=True)
                    if a is not None and a.get(r["missing"]) in (None, "", [], {})
                ),
                # そのうち、正解ツールの前に別サービスのツールを呼んだ（調べた可能性がある）。
                "missing_other_service": sum(
                    1
                    for r in called
                    if any(
                        e2e.namespace(c) != e2e.namespace(r["target"])
                        for c in r["calls"][: r["calls"].index(r["target"])]
                    )
                ),
                "errors": sum(
                    1 for r in rows if r["condition"] == condition and r["n"] == n and r["error"]
                ),
                "mean_prompt_tokens": round(
                    sum(r["prompt_tokens"] for r in full) / max(1, len(full))
                ),
            }
        )
    (ROOT / "results" / f"{prefix}_summary.json").write_text(json.dumps(out, indent=1))
    return out


def main(ds: str = "v2") -> None:
    rows = run(ds)
    verdicts = judge(rows, ds)
    for s in summarize(rows, verdicts, ds):
        f = s["full"] or 1
        print(
            f"n={s['n']} {s['condition']}: 正解 {s['correct']}/{s['full']}"
            f"（{100 * s['correct'] / f:.1f}%）avoidable {s['avoidable']} legit {s['legit']}"
            f" other {s['other']} 打ち切り {s['max_steps']} 未判定 {s['unjudged']}"
            f" / 足りない依頼で聞き返せた {s['missing_asked']}/{s['missing']}"
            f"（正解ツールを呼んだ {s['missing_called_target']}: 値を渡した"
            f" {s['missing_filled']}・渡さなかった {s['missing_omitted']}）/ 会話の失敗 {s['errors']}"
        )


if __name__ == "__main__":
    import sys

    main("lookup" if "--lookup" in sys.argv else "v2")
