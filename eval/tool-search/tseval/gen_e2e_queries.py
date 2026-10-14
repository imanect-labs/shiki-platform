"""E2E の依頼データ v2（#547）: 正解ツールの必須引数が、依頼文かカタログのツールで埋まる依頼。

v1（`data/queries.json` の ja / en）には、宛先・本文・日付や、引くツールの無い ID を依頼文に
書いていないものが多く、聞き返すのが正しい依頼が混ざっていた（#545）。v2 は次のように作る。

- 正解ツールとカタログは v1 の E2E と同じ（同じ乱数・`ask_probe.sample`）。200 件は 30 件、
  1,000 件はその半分の 15 件。
- 依頼はカタログの規模ごとに作る。下調べに使えるツール（同じサービスの一覧・検索・取得）は
  その会話のカタログにあるものだけで、規模で違う（予定を探すツールが 1,000 件にしか無い、など）。
- 依頼文（日英）と一緒に、正解ツールまでの手順（plan: 呼ぶツールと引数）を出させる。引数の値は
  依頼文に書かれた文字列か、前の手順の結果の参照のどちらか。
- plan を機械的に検査する（[`check_plan`]）: 各手順の必須引数が揃っている／文字列の値が日英それぞれの
  依頼文に書かれている／参照は前の手順を指す／下調べのツールはその規模のカタログにある一覧・検索・
  取得のツールである。
- 依頼文に書いた ID が、カタログのツールで調べられるものなら作り直させる（[`written_ids`]・
  「調べられる ID は書かない」を生成役任せにすると、ほぼ全件で ID を書いてしまう）。
- plan を見せずに、別の呼び出しで「依頼文とカタログだけで必須引数が埋まるか」も判定させる
  （[`independent_check`]）。
- どちらかが通らなければ理由を添えて作り直させる（最大 `MAX_ATTEMPTS` 回）。それでも通らない
  正解ツールがあれば止める（通らない依頼を混ぜない）。
- 一部の正解ツール（[`MISSING`]）では、ツールでは得られない必須の値をわざと 1 つ書かない依頼
  （`kind` が `ja_missing` / `en_missing`）も作る。聞き返せたら正解（聞くべきときに聞けるか）。

出力: data/e2e_queries_v2.json（[{id, target, kind, n, text, plan, missing}]）
"""

from __future__ import annotations

import hashlib
import inspect
import json
import re
import unicodedata
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from . import ask_probe, e2e
from .catalog import Tool, load_catalog
from .llm import chat_json

DATA = Path(__file__).resolve().parent.parent / "data"
CACHE = DATA / "cache" / "e2e_queries_v2"
OUT = DATA / "e2e_queries_v2.json"
MAX_ATTEMPTS = 5

# E2E の system prompt に入れる固定の「今日」（製品の `today_note` と同じ文面・#546）。
TODAY_JA = "2026年10月14日（水）"
TODAY_NOTE = (
    f"\n\n今日は {TODAY_JA}です（Asia/Tokyo）。"
    "「今日」「来週」「今月」などはこの日付を基準に解釈してください。"
)

# 情報がわざと足りない依頼を作る正解ツールと、書かない引数（ツールでは得られない値）。
# 1,000 件の比較に使う半分（ask_probe.sample の big）から 2 件、残りから 2 件。
MISSING = {
    "dropbox_share_folder": "member_email",
    "docusign_create_webhook": "url",
    "stripe_create_customer": "email",
    "teams_send_message": "content",
}

# 下調べに使えるツール（一覧・検索・取得）。名前の動詞で決める。
LOOKUP = re.compile(r"_(list|get|search|describe|find|query)(_|$)")
# ID の形の引数（依頼文に書いてあれば、調べられるものかを確かめる）。
ID_ARG = re.compile(r"(^|_)(id|key|ts)$")

