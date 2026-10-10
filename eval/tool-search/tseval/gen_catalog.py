"""合成ツールカタログの生成（業務 SaaS の MCP 風ツール群・日英の説明つき）。

実ツールだけでは数十件しか無く「ツールが増えたとき」を測れない。現実に起こる増え方
（コネクタ・MCP サーバの追加）に合わせ、サービス単位でツール群を作る。説明は日英の
両方を持たせ、カタログ言語 × クエリ言語の組み合わせを測れるようにする。

出力: data/catalog_synth.json（[{name, service, description_ja, description_en, params}]）
"""

from __future__ import annotations

import json
import re
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from .llm import chat_json

DATA = Path(__file__).resolve().parent.parent / "data"
CACHE = DATA / "cache" / "catalog"

# (識別子, 表示名, 分野)。日本の業務で実際に併用される組み合わせを意識して選ぶ。
SERVICES: list[tuple[str, str, str]] = [
    ("github", "GitHub", "ソースコード管理・Issue・PR"),
    ("gitlab", "GitLab", "ソースコード管理・CI"),
    ("jira", "Jira", "課題管理・スプリント"),
    ("confluence", "Confluence", "社内 Wiki"),
    ("backlog", "Backlog", "プロジェクト管理・課題"),
    ("slack", "Slack", "チャット・チャンネル"),
    ("teams", "Microsoft Teams", "チャット・会議"),
    ("chatwork", "Chatwork", "チャット・タスク"),
    ("lineworks", "LINE WORKS", "社内チャット・掲示板"),
    ("gmail", "Gmail", "メール"),
    ("outlook", "Outlook", "メール・予定表"),
    ("gcal", "Google カレンダー", "予定・会議室"),
    ("gdrive", "Google ドライブ", "ファイル共有"),
    ("box", "Box", "ファイル共有・承認"),
    ("dropbox", "Dropbox", "ファイル共有"),
    ("notion", "Notion", "ドキュメント・データベース"),
    ("asana", "Asana", "タスク管理"),
    ("trello", "Trello", "カンバン"),
    ("linear", "Linear", "課題管理"),
    ("salesforce", "Salesforce", "CRM・商談"),
    ("hubspot", "HubSpot", "マーケティング・CRM"),
    ("kintone", "kintone", "業務アプリ・レコード"),
    ("garoon", "Garoon", "グループウェア・ワークフロー"),
    ("freee", "freee 会計", "会計・仕訳・請求書"),
    ("moneyforward", "マネーフォワード クラウド", "経費・請求"),
    ("rakurakuseisan", "楽楽精算", "経費精算"),
    ("smarthr", "SmartHR", "人事労務・従業員情報"),
    ("jobcan", "ジョブカン", "勤怠管理"),
    ("cloudsign", "クラウドサイン", "電子契約"),
    ("docusign", "DocuSign", "電子署名"),
    ("zendesk", "Zendesk", "カスタマーサポート・チケット"),
    ("servicenow", "ServiceNow", "IT サービス管理"),
    ("datadog", "Datadog", "監視・メトリクス"),
    ("sentry", "Sentry", "エラー監視"),
    ("pagerduty", "PagerDuty", "インシデント対応・オンコール"),
    ("aws", "AWS", "クラウド基盤（EC2・S3・CloudWatch）"),
    ("gcp", "Google Cloud", "クラウド基盤（GCE・GCS・BigQuery）"),
    ("stripe", "Stripe", "決済・サブスクリプション"),
    ("shopify", "Shopify", "EC・注文・在庫"),
    ("zoom", "Zoom", "Web 会議・録画"),
    ("figma", "Figma", "デザイン・コメント"),
    ("miro", "Miro", "ホワイトボード"),
    ("ga4", "Google アナリティクス", "アクセス解析"),
    ("bigquery", "BigQuery", "データウェアハウス・SQL"),
    ("tableau", "Tableau", "BI・ダッシュボード"),
]

PROMPT = """あなたは MCP サーバのツール定義を設計するエンジニアです。
サービス「{display}」（分野: {domain}）の MCP サーバが公開するツールを {count} 個、JSON 配列で返してください。

要件:
- 実在の MCP サーバ・公式 API にありそうな粒度と命名にする。読み取り・作成・更新・削除・検索・一覧などを偏りなく含める。
- name は `{svc}_` で始まる snake_case（例: `{svc}_list_items`）。英小文字・数字・_ のみ。重複させない。
- description_ja / description_en はそれぞれ 1〜3 文。同じ内容を日本語と英語で書く。何をするか・いつ使うかが分かるように。
- params は 1〜5 個。各要素は {{"name", "type", "required", "description_ja", "description_en"}}。type は string/integer/boolean/array/object のいずれか。
- 同じサービス内で紛らわしい近縁ツール（例: 一覧と検索、下書き作成と送信）を最低 2 組含める。

出力は次の形の JSON 配列だけ:
[{{"name": "...", "description_ja": "...", "description_en": "...", "params": [...]}}]
"""

_NAME = re.compile(r"^[a-z0-9_]{3,64}$")


def _valid(svc: str, t: dict) -> bool:
    return (
        isinstance(t, dict)
        and isinstance(t.get("name"), str)
        and _NAME.match(t["name"]) is not None
        and t["name"].startswith(f"{svc}_")
        and all(
            isinstance(t.get(k), str) and t[k].strip() for k in ("description_ja", "description_en")
        )
        and isinstance(t.get("params"), list)
    )


def generate_service(svc: str, display: str, domain: str, count: int = 22) -> list[dict]:
    path = CACHE / f"{svc}.json"
    if path.exists():
        return json.loads(path.read_text())
    raw = chat_json(
        PROMPT.format(display=display, domain=domain, count=count, svc=svc), temperature=0.7
    )
    tools = [t for t in raw if _valid(svc, t)] if isinstance(raw, list) else []
    seen: set[str] = set()
    out = []
    for t in tools:
        if t["name"] in seen:
            continue
        seen.add(t["name"])
        out.append(
            {
                "name": t["name"],
                "service": svc,
                "description_ja": t["description_ja"],
                "description_en": t["description_en"],
                "params": t["params"],
            }
        )
    CACHE.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out, ensure_ascii=False, indent=1))
    return out


def main() -> None:
    with ThreadPoolExecutor(max_workers=4) as pool:
        results = list(pool.map(lambda s: generate_service(*s), SERVICES))
    catalog = [t for r in results for t in r]
    (DATA / "catalog_synth.json").write_text(json.dumps(catalog, ensure_ascii=False, indent=1))
    print(f"{len(catalog)} tools from {len(SERVICES)} services")


if __name__ == "__main__":
    main()
