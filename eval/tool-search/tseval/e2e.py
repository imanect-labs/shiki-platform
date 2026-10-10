"""E2E: LLM に実際にツールを選ばせ、正解ツールを呼べたかを測る。

条件:
- `all`         … 全ツールの定義を最初から提示（tool search なし・従来の挙動）
- `search_bm25` … `tool_search` だけを提示し、製品の順位（limit 5）で読み込ませる
- `search_rrf`  … 同上、順位は BM25 と埋め込みの RRF 融合の上位 5 件

読み込みは OpenAI 互換アダプタと同じ写し方（読み込んだ定義を `tools` の末尾へ追記し、
結果本文に呼び出し名を書き足す・`crates/llm-gateway/src/providers/openai.rs`）。
`tool_search` の説明は製品の定義そのもの（`EvalCatalog::definition`）。

1 会話は、モデルが `tool_search` 以外のツールを初めて呼んだ時点で打ち切る（その名前が
正解かどうかだけを見る。ツールの実行結果は要らない）。

出力: results/e2e.json
"""

from __future__ import annotations

import hashlib
import json
import random
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

from . import bm25, worker
from .catalog import Tool, fixed_target_subsets, load_catalog
from .evaluate import load_queries, rrf, shuffled
from .llm import chat

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / "data" / "cache" / "e2e"
MAX_STEPS = 6
SYSTEM = (
    "あなたは社内業務を手伝う AI アシスタントです。必要ならツールを使って依頼を実行してください。"
)


def wire(name: str) -> str:
    """OpenAI の function.name 制約へ（製品の `openai_names.rs` と同じくドットを `_` に）。"""
    return name.replace(".", "_")


def as_function(t: Tool) -> dict:
    d = t.tooldef("ja")
    return {
        "type": "function",
        "function": {
            "name": wire(d["name"]),
            "description": d["description"],
            "parameters": d["input_schema"],
        },
    }


class Searcher:
    """1 カタログぶんの検索器（方式ごとに読み込む 5 件を返す）。"""

    def __init__(self, tools: list[Tool], method: str):
        self.tools = shuffled(tools)  # 定義順の同点解決で正解が有利にならないように。
        tools = self.tools
        self.method = method
        self.defs = [t.tooldef("ja") for t in tools]
        _, self.definition = bm25.rank(self.defs, [], limit=5)
        if method == "search_rrf":
            self.doc_vecs = worker.embed([t.doc_text("ja") for t in tools], "document")

    def search(self, query: str) -> list[str]:
        ranks, _ = bm25.rank(self.defs, [{"id": "q", "text": query}], limit=5, depth=len(self.defs))
        if self.method == "search_bm25":
            return ranks["q"]["search"]
        qv = worker.embed([query], "query")[0]
        order = np.argsort(-(self.doc_vecs @ qv), kind="stable")
        emb = [self.tools[i].name for i in order]
        return rrf([n for n, _ in ranks["q"]["ranked"]], emb)[:5]


MOCK_RESULT = (
    "（評価用のモック）{name} を実行しました。結果: 該当 1 件。id=item-1024、"
    "name=該当の項目、url=https://example.invalid/item-1024"
)


