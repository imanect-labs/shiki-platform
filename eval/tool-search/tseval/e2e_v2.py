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

from . import ask_probe, e2e
from .gen_e2e_queries import OUT as QUERIES
from .gen_e2e_queries import TODAY_NOTE
from .llm import chat_json

ROOT = Path(__file__).resolve().parent.parent
JUDGE_CACHE = ROOT / "data" / "cache" / "missing_judge"
SYSTEM = e2e.SYSTEM + TODAY_NOTE
MOCK = "llm"
MAX_STEPS = 10
# (条件, カタログの規模)。1,000 件の依頼は v1 と同じく正解ツールの半分（ask_probe.sample の big）。
RUNS = (("all", 200), ("search_rrf_prod", 200), ("all", 1000), ("search_rrf_prod", 1000))

MISSING_JUDGE = """AI アシスタントが利用者の依頼に対し、ツールを実行せずに応答を終えました。
依頼には、実行に必要な「{arg}」（{desc}）が書かれていません。
アシスタントの最後の応答は、その値を利用者に尋ねていますか（ほかの情報も一緒に尋ねていてもよい）。

依頼:
{query}

最後の応答:
{final}

JSON だけを返してください: {{"asked": true または false, "reason": "短く"}}"""


def load() -> tuple[list[dict], dict[int, list]]:
    """v2 の依頼と、v1 と同じカタログの部分集合。"""
    queries = json.loads(QUERIES.read_text())
    _, _, subsets = ask_probe.sample()
    return queries, subsets


def run() -> list[dict]:
    queries, subsets = load()
    rows: list[dict] = []
    for condition, n in RUNS:
        tools = subsets[n]
        searcher = e2e.Searcher(tools, condition) if condition != "all" else None
        batch = [q for q in queries if q["n"] == n]
        with ThreadPoolExecutor(max_workers=4) as pool:
            out = list(
                pool.map(
                    lambda q, ts=tools, c=condition, se=searcher: e2e.run_one(
                        ts, c, q, se, system=SYSTEM, mock=MOCK, max_steps=MAX_STEPS
                    ),
                    batch,
                )
            )
        kinds = {q["id"]: q for q in batch}
        rows += [
            {**r, "kind": kinds[r["id"]]["kind"], "missing": kinds[r["id"]]["missing"]} for r in out
        ]
        errors = sum(1 for r in out if r["error"])
        print(f"E2E v2 n={n} {condition} done（失敗 {errors} 件）", flush=True)
    ask_probe.write_results("e2e_v2.json", rows)
    return rows


def asked_missing(row: dict, query: dict, desc: str) -> dict:
    """足りない値を尋ねて終えたか（同じ入力の判定は使い回す）。"""
    prompt = MISSING_JUDGE.format(
        arg=query["missing"],
        desc=desc,
        query=query["text"],
        final=ask_probe.clip_middle(row["final_text"]) or "（本文なし）",
    )
    key = hashlib.sha256((inspect.getsource(asked_missing) + prompt).encode()).hexdigest()[:20]
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


def judge(rows: list[dict]) -> list[dict]:
    queries, subsets = load()
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
    ask_probe.write_results("e2e_v2_judge.json", out)
    return out


def summarize(rows: list[dict], verdicts: list[dict]) -> list[dict]:
    """条件ごとの集計。十分な依頼は終わり方の内訳、足りない依頼は聞き返せた率。"""
    v = {(x["id"], x["condition"], x["n"]): x for x in verdicts}
    out = []
    for condition, n in RUNS:
        mine = [r for r in rows if r["condition"] == condition and r["n"] == n and not r["error"]]
        full = [r for r in mine if r["missing"] is None]
        short = [r for r in mine if r["missing"] is not None]

        def cat(r: dict) -> str:
            if r["correct"]:
                return "correct"
            if r["stop"] != "reply":
                return "max_steps"
            return v.get((r["id"], r["condition"], r["n"]), {}).get("category", "error")

        counts = {
            k: sum(1 for r in full if cat(r) == k)
            for k in ("correct", "avoidable", "legit", "other", "max_steps", "error")
        }
        asked = sum(
            1
            for r in short
            if not r["correct"]
            and r["stop"] == "reply"
            and v.get((r["id"], condition, n), {}).get("asked") is True
        )
        out.append(
            {
                "condition": condition,
                "n": n,
                "full": len(full),
                **counts,
                "missing": len(short),
                "missing_asked": asked,
                "missing_called_target": sum(1 for r in short if r["correct"]),
                "errors": sum(
                    1 for r in rows if r["condition"] == condition and r["n"] == n and r["error"]
                ),
                "mean_prompt_tokens": round(
                    sum(r["prompt_tokens"] for r in full) / max(1, len(full))
                ),
            }
        )
    (ROOT / "results" / "e2e_v2_summary.json").write_text(json.dumps(out, indent=1))
    return out


def main() -> None:
    rows = run()
    verdicts = judge(rows)
    for s in summarize(rows, verdicts):
        f = s["full"] or 1
        print(
            f"n={s['n']} {s['condition']}: 正解 {s['correct']}/{s['full']}"
            f"（{100 * s['correct'] / f:.1f}%）avoidable {s['avoidable']} legit {s['legit']}"
            f" other {s['other']} 打ち切り {s['max_steps']} 判定失敗 {s['error']}"
            f" / 足りない依頼で聞き返せた {s['missing_asked']}/{s['missing']}"
            f"（正解ツールを呼んだ {s['missing_called_target']}）/ 会話の失敗 {s['errors']}"
        )


if __name__ == "__main__":
    main()