GEN_PROMPT = """業務 SaaS のツールを使う AI アシスタントの評価用に、利用者からの依頼文を日本語と英語で 1 つずつ作ってください。
依頼は、下の「正解ツール」を呼ぶことで満たされるものにします。今日は {today}です。

正解ツール:
{target}

このカタログで使える、同じサービスのツール:
{siblings}

要件:
- 正解ツールの必須引数は、すべて「依頼文に書かれた値」か「上のツールで調べられる値」で埋まること。利用者に聞き返さずに実行できる依頼にする。
- 宛先（メールアドレスなど）・送る本文・名前・日付・金額・URL など、ツールでは調べられない値は依頼文に具体的に書く。日付は「10月21日」のように具体的に書く（「来週」などの相対表現だけにしない）。
- ID は、上のツールで一覧・検索・取得して調べられるときは依頼文に書かない（名前や件名など、人が普段使う手がかりを書く）。調べるツールが上に無い ID は依頼文に書く（例: 「ルーム ID 2283910」）。
- 依頼の意図に必要な任意の引数（変更後の値など）も依頼文に書く。
- 依頼文は社内の業務担当者が書く自然な文（1〜3 文）。ツール名は書かない。サービス名は書く。
- URL・メールアドレスは example.com などの例示用ドメインにする。トークンや Webhook の URL など、秘密情報の形をした文字列は書かない。
{missing_rule}
あわせて、正解ツールまでの手順（plan）を書いてください。plan は呼ぶ順のツールの列で、最後が正解ツールです。
- 各手順: {{"tool": "ツール名", "args": {{"引数名": 値}}, "pick": 手がかり}}
- 値は次のどれか:
  - 依頼文に書かれた文字列そのもの（日英の両方の依頼文にそのまま現れるもの。メールアドレス・ID・英数字の名前など）
  - {{"value": 実際に渡す値, "ja": "日本語の依頼文の該当箇所", "en": "英語の依頼文の該当箇所"}}（言語で書き方が違う値・数値・日付・選択肢。該当箇所は依頼文の部分文字列そのまま）
  - {{"from": 手順の番号（0 始まり）, "field": "結果のフィールド名"}}（前の手順の結果から取る値）
- 一覧・検索の手順では、結果からどれを選ぶかの手がかりを "pick" に書く（{{"ja": "...", "en": "..."}}・依頼文の部分文字列そのまま）。最後の手順には不要。
- 下調べが要らないなら（ID を依頼文に書いた場合など）正解ツールだけの plan にする。

JSON だけを返してください: {{"ja": "...", "en": "...", "plan": [...]}}
"""

MISSING_RULE = """- ただし例外として、正解ツールの必須引数 `{arg}`（{desc}）の値だけは依頼文に書かない（利用者に尋ねないと分からない状態にする。「先方に」「いつもの内容で」のようにぼかしてもよい）。plan ではその引数を {{"missing": true}} にする。
"""

RETRY = """前回の出力は次の点で要件を満たしませんでした。直して、同じ形の JSON を返してください。
{errors}

（「依頼文にもツールにも無い」と言われた値は、依頼文に具体的に書く。ID を調べるツールが上の一覧に無いなら、ID そのものを依頼文に書く。）

前回の出力:
{previous}"""

LOOKUPABLE_PROMPT = """次の依頼で、引数の値を依頼文に書かずに、下のツールで調べられるかを判定してください。

依頼:
{text}

引数: {arg}（{desc}）。依頼文では「{value}」と書かれている。

調べものに使えるツール（これ以外のツールは無い）:
{siblings}

この値を依頼文から消しても、依頼文にある名前・件名などの手がかりを使って、上のツールの戻り値から特定できますか。
ツールの説明から、その種類の ID を返すと読み取れるときだけ yes。

JSON だけを返してください: {{"answer": "yes|no", "tool": "yes のとき使うツールの名前", "reason": "短く"}}"""

CHECK_PROMPT = """次の依頼を、下のツールだけを使って、利用者に聞き返さずに実行できるかを判定してください。今日は {today}です。

依頼:
{text}

実行に使うツール:
{target}

調べものに使えるツール（これ以外のツールは無い）:
{siblings}

実行に使うツールの必須引数ごとに、値をどう得るかを答えてください。
- "request": 依頼文に書かれている（依頼文の表現から値が一意に決まる）
- "tool:<ツール名>": 上の「調べものに使えるツール」で調べられる（そのツールの必須引数も、依頼文か別のツールで埋まる）
- "missing": 依頼文にも、上のツールにも無い（利用者に尋ねる必要がある）

JSON だけを返してください: {{"args": {{"引数名": "request|tool:<ツール名>|missing"}}, "reason": "短く"}}"""


