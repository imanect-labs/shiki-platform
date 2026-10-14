"""E2E: LLM に実際にツールを選ばせ、正解ツールを呼べたかを測る。

条件:
- `all`         … 全ツールの定義を最初から提示（tool search なし・従来の挙動）
- `search_bm25` … `tool_search` だけを提示し、製品の順位（limit 5）で読み込ませる
- `search_rrf`  … 同上、順位は BM25 と埋め込みの RRF 融合の上位 5 件
- `*_prod`      … 結果本文を製品の文面（`CatalogSearch::loaded_message`）にしたもの（案内なし）
- `*_hint`      … 製品の文面に、同じ名前空間の下調べ用ツールの案内を付けたもの（#522・
                  効果が無く製品には入れなかった案。文面は評価側で再現する）

`search_bm25` / `search_rrf` の結果本文は評価側で組んだ簡略版（v3 の基準・名前の一覧のみ）。

読み込みは OpenAI 互換アダプタと同じ写し方（読み込んだ定義を `tools` の末尾へ追記し、
結果本文に呼び出し名を書き足す・`crates/llm-gateway/src/providers/openai.rs`）。
`tool_search` の説明は製品の定義そのもの（`CatalogSearch::definition`）。

1 会話は、モデルが `tool_search` 以外のツールを初めて呼んだ時点で打ち切る（その名前が
正解かどうかだけを見る。ツールの実行結果は要らない）。

出力: results/e2e.json
"""

from __future__ import annotations

import functools
import hashlib
import inspect
import json
import random
import re
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

from . import bm25, worker
from .catalog import Tool, fixed_target_subsets, load_catalog
from .evaluate import load_queries, rrf, shuffled
from .llm import chat

ROOT = Path(__file__).resolve().parent.parent
CONDITIONS = (
    "all",
    "search_bm25",
    "search_rrf",
    "search_bm25_prod",
    "search_rrf_prod",
    "search_bm25_hint",
    "search_rrf_hint",
)
CACHE = ROOT / "data" / "cache" / "e2e"
MAX_STEPS = 6
SIBLING_LIMIT = 15
MAX_QUERY_CHARS = 500  # 製品の `tool_search` と同じ上限。
DEFAULT_LIMIT = 5  # 製品の既定件数と上限。
MAX_LIMIT = 10
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
        self.method = base_method(method)
        # 製品の文面の条件は、融合も製品の関数（`CatalogSearch::fused`）で行う。簡略版の条件は
        # 初回の計測どおり評価側の RRF のまま。
        self.product = method != self.method
        self.defs = [t.tooldef("ja") for t in tools]
        self._ranks: dict[tuple[str, int], dict] = {}
        _, self.definition = bm25.rank(self.defs, [], limit=5)
        if self.method == "search_rrf":
            self.doc_vecs = worker.embed([t.doc_text("ja") for t in tools], "document")

    def ranks(self, query: str, limit: int = DEFAULT_LIMIT) -> dict:
        """製品の BM25 の結果（同じクエリは使い回す・案内の並びでも使う）。"""
        if (query, limit) not in self._ranks:
            ranks, _ = bm25.rank(
                self.defs, [{"id": "q", "text": query}], limit=limit, depth=len(self.defs)
            )
            self._ranks[(query, limit)] = ranks["q"]
        return self._ranks[(query, limit)]

    def search(self, query: str, limit: int = DEFAULT_LIMIT) -> list[str]:
        # 製品は空・長すぎるクエリをエラーにする（何も読み込まない）。
        if not query.strip() or len(query.strip()) > MAX_QUERY_CHARS:
            return []
        ranks = self.ranks(query, limit)
        # `select:` の名指しは製品と同じく埋め込みを使わない（名前の完全一致だけ）。
        if self.method == "search_bm25" or query.strip().startswith("select:"):
            return ranks["search"]
        qv = worker.embed([query], "query")[0]
        sims = self.doc_vecs @ qv
        order = np.argsort(-sims, kind="stable")  # 同点は定義順（製品と同じ）。
        emb = [self.tools[i].name for i in order]
        if not self.product:
            return rrf([n for n, _ in ranks["ranked"]], emb)[:limit]
        # 製品は全文書の類似度が同じ（クエリが何も区別しない）とき BM25 だけで返す。
        if float(sims.max() - sims.min()) <= np.finfo(np.float32).eps:
            return ranks["search"]
        return bm25.fuse(self.defs, query, emb, limit)


def base_method(condition: str) -> str:
    """条件名から順位付けの方式（`search_bm25` / `search_rrf`）を取り出す。"""
    return condition.removesuffix("_prod").removesuffix("_hint")


