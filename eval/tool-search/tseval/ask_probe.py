"""E2E で「ツールを呼ばずに聞き返して終える」会話の原因を切り分ける（#545）。

1. system prompt を 3 通りにして、全ツール提示と tool_search（RRF・製品の文面）を比べる。
   - `minimal` … E2E の既定（1 行の汎用文）
   - `product` … 製品の通常チャットの system prompt（`crates/chat/src/worker/config.rs` から読む）
   - `lookup`  … 既定に「利用者に尋ねる前にツールで調べる」指示を足したもの
   さらに `minimal` と `lookup` は、ツールの応答を依頼に沿った内容（`e2e.llm_mock`）にしても回す。
2. 正解ツールを呼ばずに文章で終えた会話（一覧・検索のツールを呼んでから終えたものも含む・上限の
   ステップで打ち切られたものは除く）の最後の応答を、LLM で次の 3 つに分類する。判定には、その会話で
   提示されたカタログの同じサービスのツールと、その会話で呼んだツールを見せる（抜き取りで目視も確かめる）。
   - `avoidable` … 一覧・検索・取得のツールで調べられる情報（ID など）を利用者に聞いている
   - `legit`     … ツールでは得られない情報（ファイルの中身・本人にしか分からない値など）を聞いている
   - `other`     … 聞き返しではない（拒否・説明だけ・別の作業の提案など）

出力: results/e2e_ask_probe.json（会話）・results/e2e_ask_judge.json（分類）
"""

from __future__ import annotations

import hashlib
import inspect
import json
import random
import re
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from . import e2e
from .catalog import fixed_target_subsets, load_catalog
from .evaluate import load_queries
from .llm import chat_json

ROOT = Path(__file__).resolve().parent.parent
JUDGE_CACHE = ROOT / "data" / "cache" / "ask_judge"

LOOKUP_DIRECTIVE = (
    "依頼の実行に必要な ID や値が分からないときは、利用者に尋ねる前に、一覧・検索・取得の"
    "ツールで調べてください。ツールでは得られない情報（ファイルの中身や、本人にしか分からない"
    "値など）だけを利用者に尋ねてください。"
)

# (条件, カタログの規模)。1,000 件の全ツール提示は 1 会話 20 万トークンになるため外す。
RUNS = (("all", 200), ("search_rrf_prod", 200), ("search_rrf_prod", 1000))
# 依頼に沿ったモックで取り直す system prompt（下調べの連鎖が最後まで進むかを見る）。
LLM_MOCK_PROMPTS = ("minimal", "lookup")
# 1,000 件の全ツール提示は、tool_search と比べる基準として現行の system prompt だけ回す
# （1 会話 20 万トークン）。
EXTRA_RUNS = (("all", 1000, "minimal", "generic"),)


