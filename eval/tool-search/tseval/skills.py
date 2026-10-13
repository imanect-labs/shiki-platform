"""skill の発見方式の比較（一覧 vs 検索）。

skill は tool search に統合せず、`skill` ツールの説明に name: description を一覧する方式を採る
（Anthropic Agent Skills・Claude Code・Codex と同じ）。一覧方式は件数（文字予算）で破綻するため、
**どの件数から劣化するか**を測り、skill 専用の検索を入れる閾値の根拠にする。

条件（件数 N ごと）:
- `list_cap50` … 製品の現行（name 順に先頭 50 件・説明は 200 字まで・`skill_catalog.rs`）
- `list_all`   … 上限なしで全件を一覧
- `search`     … `skill_search` で検索（製品の BM25F）してから `skill` で読み込む

出力: data/skills.json / data/skill_queries.json / results/skills_e2e.json
"""

from __future__ import annotations

import hashlib
import json
import random
import re
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from . import bm25
from .llm import chat, chat_json

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "data"
CACHE = DATA / "cache" / "skills"
MAX_LISTED = 50
MAX_DESC = 200

DOMAINS = [
    "経理・決算",
    "経費精算",
    "請求・債権管理",
    "購買・調達",
    "人事・採用",
    "労務・勤怠",
    "研修・育成",
    "法務・契約",
    "コンプライアンス・監査",
    "情報システム・ヘルプデスク",
    "セキュリティ",
    "開発・コードレビュー",
    "インフラ・運用",
    "データ分析",
    "マーケティング",
    "広報・PR",
    "営業・提案",
    "カスタマーサポート",
    "品質保証",
    "製造・生産管理",
    "物流・在庫",
    "経営企画・予算",
    "総務・ファシリティ",
    "IR・開示",
    "研究開発",
]

SKILL_PROMPT = """社内 AI アシスタント用の「スキル」（作業手順を書いた指示文のパッケージ）を、分野「{domain}」について {count} 個作ってください。

要件:
- name は英小文字の kebab-case（例: `monthly-close-checklist`）。分野内で重複させない。
- description は日本語で 80〜200 字。何の作業をどう進めるスキルか・どんな依頼で使うかを書く。
- 分野内で紛らわしい近縁スキル（例: 月次と四半期、作成とレビュー）を最低 3 組含める。

出力は JSON 配列だけ: [{{"name": "...", "description": "..."}}]
"""

REQUEST_PROMPT = """次の「正解スキル」を使うべき、ユーザーからの依頼文を 1 つ作ってください。

正解スキル: {name} — {desc}

紛らわしい近縁スキル（これらでは満たせない依頼にすること）:
{siblings}

要件: 社内の業務担当者が書く自然な日本語（1〜2 文）。スキル名は書かない。説明の語をそのまま並べない。
出力は JSON だけ: {{"text": "..."}}
"""


def generate_skills(per_domain: int = 20) -> list[dict]:
    def one(domain: str) -> list[dict]:
        path = CACHE / f"skills-{DOMAINS.index(domain):02d}.json"
        if path.exists():
            return json.loads(path.read_text())
        raw = chat_json(SKILL_PROMPT.format(domain=domain, count=per_domain))
        out = [
            {"name": s["name"], "domain": domain, "description": s["description"]}
            for s in raw
            if isinstance(s, dict)
            and isinstance(s.get("name"), str)
            and re.match(r"^[a-z0-9-]{3,64}$", s["name"])
            and isinstance(s.get("description"), str)
        ]
        CACHE.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(out, ensure_ascii=False))
        return out

    with ThreadPoolExecutor(max_workers=4) as pool:
        skills = [s for r in pool.map(one, DOMAINS) for s in r]
    seen: set[str] = set()
    uniq = [s for s in skills if not (s["name"] in seen or seen.add(s["name"]))]
    (DATA / "skills.json").write_text(json.dumps(uniq, ensure_ascii=False, indent=1))
    return uniq