def norm(s: str) -> str:
    """部分一致の比較用（全角半角・大小・空白を揃える）。"""
    return re.sub(r"\s+", "", unicodedata.normalize("NFKC", s).casefold())


def describe(t: Tool) -> str:
    d = t.tooldef("ja")
    schema = d["input_schema"]
    req = set(schema.get("required", []))
    args = "、".join(
        f"{k}{'（必須）' if k in req else ''}: {v.get('description', '')}"
        for k, v in schema.get("properties", {}).items()
    )
    return f"- {d['name']}: {d['description']}\n  引数: {args or 'なし'}"


def required(t: Tool) -> list[str]:
    return list(t.tooldef("ja")["input_schema"].get("required", []))


def lookups(target: Tool, catalog200: list[Tool]) -> list[Tool]:
    """下調べに使えるツール: 200 件のカタログにある、同じサービスの一覧・検索・取得のツール。"""
    ns = e2e.namespace(target.name)
    return [
        t
        for t in catalog200
        if t.name != target.name and e2e.namespace(t.name) == ns and LOOKUP.search(t.name)
    ]


def check_value(
    where: str, arg: str, v: object, texts: dict[str, str], step: int, can_miss: bool
) -> list[str]:
    """plan の引数 1 つの検査。"""
    if isinstance(v, dict) and v.get("missing") is True:
        return [] if can_miss else [f"{where}: {arg} を missing にできるのは指定した引数だけ"]
    if isinstance(v, dict) and "from" in v:
        n = v.get("from")
        if not isinstance(n, int) or not 0 <= n < step:
            return [f"{where}: {arg} の参照先 {n!r} は前の手順ではない"]
        return []
    if isinstance(v, dict) and "value" in v:
        errs = []
        for lang in ("ja", "en"):
            span = v.get(lang)
            if not isinstance(span, str) or not span.strip():
                errs.append(f"{where}: {arg} の {lang} の該当箇所が無い")
            elif norm(span) not in norm(texts[lang]):
                errs.append(f"{where}: {arg} の {lang} の該当箇所「{span}」が依頼文に無い")
        return errs
    if isinstance(v, str):
        missing = [lang for lang in ("ja", "en") if norm(v) not in norm(texts[lang])]
        return (
            [
                f"{where}: {arg} の値「{v}」が {'・'.join(missing)} の依頼文に無い"
                "（言語で書き方が違うなら {value, ja, en} の形にする）"
            ]
            if missing
            else []
        )
    if isinstance(v, list):
        return [e for x in v for e in check_value(where, arg, x, texts, step, False)]
    return [f"{where}: {arg} の値 {v!r} は {{value, ja, en}} の形にする（依頼文の該当箇所を示す）"]