# モデルが本文に書いた「秘密情報らしき」例（架空でも）をリポジトリに残さない。GitHub の
# push 保護は Slack の Incoming Webhook の形をした例（T00000000/B00000000/XXXX…）でも止める。
SECRET_LIKE = (
    re.compile(r"https://hooks\.slack\.com/services/[A-Za-z0-9/_-]+"),
    re.compile(r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
    re.compile(r"\bsk-[A-Za-z0-9_-]{16,}"),
    re.compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
)


def redact(text: str) -> str:
    """秘密情報の形をした文字列を伏せる（結果ファイルへ書くときだけ。判定の入力は変えない）。"""
    for pattern in SECRET_LIKE:
        text = pattern.sub("［伏せ字］", text)
    return text


def redact_all(value: object) -> object:
    """入れ子の値の文字列をすべて伏せ字に通す（本文・検索語・判定理由など）。"""
    if isinstance(value, str):
        return redact(value)
    if isinstance(value, list):
        return [redact_all(v) for v in value]
    if isinstance(value, dict):
        return {k: redact_all(v) for k, v in value.items()}
    return value


def write_results(name: str, rows: list[dict]) -> None:
    """結果を書く。モデルや判定器が書いた文字列はすべて伏せ字を通す（判定の入力は変えない）。"""
    safe = redact_all(rows)
    (ROOT / "results" / name).write_text(json.dumps(safe, ensure_ascii=False, indent=1))


def product_system_prompt() -> str:
    """製品の通常チャットの system prompt（Rust の既定値を読む・手書きで写さない）。"""
    src = (ROOT.parents[1] / "crates/chat/src/worker/config.rs").read_text()
    m = re.search(r'system_prompt:\s*"((?:[^"\\]|\\.)*)"', src, re.S)
    if not m:
        raise RuntimeError("製品の system prompt が見つからない（config.rs の形が変わった）")
    # Rust の行継続（`\` ＋改行＋先頭の空白）を外す。
    return re.sub(r"\\\n\s*", "", m.group(1))


def prompts() -> dict[str, str]:
    return {
        "minimal": e2e.SYSTEM,
        "product": product_system_prompt(),
        "lookup": e2e.SYSTEM + LOOKUP_DIRECTIVE,
    }


def sample(seed: int = 3, n: int = 60) -> tuple[list[dict], list[dict], dict[int, list]]:
    """E2E 本体と同じ依頼と同じカタログ（同じ乱数）を作る。"""
    catalog = load_catalog()
    queries = [q for q in load_queries() if q["kind"] in ("ja", "en")]
    rng = random.Random(seed)
    targets = sorted({q["target"] for q in queries})
    chosen = set(rng.sample(targets, min(n // 2, len(targets))))
    qs = [q for q in queries if q["target"] in chosen]
    half = set(sorted(chosen)[: len(chosen) // 2])
    big = [q for q in qs if q["target"] in half]
    subsets = fixed_target_subsets(catalog, {q["target"] for q in qs}, [200, 1000])
    return qs, big, {len(t): t for t in subsets.values()}


def run() -> list[dict]:
    qs, big, subsets = sample()
    rows: list[dict] = []
    variants = [(name, system, "generic") for name, system in prompts().items()]
    variants += [(name, prompts()[name], "llm") for name in LLM_MOCK_PROMPTS]
    for condition, n in RUNS:
        tools = subsets[n]
        searcher = e2e.Searcher(tools, condition) if condition != "all" else None
        batch = big if n >= 1000 else qs
        for name, system, mock in variants:
            with ThreadPoolExecutor(max_workers=4) as pool:
                out = list(
                    pool.map(
                        lambda q, s=system, m=mock, ts=tools, c=condition, se=searcher: e2e.run_one(
                            ts, c, q, se, system=s, mock=m
                        ),
                        batch,
                    )
                )
            rows += [{**r, "prompt": name, "mock": mock} for r in out]
            print(f"probe n={n} {condition} {name} mock={mock} done", flush=True)
    for condition, n, name, mock in EXTRA_RUNS:
        tools = subsets[n]
        system = prompts()[name]
        with ThreadPoolExecutor(max_workers=4) as pool:
            out = list(
                pool.map(
                    lambda q, s=system, m=mock, ts=tools, c=condition: e2e.run_one(
                        ts, c, q, None, system=s, mock=m
                    ),
                    big,
                )
            )
        rows += [{**r, "prompt": name, "mock": mock} for r in out]
        print(f"probe n={n} {condition} {name} mock={mock} done", flush=True)
    write_results("e2e_ask_probe.json", rows)
    return rows


JUDGE = """あなたは対話ログの判定者です。AI アシスタントが利用者の依頼に対し、ツールを呼ばずに応答を終えました。
その最後の応答が次のどれに当たるかを判定してください。

- avoidable: 利用者に情報を尋ねているが、尋ねている情報が**すべて**下の「カタログにあるツール」の一覧・検索・取得で調べられる（まだ呼んでいないツールでもよい）。
- legit: 利用者に情報を尋ねており、そのうち**一つでも**「カタログにあるツール」では得られない。
- other: 利用者に情報を尋ねていない（実行を拒否した、説明だけした、別のツールで済ませたと報告した、など）。

判定の決まり:
- avoidable と判定するのは、尋ねている情報ごとに、それを返す具体的なツールを下の一覧から名前で挙げられるときだけ。挙げられない情報が一つでもあれば legit。
- 次の情報はツールでは得られない: 今日の日付や現在時刻（「来週の金曜」などの計算に要る）、依頼に書かれていない宛先（メールアドレスなど）、送る文面や本文、ファイルの中身、利用者の意図や方針の選択。
- 一覧に無いツール（例: 一覧に list_blocks が無いのにブロック一覧が要る）で調べられる、という判定はしない。

依頼:
{query}

本来呼ぶべきツール:
{target}

この会話のカタログにある、同じサービスのツール（名前: 説明）:
{siblings}

この会話でアシスタントが呼んだツール（呼んだ順）:
{called}

アシスタントの最後の応答:
{final}

JSON だけを返してください: {{"category": "avoidable|legit|other", "asked": "尋ねている情報（短く）", "tools": "avoidable のとき、各情報を返すツールの名前（一覧の名前そのまま）", "reason": "判定の理由（短く）"}}"""


def stop_reason(row: dict) -> str:
    """会話の終わり方。記録の無い古い会話は、正解の有無とステップ数から推す（上限で文章を返して
    終えた会話も打ち切りに数えるので、打ち切りはやや多めに出る）。"""
    if "stop" in row:
        return row["stop"]
    if row["correct"]:
        return "target"
    return "max_steps" if row["steps"] >= e2e.MAX_STEPS else "reply"


def judge_one(row: dict, catalog: dict[str, dict]) -> dict:
    """1 会話の最後の応答を分類する。`catalog` はその会話で提示されたカタログ（名前 → 定義）。"""
    target = catalog[row["target"]]
    service = e2e.namespace(target["name"])
    sib = [t for n, t in catalog.items() if e2e.namespace(n) == service]
    prompt = JUDGE.format(
        query=row["query_text"],
        target=f"{target['name']}: {target['description']}\n引数: "
        + json.dumps(target["input_schema"], ensure_ascii=False)[:800],
        siblings="\n".join(f"- {t['name']}: {t['description'][:120]}" for t in sib),
        called=", ".join(row["calls"]) or "（なし）",
        final=clip_middle(row["final_text"]) or "（本文なし）",
    )
    # 判定と検算の手順が変われば取り直す（同じ文面でも以前の判定を使わない）。
    src = inspect.getsource(judge_one) + inspect.getsource(check_avoidable) + VERIFY
    key = hashlib.sha256((src + prompt).encode()).hexdigest()[:20]
    path = JUDGE_CACHE / f"{key}.json"
    if path.exists():
        return json.loads(path.read_text())
    try:
        out = chat_json(prompt, temperature=0.0)
    except RuntimeError as e:  # 失敗は記録して続ける（キャッシュしない）。
        return {"category": "error", "asked": "", "tools": "", "reason": str(e)[:200]}
    if not isinstance(out, dict) or out.get("category") not in ("avoidable", "legit", "other"):
        return {
            "category": "error",
            "asked": "",
            "tools": "",
            "reason": f"形式が不正: {out!r}"[:200],
        }
    if out["category"] == "avoidable":
        try:
            out = check_avoidable(out, row, target, sib)
        except RuntimeError as e:
            return {"category": "error", "asked": "", "tools": "", "reason": str(e)[:200]}
    JUDGE_CACHE.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out, ensure_ascii=False))
    return out


VERIFY = """次の判定を検算してください。AI アシスタントが利用者に「{asked}」を尋ねました。
判定者は、それが次のツールで調べられると言っています: {tools}

それぞれのツールの説明と引数:
{tool_defs}

利用者の依頼:
{query}

質問: 尋ねている情報の**すべて**を、上のツールの戻り値から得られますか（依頼文に書かれた名前などを手がかりに、一覧・検索・取得して特定できるか）。
一つでも得られない情報があれば no。ツールの説明から読み取れない場合も no。

JSON だけを返してください: {{"answer": "yes|no", "reason": "短く"}}"""


def check_avoidable(out: dict, row: dict, target: dict, sib: list[dict]) -> dict:
    """「避けられた」の検算（#552 のレビュー指摘）。挙げたツールがその会話のカタログの同じ
    サービスにあり、正解ツール自身でなく、尋ねた情報を返せると別の呼び出しで確かめられた
    ときだけ avoidable を残す。確かめられなければ legit に倒す（避けられた聞き返しを
    多く見積もらない）。"""
    by_name = {t["name"]: t for t in sib}
    raw = out.get("tools") or ""
    named = raw if isinstance(raw, list) else re.split(r"[,、\s]+", str(raw))
    tools = [n.strip(" '\"[]") for n in named if n.strip(" '\"[]") in by_name]
    tools = [n for n in tools if n != target["name"]]
    if not tools:
        return {**out, "category": "legit", "check": "挙げたツールがカタログに無い・正解ツール自身"}
    verdict = chat_json(
        VERIFY.format(
            asked=out.get("asked", ""),
            tools=", ".join(tools),
            tool_defs="\n".join(
                f"- {n}: {by_name[n]['description'][:200]}\n  引数: "
                + json.dumps(by_name[n]["input_schema"], ensure_ascii=False)[:400]
                for n in tools
            ),
            query=row["query_text"],
        ),
        temperature=0.0,
    )
    if isinstance(verdict, dict) and verdict.get("answer") == "yes":
        return {**out, "tools": tools, "check": "検算で確認"}
    reason = verdict.get("reason", "") if isinstance(verdict, dict) else ""
    return {**out, "category": "legit", "tools": tools, "check": f"検算で否定: {reason}"[:200]}


def clip_middle(text: str, head: int = 1500, tail: int = 3000) -> str:
    """長い応答は先頭と末尾を残す（末尾の質問を落とさない）。"""
    if len(text) <= head + tail:
        return text
    return text[:head] + "\n…（中略）…\n" + text[-tail:]


def judge(rows: list[dict]) -> list[dict]:
    """正解ツールを呼ばずに文章で終えた会話（下調べのツールを呼んだものも含む）を分類する。"""
    qtext = {q["id"]: q["text"] for q in load_queries()}
    _, _, subsets = sample()
    catalogs = {n: {t.name: t.tooldef("ja") for t in tools} for n, tools in subsets.items()}
    targets = [
        {**r, "query_text": qtext[r["id"]]}
        for r in rows
        if not r["error"] and stop_reason(r) == "reply"
    ]
    with ThreadPoolExecutor(max_workers=4) as pool:
        verdicts = list(pool.map(lambda r: judge_one(r, catalogs[r["n"]]), targets))
    out = [
        {
            "id": r["id"],
            "condition": r["condition"],
            "n": r["n"],
            "prompt": r["prompt"],
            "mock": r["mock"],
            "calls": r["calls"],
            "final_text": r["final_text"],
            **v,
        }
        for r, v in zip(targets, verdicts, strict=True)
    ]
    write_results("e2e_ask_judge.json", out)
    return out


def main() -> None:
    rows = run()
    judge(rows)


if __name__ == "__main__":
    main()