def search_content(
    condition: str,
    searcher: Searcher,
    query: str,
    limit: int | None,
    hits: list[str],
    loaded: set[str],
) -> str:
    """`tool_search` の結果本文（OpenAI 互換アダプタと同じく呼び出し名を書き足す）。

    `loaded` はこの会話でそれまでに読み込んだツール（案内から除く）。"""
    if condition in ("search_bm25", "search_rrf"):
        lines = "".join(f"\n- {h}" for h in hits)
        return (
            f"{len(hits)} 件のツールを読み込みました。以降は通常どおり呼び出せます。{lines}"
            f"\n（読み込んだツールの呼び出し名: {', '.join(wire(h) for h in hits)}）"
            if hits
            else "該当するツールは見つかりませんでした。"
        )
    content = bm25.loaded_message(searcher.defs, query, limit, hits)
    if condition.endswith("_hint") and hits:
        content += sibling_hint(searcher, query, hits, loaded)
    if hits:
        content += f"\n（読み込んだツールの呼び出し名: {', '.join(wire(h) for h in hits)}）"
    return content


def namespace(name: str) -> str:
    """名前空間（名前の最初の区切りまで・製品の `catalog_listing` と同じ）。"""
    return re.split(r"[._]", name, maxsplit=1)[0]


def sibling_hint(searcher: Searcher, query: str, hits: list[str], loaded: set[str]) -> str:
    """製品に入れなかった案（#522）の再現: 読み込んだツールと同じ名前空間の、まだ読み込んで
    いないツールの名前を添え、下調べに使えると案内する。並びは製品の BM25 の順位 → 定義順で
    15 件まで（試作した製品実装と同じ文面・同じ並び）。今回とそれまでに読み込んだツールは除く
    （試作は今回の分しか除かず、読み込み済みのツールを案内して読み込み直しを招いた）。"""
    spaces = {namespace(h) for h in hits}
    order = [n for n, _ in searcher.ranks(query)["ranked"]] + [d["name"] for d in searcher.defs]
    out: list[str] = []
    for n in order:
        if len(out) >= SIBLING_LIMIT:
            break
        if n not in hits and n not in loaded and namespace(n) in spaces and n not in out:
            out.append(n)
    if not out:
        return ""
    return (
        f"\n同じ名前空間のまだ読み込んでいないツール: {', '.join(out)}。"
        "必要な ID や値が分からないときは、利用者に尋ねる前に、この中の一覧・検索・取得のツールを"
        " `select:<名前>` で読み込んで調べられる。"
    )


def product_limit(raw: object) -> int:
    """製品の `parse_input` と同じ limit の正規化（整数でなければ既定・1〜上限に丸める）。"""
    # serde の `as_u64` が受け付ける範囲（0 以上 2^64 未満の整数）だけを有効とする。
    if isinstance(raw, int) and not isinstance(raw, bool) and 0 <= raw < 2**64:
        return min(max(raw, 1), MAX_LIMIT)
    return DEFAULT_LIMIT


def catalog_fingerprint(tools: list[Tool]) -> str:
    """提示するカタログの定義（名前・説明・引数）の指紋。"""
    defs = sorted((t.tooldef("ja") for t in tools), key=lambda d: d["name"])
    return hashlib.sha256(json.dumps(defs, ensure_ascii=False).encode()).hexdigest()[:16]


@functools.cache
def text_fingerprint(fused: bool = False) -> str:
    """結果本文と順位を作るコードの指紋（変われば `_prod` / `_hint` のキャッシュを使わない）。"""
    repo = ROOT.parents[1]
    src = (repo / "crates/agent-core/src/tool_search/mod.rs").read_text()
    src += (repo / "crates/agent-core/examples/tool_search_rank.rs").read_text()
    src += inspect.getsource(bm25)  # CLI への橋渡し（loaded_message / fuse）。
    src += inspect.getsource(search_content) + inspect.getsource(sibling_hint)
    if fused:  # RRF の条件は融合の経路（Searcher）も含める。
        src += inspect.getsource(Searcher)
    return hashlib.sha256(src.encode()).hexdigest()[:16]