def check_plan(item: dict, target: Tool, available: list[Tool], missing: str | None) -> list[str]:
    """plan の機械的な検査。誤りの一覧を返す（空なら合格）。"""
    texts = {lang: item.get(lang) for lang in ("ja", "en")}
    if not all(isinstance(t, str) and t.strip() for t in texts.values()):
        return ["ja / en の依頼文が無い"]
    plan = item.get("plan")
    if not isinstance(plan, list) or not plan or not all(isinstance(s, dict) for s in plan):
        return ["plan が無い・形が不正"]
    errs: list[str] = []
    for lang, text in texts.items():
        if target.name in text:
            errs.append(f"{lang} の依頼文にツール名を書いている")
        if any(p.search(text) for p in ask_probe.SECRET_LIKE):
            errs.append(f"{lang} の依頼文に秘密情報の形をした文字列がある")
    tools = {t.name: t for t in available} | {target.name: target}
    last = len(plan) - 1
    if plan[last].get("tool") != target.name:
        errs.append(f"plan の最後が正解ツール {target.name} ではない")
    for i, step in enumerate(plan):
        name = step.get("tool")
        where = f"手順 {i}（{name}）"
        if name not in tools or (i < last and name == target.name):
            errs.append(f"{where}: 下調べに使えるツールではない（使えるのは上の一覧だけ）")
            continue
        args = step.get("args") or {}
        if not isinstance(args, dict):
            errs.append(f"{where}: args の形が不正")
            continue
        errs += [f"{where}: 必須引数 {r} が無い" for r in required(tools[name]) if r not in args]
        req = required(tools[name])
        for k, v in args.items():
            # 下調べの任意の引数（絞り込みの項目名など）と、任意の真偽値・数値（再帰の有無・
            # 件数など）は依頼文の根拠を求めない（検索の仕方であって、依頼の内容ではない）。
            if k not in req and (i < last or isinstance(v, (bool, int, float))):
                continue
            errs += check_value(where, k, v, texts, i, i == last and k == missing)
        if i < last:
            pick = step.get("pick")
            if isinstance(pick, dict) and "value" not in pick:
                pick = {**pick, "value": pick.get("ja")}  # 手がかりは該当箇所だけでよい。
            if pick is not None:
                errs += check_value(where, "pick", pick, texts, i, False)
            elif not any(
                isinstance(v, str) or (isinstance(v, dict) and "value" in v) for v in args.values()
            ):
                errs.append(f"{where}: 結果からどれを選ぶかの手がかり（pick）が無い")
    if missing is not None and plan[last].get("args", {}).get(missing) != {"missing": True}:
        errs.append(f'正解ツールの {missing} を {{"missing": true}} にしていない')
    return errs


def literal(v: object) -> str | None:
    """plan の値が依頼文に書かれた値なら、その書き方（日本語の依頼文の該当箇所）。"""
    if isinstance(v, str):
        return v
    if isinstance(v, dict) and "value" in v:
        return str(v.get("ja"))
    return None


def written_ids(item: dict, target: Tool, available: list[Tool]) -> list[str]:
    """依頼文に書いた ID の形の値のうち、カタログのツールで調べられるもの（書かずに調べさせる）。"""
    if not available:
        return []
    tools = {t.name: t for t in available} | {target.name: target}
    names = {t.name for t in available}
    errs = []
    for step in item["plan"]:
        tool = tools[step["tool"]]
        props = tool.tooldef("ja")["input_schema"].get("properties", {})
        for arg in required(tool):
            value = literal(step["args"].get(arg))
            if value is None or not ID_ARG.search(arg):
                continue
            out = chat_json(
                LOOKUPABLE_PROMPT.format(
                    text=item["ja"],
                    arg=arg,
                    desc=props.get(arg, {}).get("description", ""),
                    value=value,
                    siblings="\n".join(describe(t) for t in available),
                ),
                temperature=0.0,
            )
            if not isinstance(out, dict) or out.get("answer") != "yes":
                continue
            via = str(out.get("tool", "")).strip()
            if via in names:
                errs.append(
                    f"{step['tool']} の {arg}（「{value}」）は {via} で調べられる。"
                    "依頼文には ID を書かず、名前などの手がかりを書いて、調べる手順を plan に入れる"
                )
    return errs


def independent_check(
    text: str, target: Tool, available: list[Tool], missing: str | None
) -> list[str]:
    """plan を見せずに、依頼文とカタログだけで必須引数が埋まるかを判定させる。"""
    out = chat_json(
        CHECK_PROMPT.format(
            today=TODAY_JA,
            text=text,
            target=describe(target),
            siblings="\n".join(describe(t) for t in available) or "（なし）",
        ),
        temperature=0.0,
    )
    got = out.get("args") if isinstance(out, dict) else None
    if not isinstance(got, dict):
        return [f"判定の形が不正: {out!r}"[:200]]
    reason = out.get("reason", "")
    names = {t.name for t in available}
    errs = []
    for arg in required(target):
        how = str(got.get(arg, "missing"))
        if arg == missing:
            if how != "missing":
                errs.append(f"{arg} は書かない指定だが、判定者は「{how}」で埋まると判断した")
        elif how == "missing":
            errs.append(f"判定者は {arg} が依頼文にもツールにも無いと判断した（{reason}）")
        elif how.startswith("tool:") and how.removeprefix("tool:").strip() not in names:
            errs.append(f"判定者は {arg} を一覧に無いツール {how} で埋めた（{reason}）")
    return errs