def generate_queries(skills: list[dict], sample: int = 60, seed: int = 5) -> list[dict]:
    rng = random.Random(seed)
    targets = rng.sample(skills, sample)

    def one(s: dict) -> dict:
        path = CACHE / f"req-{s['name']}.json"
        if path.exists():
            return json.loads(path.read_text())
        sib = [x for x in skills if x["domain"] == s["domain"] and x["name"] != s["name"]]
        raw = chat_json(
            REQUEST_PROMPT.format(
                name=s["name"],
                desc=s["description"],
                siblings="\n".join(f"- {x['name']}: {x['description']}" for x in sib[:12]),
            )
        )
        out = {"id": f"skill:{s['name']}", "target": s["name"], "text": str(raw["text"])}
        CACHE.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(out, ensure_ascii=False))
        return out

    with ThreadPoolExecutor(max_workers=4) as pool:
        qs = list(pool.map(one, targets))
    (DATA / "skill_queries.json").write_text(json.dumps(qs, ensure_ascii=False, indent=1))
    return qs


def render_listing(entries: list[dict], cap: int | None) -> str:
    """`skill_catalog::render_tool_description` と同じ文面（cap=None は上限なし）。"""
    out = (
        "スキル（社内で定義された作業手順・指示文）を名前で読み込む。読み込むと instructions が"
        "返り、以降その指示に従って作業できる。1 メッセージ中に何個でも呼んでよい。"
        "下の一覧は name: description。必要になったスキルだけを読み込むこと。\n\n利用可能なスキル:\n"
    )
    listed = entries if cap is None else entries[:cap]
    for e in listed:
        out += f"- {e['name']}: {e['description'][:MAX_DESC].strip()}\n"
    if cap is not None and len(entries) > cap:
        out += (
            f"（他 {len(entries) - cap} 件。名前が分かれば一覧外でも読み込める。"
            "見つからないスキルはスレッドへのピンで使える）\n"
        )
    return out


SKILL_PARAMS = {
    "type": "object",
    "properties": {
        "name": {"type": "string", "description": "読み込むスキル名（ツール説明の一覧にある name）"}
    },
    "required": ["name"],
}


SEARCH_DESCRIPTION = (
    "社内のスキル（作業手順・指示文）を検索する。やりたい作業を自然文（日本語/英語）か"
    "キーワードで探すと、候補の name と説明が返る。見つけた name で skill を読み込む。"
)


def skill_tools(entries: list[dict], condition: str, search_def: dict | None) -> list[dict]:
    """条件ごとに提示するツール（一覧方式は skill 1 つ・検索方式は skill ＋ skill_search）。"""
    ordered = sorted(entries, key=lambda e: e["name"])  # 製品のカタログ源は name 順。
    if condition in ("list_cap50", "list_all"):
        listing = render_listing(ordered, MAX_LISTED if condition == "list_cap50" else None)
        return [
            {
                "type": "function",
                "function": {"name": "skill", "description": listing, "parameters": SKILL_PARAMS},
            }
        ]
    assert search_def is not None
    # search は名前一覧を持たない専用の説明。search_names は tool_search と同じく
    # 検索できる名前の一覧を説明に載せる（名前だけの一覧 ＋ 検索という中間案）。
    desc = SEARCH_DESCRIPTION
    if condition == "search_names":
        desc = (
            search_def["description"]
            .replace("まだ読み込まれていないツール", "スキル")
            .replace("tool_search", "skill_search")
        )
    return [
        {
            "type": "function",
            "function": {
                "name": "skill",
                "description": "スキル（社内で定義された作業手順・指示文）を名前で読み込む。"
                "名前が分からなければ先に skill_search で探すこと。",
                "parameters": SKILL_PARAMS,
            },
        },
        {
            "type": "function",
            "function": {
                "name": "skill_search",
                "description": desc,
                "parameters": search_def["parameters"],
            },
        },
    ]