MOCK_RESULT = (
    "（評価用のモック）{name} を実行しました。結果: 該当 1 件。id=item-1024、"
    "name=該当の項目、url=https://example.invalid/item-1024"
)
MOCK_CACHE = ROOT / "data" / "cache" / "mock"
# 提示していない名前の呼び出しへの製品の観測（`crates/agent-core/src/agent_gate.rs`）。
UNKNOWN_TOOL = "unknown tool: {name}"
MOCK_PROMPT = """あなたは業務 SaaS の API のモックです。次のツールが、次の引数で呼ばれました。
利用者の依頼の文脈に沿った、もっともらしい成功時の応答を JSON で返してください。

- 依頼に出てくる名前・件名・日付・チャンネルなどに合うデータを含める（一覧・検索なら、依頼の対象が分かる 1〜3 件）。
- ID・URL・メールアドレスは架空でよいが、それらしい形にする。
- 依頼に無い情報を作るのは、応答の形として必要な範囲にとどめる。

利用者の依頼:
{request}

ツール: {name}
説明: {description}
引数: {args}

JSON だけを返してください。"""


@functools.cache
def mock_fingerprint() -> str:
    """依頼に沿ったモックを作るコードと文面の指紋。"""
    src = MOCK_PROMPT + inspect.getsource(llm_mock)
    return hashlib.sha256(src.encode()).hexdigest()[:16]


def llm_mock(tool: Tool, args: str, request: str) -> str:
    """依頼の文脈に沿ったツールの応答を LLM で作る（#545・同じ入力は使い回す）。

    一律の「該当の項目」を返すと、一覧・検索で下調べしたモデルが「目的の物か判断できない」と
    利用者に聞き返して終える。下調べの連鎖を最後まで測るために使う。
    """
    d = tool.tooldef("ja")
    prompt = MOCK_PROMPT.format(
        request=request, name=d["name"], description=d["description"], args=args or "{}"
    )
    # 生成・抽出のコードが変われば作り直す（文面だけでなく実装の指紋もキーに入れる）。
    key = hashlib.sha256((mock_fingerprint() + prompt).encode()).hexdigest()[:20]
    path = MOCK_CACHE / f"{key}.json"
    if path.exists():
        cached = path.read_text()
        try:
            json.loads(cached)
            return cached
        except json.JSONDecodeError:
            path.unlink()  # 以前の実装が残した、途中で切れた応答は作り直す。
    for max_tokens in (1024, 3072):
        out = chat([{"role": "user", "content": prompt}], max_tokens=max_tokens, temperature=0.0)
        text = (out["message"].get("content") or "").strip()
        m = re.search(r"```(?:json)?\s*(.*?)```", text, re.S)
        body = (m.group(1) if m else text).strip()
        try:
            json.loads(body)  # 途中で切れた・崩れた応答は使わない（キャッシュもしない）。
        except json.JSONDecodeError:
            continue
        MOCK_CACHE.mkdir(parents=True, exist_ok=True)
        path.write_text(body)
        return body
    # 会話ごと失敗にする（run_one が捕まえ、会話もキャッシュしない）。
    raise RuntimeError(f"モックの応答が JSON にならない: {d['name']}")


def cache_path(
    tools: list[Tool],
    condition: str,
    query: dict,
    fingerprint: str | None = None,
    variant: str | None = None,
) -> Path:
    """1 会話のキャッシュの置き場所。

    キーは入力内容から作る（依頼文・提示するカタログ・条件が変われば取り直す）。製品の文面を
    使う条件は、文面と順位を作るコード（製品の tool_search・順位 CLI・評価側の案内と橋渡し）と
    カタログの定義（説明文の変更でも本文が変わる）も入力に含める。`fingerprint` はコードの指紋を
    差し替えるとき（キャッシュの付け替え）だけ渡す。`variant` は既定と違う system prompt で
    回すときの、その system prompt の本文（#545）。
    """
    parts = [condition, query["text"], sorted(t.name for t in tools)]
    if condition not in ("all", "search_bm25", "search_rrf"):
        fp = fingerprint or text_fingerprint(base_method(condition) == "search_rrf")
        parts += [fp, catalog_fingerprint(tools)]
    if variant is not None:
        parts.append(["system", variant])
    digest = hashlib.sha256(json.dumps(parts, ensure_ascii=False).encode()).hexdigest()[:16]
    key = f"v3-{condition}-{len(tools)}-{query['id']}-{digest}".replace("/", "_")
    return CACHE / f"{key}.json"