def generate(target: Tool, available: list[Tool], missing: str | None, n: int) -> dict:
    """1 つの正解ツールの依頼（日英）を、検査を通るまで作り直す。"""
    rule = ""
    if missing is not None:
        desc = target.tooldef("ja")["input_schema"]["properties"][missing].get("description", "")
        rule = MISSING_RULE.format(arg=missing, desc=desc)
    prompt = GEN_PROMPT.format(
        today=TODAY_JA,
        target=describe(target),
        siblings="\n".join(describe(t) for t in available) or "（なし）",
        missing_rule=rule,
    )
    # 検査の手順が変われば作り直す（以前の検査だけを通った依頼を使わない）。
    checks = "".join(
        inspect.getsource(f) for f in (check_value, check_plan, written_ids, independent_check)
    )
    key = hashlib.sha256(
        (prompt + LOOKUPABLE_PROMPT + CHECK_PROMPT + RETRY + checks).encode()
    ).hexdigest()[:16]
    path = CACHE / f"{target.name}-{n}-{'missing' if missing else 'full'}-{key}.json"
    if path.exists():
        return json.loads(path.read_text())
    messages = prompt
    log = []
    for attempt in range(1, MAX_ATTEMPTS + 1):
        item = chat_json(messages, temperature=0.7)
        if not isinstance(item, dict):
            errs = [f"JSON の形が不正: {item!r}"[:200]]
        else:
            errs = check_plan(item, target, available, missing)
            if not errs:
                errs = written_ids(item, target, available)
            if not errs:
                for lang in ("ja", "en"):
                    errs += [
                        f"{lang}: {e}"
                        for e in independent_check(item[lang], target, available, missing)
                    ]
        log.append(errs)
        if not errs:
            out = {**item, "attempts": attempt, "rejections": log[:-1]}
            CACHE.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(out, ensure_ascii=False))
            return out
        messages = (
            prompt
            + "\n\n"
            + RETRY.format(
                errors="\n".join(f"- {e}" for e in errs),
                previous=json.dumps(item, ensure_ascii=False),
            )
        )
    raise RuntimeError(f"{target.name}（n={n}, missing={missing}）: 検査を通らない: {log[-1]}")


def main() -> None:
    qs, big, subsets = ask_probe.sample()
    catalog = {t.name: t for t in load_catalog()}
    per_size = {200: {q["target"] for q in qs}, 1000: {q["target"] for q in big}}
    jobs = [
        (name, missing, n)
        for n, targets in per_size.items()
        for name, missing in [(t, None) for t in sorted(targets)]
        + sorted((t, a) for t, a in MISSING.items() if t in targets)
    ]
    with ThreadPoolExecutor(max_workers=4) as pool:
        made = list(
            pool.map(
                lambda j: generate(
                    catalog[j[0]], lookups(catalog[j[0]], subsets[j[2]]), j[1], j[2]
                ),
                jobs,
            )
        )
    queries = []
    for (name, missing, n), item in zip(jobs, made, strict=True):
        for lang in ("ja", "en"):
            kind = f"{lang}_missing" if missing else lang
            queries.append(
                {
                    "id": f"{name}:{kind}@{n}",
                    "target": name,
                    "kind": kind,
                    "n": n,
                    "text": item[lang],
                    "plan": item["plan"],
                    "missing": missing,
                    "attempts": item["attempts"],
                }
            )
    OUT.write_text(json.dumps(queries, ensure_ascii=False, indent=1))
    retried = sum(1 for m in made if m["attempts"] > 1)
    chains = sum(1 for m in made if len(m["plan"]) > 1)
    print(
        f"{len(jobs)} 件（正解ツール × 規模）/ {len(queries)} 件の依頼"
        f"（作り直しあり {retried} 件・下調べの手順あり {chains} 件）"
    )


if __name__ == "__main__":
    main()