def run_one(entries: list[dict], condition: str, q: dict, search_def: dict | None) -> dict:
    tools = skill_tools(entries, condition, search_def)
    # キーは入力内容から作る（依頼文・提示するツール定義が変われば取り直す）。
    digest = hashlib.sha256(
        json.dumps([condition, q["text"], tools], ensure_ascii=False).encode()
    ).hexdigest()[:16]
    path = CACHE / f"e2e-v2-{condition}-{len(entries)}-{q['target']}-{digest}.json"
    if path.exists():
        return json.loads(path.read_text())
    messages = [
        {
            "role": "system",
            "content": "あなたは社内業務を手伝う AI アシスタントです。"
            "依頼に合うスキルがあれば読み込んでから作業してください。",
        },
        {"role": "user", "content": q["text"]},
    ]
    rec = {
        "target": q["target"],
        "condition": condition,
        "n": len(entries),
        "called": [],
        "searches": 0,
        "prompt_tokens": 0,
        "error": None,
    }
    # 検索の索引は名前順に依存しないよう決まった乱数で混ぜる（同点は定義順で解決される）。
    shuffled_entries = sorted(entries, key=lambda e: e["name"])
    random.Random(17).shuffle(shuffled_entries)
    defs = [
        {
            "name": e["name"],
            "description": e["description"],
            "input_schema": {"type": "object", "properties": {}},
        }
        for e in shuffled_entries
    ]
    by = {e["name"]: e for e in entries}
    try:
        for _ in range(4):
            r = chat(messages, tools=tools, max_tokens=2048, temperature=0.0)
            msg = r["message"]
            rec["prompt_tokens"] += r["usage"].get("prompt_tokens", 0)
            calls = msg.get("tool_calls") or []
            if not calls:
                break
            messages.append(
                {"role": "assistant", "content": msg.get("content") or "", "tool_calls": calls}
            )
            # 1 メッセージで複数の skill を読み込んでよい（説明にそう書いてある）。全部見る。
            loaded = [c for c in calls if c["function"]["name"] == "skill"]
            for c in loaded:
                try:
                    rec["called"].append(json.loads(c["function"]["arguments"]).get("name"))
                except json.JSONDecodeError:
                    rec["called"].append(None)
            if loaded:
                break
            for c in calls:
                rec["searches"] += 1
                try:
                    query = json.loads(c["function"]["arguments"] or "{}").get("query", "")
                except json.JSONDecodeError:
                    query = ""
                ranks, _ = bm25.rank(defs, [{"id": "q", "text": query or "?"}], limit=5)
                hits = ranks["q"]["search"]
                body = "\n".join(f"- {h}: {by[h]['description'][:MAX_DESC]}" for h in hits)
                messages.append(
                    {
                        "role": "tool",
                        "tool_call_id": c["id"],
                        "content": f"候補のスキル:\n{body}" if hits else "見つかりませんでした。",
                    }
                )
    except RuntimeError as e:
        rec["error"] = str(e)[:300]
    rec["correct"] = q["target"] in rec["called"]
    # 利用枠超過などの失敗はキャッシュしない（再実行で取り直す）。
    if rec["error"] is None:
        CACHE.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(rec, ensure_ascii=False))
    return rec


def main(sizes: tuple[int, ...] = (20, 50, 100, 200, 500)) -> None:
    skills = (
        json.loads((DATA / "skills.json").read_text())
        if (DATA / "skills.json").exists()
        else generate_skills()
    )
    qs = (
        json.loads((DATA / "skill_queries.json").read_text())
        if (DATA / "skill_queries.json").exists()
        else generate_queries(skills)
    )
    targets = {q["target"] for q in qs}
    rng = random.Random(9)
    rest = [s for s in skills if s["name"] not in targets]
    rng.shuffle(rest)
    rows = []
    for n in sizes:
        # 正解スキルは件数に関わらず全部入れたいが、20 件の規模では入りきらない。
        # 小さい規模では正解のうち先頭 n//2 件だけを使い、残りを妨害で埋める。
        tq = qs if n >= len(qs) * 2 else qs[: n // 2]
        must = [s for s in skills if s["name"] in {q["target"] for q in tq}]
        entries = must + rest[: max(0, n - len(must))]
        defs = [
            {
                "name": e["name"],
                "description": e["description"],
                "input_schema": {"type": "object", "properties": {}},
            }
            for e in entries
        ]
        _, search_def = bm25.rank(defs, [], limit=5)
        for condition in ("list_cap50", "list_all", "search", "search_names"):
            with ThreadPoolExecutor(max_workers=4) as pool:
                rows += list(
                    pool.map(
                        lambda q, c=condition, es=entries, sd=search_def: run_one(es, c, q, sd), tq
                    )
                )
            print(f"skills n={n} {condition} done", flush=True)
    (ROOT / "results").mkdir(exist_ok=True)
    (ROOT / "results" / "skills_e2e.json").write_text(
        json.dumps(rows, ensure_ascii=False, indent=1)
    )


if __name__ == "__main__":
    main()
