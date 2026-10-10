"""評価カタログ（実ツールのスナップショット ＋ 合成ツール）の読み込みと ToolDef への変換。"""

from __future__ import annotations

import json
import random
from dataclasses import dataclass, field
from pathlib import Path

DATA = Path(__file__).resolve().parent.parent / "data"


@dataclass(frozen=True)
class Tool:
    name: str
    service: str
    description_ja: str
    description_en: str
    params: list[dict] = field(default_factory=list, hash=False, compare=False)
    # 実ツールは製品の input_schema をそのまま持つ（合成は params から組む）。
    schema: dict | None = field(default=None, hash=False, compare=False)

    def tooldef(self, lang: str = "ja") -> dict:
        """順位 CLI に渡す ToolDef（中立形）。実ツールは英語の説明を持たないので常に日本語。"""
        desc = self.description_en if lang == "en" and self.description_en else self.description_ja
        if self.schema is not None:
            schema = self.schema
        else:
            props = {
                p["name"]: {
                    "type": p.get("type", "string"),
                    "description": p.get(f"description_{lang}") or p.get("description_ja", ""),
                }
                for p in self.params
                if isinstance(p, dict) and p.get("name")
            }
            req = [p["name"] for p in self.params if isinstance(p, dict) and p.get("required")]
            schema = {"type": "object", "properties": props, "required": req}
        return {"name": self.name, "description": desc, "input_schema": schema}

    def doc_text(self, lang: str = "ja") -> str:
        """埋め込み・reranker に渡す文書表現（名前・説明・引数）。"""
        d = self.tooldef(lang)
        params = d["input_schema"].get("properties", {})
        args = "、".join(f"{k}（{v.get('description', '')}）" for k, v in params.items())
        return f"{d['name']}: {d['description']}" + (f"\n引数: {args}" if args else "")


def load_real() -> list[Tool]:
    """製品の遅延ツール（dev 構成の通常チャットで遅延になった集合のスナップショット）。"""
    raw = json.loads((DATA / "catalog_real_snapshot.json").read_text())
    return [
        Tool(
            name=t["name"],
            service="shiki",
            description_ja=t["description"],
            description_en="",
            schema=t["input_schema"],
        )
        for t in raw
        if t.get("defer_loading")
    ]


def load_synth() -> list[Tool]:
    raw = json.loads((DATA / "catalog_synth.json").read_text())
    return [
        Tool(
            name=t["name"],
            service=t["service"],
            description_ja=t["description_ja"],
            description_en=t["description_en"],
            params=t["params"],
        )
        for t in raw
    ]


def load_catalog() -> list[Tool]:
    return load_real() + load_synth()


def growth_order(catalog: list[Tool], seed: int = 11) -> list[Tool]:
    """カタログが**サービス単位で**増えていく順（実ツールが先頭・以降はサービスをランダム順に足す）。

    現実のカタログはコネクタ（MCP サーバ）単位で増える。規模 N のカタログはこの列の先頭 N 件で、
    評価するのは正解ツールがその中に居るクエリだけ（規模が大きいほど評価クエリも増える）。
    """
    rng = random.Random(seed)
    real = [t for t in catalog if t.service == "shiki"]
    services = sorted({t.service for t in catalog if t.service != "shiki"})
    rng.shuffle(services)
    out = list(real)
    for s in services:
        tools = [t for t in catalog if t.service == s]
        rng.shuffle(tools)
        out.extend(tools)
    return out


def fixed_target_subsets(
    catalog: list[Tool], targets: set[str], sizes: list[int], seed: int = 13
) -> dict[int, list[Tool]]:
    """正解ツールを全部含めたまま、**妨害ツールだけ**を増やす入れ子の部分集合。

    評価クエリを固定して「候補が増えたぶんの劣化」だけを取り出す（growth_order は
    評価クエリの集合も規模と一緒に動くので、こちらで切り分ける）。
    """
    rng = random.Random(seed)
    must = [t for t in catalog if t.name in targets]
    rest = [t for t in catalog if t.name not in targets]
    rng.shuffle(rest)
    return {n: must + rest[: max(0, n - len(must))] for n in sizes}


# サービス名の表記（依頼文がサービスを名指ししているかの判定に使う）。
_ALIASES: dict[str, list[str]] = {
    "github": ["GitHub"],
    "gitlab": ["GitLab"],
    "jira": ["Jira"],
    "confluence": ["Confluence"],
    "backlog": ["Backlog"],
    "slack": ["Slack"],
    "teams": ["Teams"],
    "chatwork": ["Chatwork"],
    "lineworks": ["LINE WORKS", "LINEWORKS"],
    "gmail": ["Gmail"],
    "outlook": ["Outlook"],
    "gcal": ["Google カレンダー", "Googleカレンダー", "Google Calendar"],
    "gdrive": ["Google ドライブ", "Googleドライブ", "Google Drive"],
    "box": ["Box"],
    "dropbox": ["Dropbox"],
    "notion": ["Notion"],
    "asana": ["Asana"],
    "trello": ["Trello"],
    "linear": ["Linear"],
    "salesforce": ["Salesforce"],
    "hubspot": ["HubSpot"],
    "kintone": ["kintone"],
    "garoon": ["Garoon", "ガルーン"],
    "freee": ["freee"],
    "moneyforward": ["マネーフォワード", "Money Forward", "MoneyForward"],
    "rakurakuseisan": ["楽楽精算", "Rakuraku"],
    "smarthr": ["SmartHR"],
    "jobcan": ["ジョブカン", "Jobcan"],
    "cloudsign": ["クラウドサイン", "CloudSign"],
    "docusign": ["DocuSign"],
    "zendesk": ["Zendesk"],
    "servicenow": ["ServiceNow"],
    "datadog": ["Datadog"],
    "sentry": ["Sentry"],
    "pagerduty": ["PagerDuty"],
    "aws": ["AWS"],
    "gcp": ["Google Cloud", "GCP"],
    "stripe": ["Stripe"],
    "shopify": ["Shopify"],
    "zoom": ["Zoom"],
    "figma": ["Figma"],
    "miro": ["Miro"],
    "ga4": ["Google アナリティクス", "Googleアナリティクス", "Google Analytics", "GA4"],
    "bigquery": ["BigQuery"],
    "tableau": ["Tableau"],
}


def names_service(text: str, service: str) -> bool:
    """依頼文がそのサービスを名指ししているか（製品の実ツールはサービスを持たないので常に真）。"""
    if service == "shiki":
        return True
    low = text.lower()
    return any(a.lower() in low for a in _ALIASES.get(service, []))