def run_one(tools: list[Tool], condition: str, query: dict, searcher: Searcher | None) -> dict:
    """1 会話を回す。正解ツールを呼んだ時点で打ち切る（下調べのツールを先に呼ぶのは正しい手順
    なので、他のツールにはモックの結果を返して続けさせる）。"""
    # キーは入力内容から作る（依頼文・提示するカタログ・条件が変われば取り直す）。
    digest = hashlib.sha256(
        json.dumps(
            [condition, query["text"], sorted(t.name for t in tools)], ensure_ascii=False
        ).encode()
    ).hexdigest()[:16]
    key = f"v3-{condition}-{len(tools)}-{query['id']}-{digest}".replace("/", "_")
    path = CACHE / f"{key}.json"
    if path.exists():
        return json.loads(path.read_text())
    by_wire = {wire(t.name): t for t in tools}
    if condition == "all":
        offered = [as_function(t) for t in shuffled(tools)]
    else:
        assert searcher is not None
        offered = [
            {
                "type": "function",
                "function": {
                    "name": "tool_search",
                    "description": searcher.definition["description"],
                    "parameters": searcher.definition["parameters"],
                },
            }
        ]
    messages: list[dict] = [
        {"role": "system", "content": SYSTEM},
        {"role": "user", "content": query["text"]},
    ]
    rec = {
        "id": query["id"],
        "target": query["target"],
        "condition": condition,
        "n": len(tools),
        "calls": [],
        "searches": [],
        "prompt_tokens": 0,
        "completion_tokens": 0,
        "steps": 0,
        "error": None,
    }
    t0 = time.perf_counter()
    try:
        for _ in range(MAX_STEPS):
            r = chat(messages, tools=offered, max_tokens=4096, temperature=0.0)
            msg, usage = r["message"], r["usage"]
            rec["steps"] += 1
            rec["prompt_tokens"] += usage.get("prompt_tokens", 0)
            rec["completion_tokens"] += usage.get("completion_tokens", 0)
            calls = msg.get("tool_calls") or []
            if not calls:
                break
            messages.append(
                {"role": "assistant", "content": msg.get("content") or "", "tool_calls": calls}
            )
            for c in calls:
                name = c["function"]["name"]
                if name != "tool_search":
                    local = by_wire[name].name if name in by_wire else name
                    rec["calls"].append(local)
                    messages.append(
                        {
                            "role": "tool",
                            "tool_call_id": c["id"],
                            "content": MOCK_RESULT.format(name=name),
                        }
                    )
                    continue
                try:
                    q = json.loads(c["function"]["arguments"] or "{}").get("query", "")
                except json.JSONDecodeError:
                    q = ""
                assert searcher is not None
                hits = searcher.search(q) if q else []
                rec["searches"].append({"query": q, "hits": hits})
                known = {o["function"]["name"] for o in offered}
                offered += [
                    as_function(t) for t in tools if t.name in hits and wire(t.name) not in known
                ]
                lines = "".join(f"\n- {h}" for h in hits)
                content = (
                    f"{len(hits)} 件のツールを読み込みました。以降は通常どおり呼び出せます。{lines}"
                    f"\n（読み込んだツールの呼び出し名: {', '.join(wire(h) for h in hits)}）"
                    if hits
                    else "該当するツールは見つかりませんでした。"
                )
                messages.append({"role": "tool", "tool_call_id": c["id"], "content": content})
            if query["target"] in rec["calls"]:
                break
    except RuntimeError as e:
        rec["error"] = str(e)[:300]
    rec["latency_s"] = time.perf_counter() - t0
    rec["correct"] = query["target"] in rec["calls"]
    rec["first_correct"] = bool(rec["calls"]) and rec["calls"][0] == query["target"]
    # 利用枠超過などの失敗はキャッシュしない（再実行で取り直す）。
    if rec["error"] is None:
        CACHE.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(rec, ensure_ascii=False))
    return rec


def main(sizes: tuple[int, ...] = (200, 1000), sample: int = 60, seed: int = 3) -> None:
    catalog = load_catalog()
    queries = [q for q in load_queries() if q["kind"] in ("ja", "en")]
    rng = random.Random(seed)
    targets = sorted({q["target"] for q in queries})
    chosen = set(rng.sample(targets, min(sample // 2, len(targets))))
    qs = [q for q in queries if q["target"] in chosen]
    half = set(sorted(chosen)[: len(chosen) // 2])
    big_qs = [q for q in qs if q["target"] in half]
    subsets = fixed_target_subsets(catalog, {q["target"] for q in qs}, list(sizes))
    rows = []
    for tools in subsets.values():
        for condition in ("all", "search_bm25", "search_rrf"):
            searcher = Searcher(tools, condition) if condition != "all" else None
            # 全ツール提示は 1 会話 10 万トークン超になり、利用枠を使い切る。大きい規模では
            # **全条件で**同じ半分の正解ツール（日英とも）に絞る（条件間で同じ標本を比べる）。
            batch = big_qs if len(tools) >= 1000 else qs
            with ThreadPoolExecutor(max_workers=4) as pool:
                # 失敗（利用枠超過など）も行として残す。集計で正解率から外し、件数を数える。
                rows += list(
                    pool.map(
                        lambda q, c=condition, s=searcher, ts=tools: run_one(ts, c, q, s), batch
                    )
                )
            print(f"E2E n={len(tools)} {condition} done", flush=True)
    (ROOT / "results").mkdir(exist_ok=True)
    (ROOT / "results" / "e2e.json").write_text(json.dumps(rows, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