def run_one(
    tools: list[Tool],
    condition: str,
    query: dict,
    searcher: Searcher | None,
    system: str | None = None,
    mock: str = "generic",
    max_steps: int = MAX_STEPS,
    record_args: bool = False,
) -> dict:
    """1 会話を回す。正解ツールを呼んだ時点で打ち切る（下調べのツールを先に呼ぶのは正しい手順
    なので、他のツールにはモックの結果を返して続けさせる）。

    `system` を渡すと既定（`SYSTEM`）の代わりに使う（キャッシュのキーにも入る・#545）。
    `mock="llm"` は、ツールの応答を一律の文面ではなく依頼に沿った内容にする（[`llm_mock`]）。
    `max_steps` は 1 会話の応答の回数の上限（既定以外はキャッシュのキーにも入る・#547）。
    `record_args` なら、呼んだ引数（`call_args`）を持たない古いキャッシュを使わずに取り直す。
    """
    variant = (
        system
        if mock == "generic"
        # モックを作るコードと文面が変われば取り直す。
        else json.dumps([system, mock, mock_fingerprint()], ensure_ascii=False)
    )
    if max_steps != MAX_STEPS:
        variant = json.dumps([variant, "max_steps", max_steps], ensure_ascii=False)
    path = cache_path(tools, condition, query, variant=variant)
    names = {t.name for t in tools}
    if path.exists():
        cached = json.loads(path.read_text())
        # 提示していない名前を呼んだ会話は、以前は成功のモックを返していた（製品は
        # `unknown tool` のエラー）。その会話だけ取り直す（それ以外は呼ぶまでが同じ）。
        stale_args = record_args and "call_args" not in cached
        if all(c in names for c in cached["calls"]) and not stale_args:
            return cached
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
        {"role": "system", "content": SYSTEM if system is None else system},
        {"role": "user", "content": query["text"]},
    ]
    rec = {
        "id": query["id"],
        "target": query["target"],
        "condition": condition,
        "n": len(tools),
        "calls": [],
        # 呼んだツールの引数（足りない依頼で値を作ったかを後で見る・#547。古いキャッシュには無い）。
        "call_args": [],
        "searches": [],
        "prompt_tokens": 0,
        "completion_tokens": 0,
        "steps": 0,
        "error": None,
        "final_text": "",
        # 終わり方: target（正解を呼んだ）/ reply（ツールを呼ばずに文章で終えた）/ max_steps。
        "stop": "max_steps",
    }
    t0 = time.perf_counter()
    try:
        for _ in range(max_steps):
            r = chat(messages, tools=offered, max_tokens=4096, temperature=0.0)
            msg, usage = r["message"], r["usage"]
            rec["steps"] += 1
            rec["prompt_tokens"] += usage.get("prompt_tokens", 0)
            rec["completion_tokens"] += usage.get("completion_tokens", 0)
            calls = msg.get("tool_calls") or []
            # 最後の応答の本文（聞き返しの中身を後で分類する・#545）。
            rec["final_text"] = msg.get("content") or ""  # 全文（末尾の質問を落とさない）
            if not calls:
                rec["stop"] = "reply"
                break
            messages.append(
                {"role": "assistant", "content": msg.get("content") or "", "tool_calls": calls}
            )
            for c in calls:
                name = c["function"]["name"]
                if name != "tool_search":
                    local = by_wire[name].name if name in by_wire else name
                    rec["calls"].append(local)
                    rec["call_args"].append(c["function"]["arguments"])
                    messages.append(
                        {
                            "role": "tool",
                            "tool_call_id": c["id"],
                            "content": (
                                # 製品は提示していない名前をエラーの観測にする（agent_gate::execute_tool）。
                                UNKNOWN_TOOL.format(name=name)
                                if name not in by_wire
                                else MOCK_RESULT.format(name=name)
                                if mock == "generic"
                                else llm_mock(
                                    by_wire[name], c["function"]["arguments"], query["text"]
                                )
                            ),
                        }
                    )
                    continue
                try:
                    args = json.loads(c["function"]["arguments"] or "{}")
                except json.JSONDecodeError:
                    args = {}
                q = args.get("query", "") if isinstance(args, dict) else ""
                q = q if isinstance(q, str) else ""
                # 製品の文面の条件は、モデルが渡した limit も製品と同じに扱う（簡略版は 5 件固定）。
                raw_limit = args.get("limit") if isinstance(args, dict) else None
                limit = raw_limit if condition not in ("search_bm25", "search_rrf") else None
                assert searcher is not None
                hits = searcher.search(q, product_limit(limit)) if q else []
                rec["searches"].append({"query": q, "limit": limit, "hits": hits})
                known = {o["function"]["name"] for o in offered}
                offered += [
                    as_function(t) for t in tools if t.name in hits and wire(t.name) not in known
                ]
                loaded = {by_wire[n].name for n in known if n in by_wire}
                content = search_content(condition, searcher, q, limit, hits, loaded)
                messages.append({"role": "tool", "tool_call_id": c["id"], "content": content})
            if query["target"] in rec["calls"]:
                rec["stop"] = "target"
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
        for condition in CONDITIONS:
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
