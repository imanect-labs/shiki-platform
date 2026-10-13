"""評価クエリの生成（正解ツール 1 つに対する依頼文と、エージェントが書く検索語）。

正解ツールごとに 3 種の依頼文を作る:
- `ja`      … 業務の日本語（必要ならサービス名を含む）
- `en`      … 英語
- `ja_para` … 日本語の言い換え（ツール名・説明の語をなるべく使わない＝語彙のずれを測る）

さらに各依頼文から、エージェントが `tool_search` に渡しそうな短い検索語（`agent`）を
別の呼び出しで作る（実運用でクエリを書くのはユーザーではなくモデルなので）。

紛らわしい兄弟ツール（同じサービスの他ツール）を一緒に見せ、「正解ツールでしか
満たせない依頼」に限定させる（正解が一意でない問いを作らない）。

出力: data/queries.json（[{id, target, kind, text}]）
"""

from __future__ import annotations

import json
import random
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from .catalog import Tool, load_catalog
from .llm import chat_json

DATA = Path(__file__).resolve().parent.parent / "data"
CACHE = DATA / "cache" / "queries"

REQUEST_PROMPT = """次の「正解ツール」でしか満たせない、ユーザーからの依頼文を 3 種類作ってください。

正解ツール:
- name: {name}
- 説明: {desc}

紛らわしい近縁ツール（これらでは満たせない依頼にすること）:
{siblings}

要件:
- ja: 社内の業務担当者が書く自然な日本語の依頼（1〜2 文）。どのサービスか分からないと一意に決まらない場合はサービス名を含める。
- en: 同じ意図の英語の依頼（1〜2 文）。
- ja_para: 同じ意図の日本語の言い換え。ツール名・説明に出てくる語をなるべく避け、口語的・遠回しな言い方にする。ただし意図は一意に読み取れること。
- 具体的な値（チャンネル名・日付・金額など）を自然に含めてよい。
- ツール名そのもの（{name}）は書かない。

出力は JSON だけ: {{"ja": "...", "en": "...", "ja_para": "..."}}
"""

AGENT_PROMPT = """あなたは AI エージェントです。手元のツールだけでは足りないので、ツールカタログを検索する
`tool_search(query)` を呼んで必要なツールを探します。次の各依頼について、あなたが渡す query を書いてください。

- query は短い検索語（キーワードや短い句・5〜12 語程度）。言語は自由（日本語でも英語でもよい）。
- 実在しそうなツール名を推測して書いてもよいが、依頼文の丸写しはしない。

依頼:
{items}

出力は JSON だけ: 依頼の番号をキーにした query の対応 {{"1": "...", "2": "..."}}
"""


def _siblings(target: Tool, catalog: list[Tool], k: int = 12) -> str:
    same = [t for t in catalog if t.service == target.service and t.name != target.name]
    random.Random(target.name).shuffle(same)
    return "\n".join(f"- {t.name}: {t.description_ja}" for t in same[:k]) or "（なし）"


def requests_for(target: Tool, catalog: list[Tool]) -> dict:
    path = CACHE / f"req-{target.name}.json"
    if path.exists():
        return json.loads(path.read_text())
    raw = chat_json(
        REQUEST_PROMPT.format(
            name=target.name, desc=target.description_ja, siblings=_siblings(target, catalog)
        )
    )
    # 1 件だけ頼んでも配列で複数返すことがある。先頭を採る（どれも同じ要件を満たす）。
    if isinstance(raw, list) and raw:
        raw = raw[0]
    if not isinstance(raw, dict) or not all(
        isinstance(raw.get(k), str) for k in ("ja", "en", "ja_para")
    ):
        raise RuntimeError(f"依頼文の形が不正: {target.name}: {raw}")
    out = {k: raw[k].strip() for k in ("ja", "en", "ja_para")}
    CACHE.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out, ensure_ascii=False))
    return out


def agent_queries(batch_id: str, texts: list[str]) -> list[str]:
    path = CACHE / f"agent-{batch_id}.json"
    if path.exists():
        return json.loads(path.read_text())
    items = "\n".join(f"{i + 1}. {t}" for i, t in enumerate(texts))
    raw = chat_json(AGENT_PROMPT.format(items=items), temperature=0.3)
    if not isinstance(raw, dict):
        raise RuntimeError(f"検索語の形が不正: {raw}")
    out = [str(raw.get(str(i + 1), "")).strip() or texts[i] for i in range(len(texts))]
    CACHE.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out, ensure_ascii=False))
    return out


def pick_targets(catalog: list[Tool], per_service: int = 3, seed: int = 7) -> list[Tool]:
    """正解ツールの選び方: 実ツールは全部、合成はサービスごとに `per_service` 個。"""
    rng = random.Random(seed)
    real = [t for t in catalog if t.service == "shiki"]
    by_svc: dict[str, list[Tool]] = {}
    for t in catalog:
        if t.service != "shiki":
            by_svc.setdefault(t.service, []).append(t)
    synth = [
        t for tools in by_svc.values() for t in rng.sample(tools, min(per_service, len(tools)))
    ]
    return real + synth


def main() -> None:
    catalog = load_catalog()
    targets = pick_targets(catalog)
    with ThreadPoolExecutor(max_workers=4) as pool:
        reqs = list(pool.map(lambda t: requests_for(t, catalog), targets))
    queries = []
    for t, r in zip(targets, reqs, strict=True):
        for kind in ("ja", "en", "ja_para"):
            queries.append(
                {"id": f"{t.name}:{kind}", "target": t.name, "kind": kind, "text": r[kind]}
            )
    # エージェントの検索語は ja 依頼から作る（20 件ずつまとめて頼む）。
    ja = [q for q in queries if q["kind"] == "ja"]
    batches = [ja[i : i + 20] for i in range(0, len(ja), 20)]
    with ThreadPoolExecutor(max_workers=4) as pool:
        agent = list(
            pool.map(
                lambda b: agent_queries(f"{b[0]['target']}-{len(b)}", [q["text"] for q in b]),
                batches,
            )
        )
    for b, qs in zip(batches, agent, strict=True):
        for q, text in zip(b, qs, strict=True):
            queries.append(
                {"id": f"{q['target']}:agent", "target": q["target"], "kind": "agent", "text": text}
            )
    (DATA / "queries.json").write_text(json.dumps(queries, ensure_ascii=False, indent=1))
    print(f"{len(targets)} targets / {len(queries)} queries")


if __name__ == "__main__":
    main()
