# skillex（外部クライアント）→ llm-gateway の m2m 認証契約

> 正本: docs/design.md §4.1.1 / §4.5 / docs/requirements.md FR-1.1 / docs/roadmap/parallel-tracks.md（SK トラック）。
> 本書は skillex が shiki の llm-gateway を machine-to-machine で呼ぶための client・トークン契約を示す。

## 経緯

- 当初（Phase 0 Task 0.10）は shiki の Keycloak を skillex と**共有するアイデンティティプール**とし、
  User・サービスアクセス権・請求・管理画面を SaaS で統一する設計だった。
- **2026-09-29 の human 決定でこれを廃止**（運用負荷／shiki と skillex の顧客が重ならない）。
  ユーザー認証は SaaS でも分離し、**残る結合は skillex → llm-gateway の m2m 呼び出しのみ**とした。
  旧設計のリスク整理は design-caveats.md PIT-26〜29（各項に更新注記）。

## 方針（確定）

- **ユーザー認証は分離**: realm `shiki` は shiki ユーザー専用。skillex は SaaS/オンプレとも自前の専用 Keycloak を持つ
  （skillex 側の関心事。shiki の realm へはフェデレートしない）。
- **llm-gateway = 全社共通 LLM ゲートウェイ**。skillex はその最初の外部クライアントで、shiki-server が同一バイナリで公開する
  **外部クライアント向け LLM API**（OpenAI 互換を想定・形は実装タスクで確定）を呼ぶ。
  ⚠️ **未実装**: 現時点で `aud=shiki-llm` のトークンを受け付けるエンドポイントは `crates/` に無い（parallel-tracks SK.8）。
- **SaaS 限定**。オンプレの skillex は自前のローカル vLLM を使い、shiki とは接続しない。

## client / トークン契約

`deploy/keycloak/shiki-realm.json` に定義する `skillex` client が現時点の正本
（`contracts/` 作成時にそちらへ切り出す。`contracts/` は外部 LLM API 仕様＋本契約＋後方互換ポリシのみを持つ）。

- client_id: `skillex`（confidential、`serviceAccountsEnabled=true`）。
  service account は**テナント属性（`tenant` claim）を持たない**。ただしテナント解決は claim だけで決まらない（single テナンシーでは設定値で固定される）ため、外部クライアント API は通常の `AuthContext` テナント解決を**使わず**、外部クライアント用の予約名前空間に固定する（SK.8）。
- 取得方法（machine-to-machine）: OAuth2 client_credentials grant。
  - エンドポイント: `POST {issuer}/protocol/openid-connect/token`
  - パラメータ: `grant_type=client_credentials`, `client_id=skillex`, `client_secret=<secret>`
  - dev secret: `skillex-dev-secret`（本番は環境ごとに発行・ローテーション＝SK.5）。
- トークンの想定クレーム:
  - `aud`: `shiki-llm`（外部クライアント LLM API の audience。audience mapper で付与）。
  - `iss`: `{KC_PUBLIC_URL}/realms/shiki`。
  - `azp`: `skillex`。
- **検証側（shiki）の義務**:
  - 外部クライアント LLM API は `iss`/`aud`/`azp` を**厳密検証**し、登録済み外部クライアントの `azp` のみ受理する。
  - **shiki の通常ユーザー API はこの m2m トークンを拒否**する（confused-deputy 防御・PIT-27）。
    現行の通常ユーザー API は BFF のセッション Cookie のみで **Bearer 入口を持たない**。Bearer JWT を検証する `/admin/*` と
    BFF callback は `auth.audience`（既定 `shiki-api`）を必須にしている（`crates/api/src/middleware/auth.rs`）。
- **会計**: 外部クライアントは `azp` をキーにした別名前空間で計測する。skillex が渡す自社 org id は
  **会計ラベルとしてのみ**扱い、認可根拠にしない。skillex 分は製品間の内部原価精算（顧客への統一請求はしない）。
- ユーザー委譲（skillex のユーザー代理で shiki を叩く）は**想定しない**。必要になったら別途 human 判断とする。

## 検証（Phase 0 受け入れ。CI `ci.yml` の compose smoke は `aud` に `shiki-llm` が含まれることを assert）

`docker compose up` 後、client_credentials でアクセストークンを取得し `aud` を確認する:

```sh
curl -s -X POST http://localhost:8081/realms/shiki/protocol/openid-connect/token \
  -d grant_type=client_credentials \
  -d client_id=skillex \
  -d client_secret=skillex-dev-secret | jq -r .access_token \
  | cut -d. -f2 | base64 -d 2>/dev/null | jq '{aud, iss, azp}'
```

→ `aud` に `shiki-llm` が含まれ、`iss` が `…/realms/shiki`、`azp` が `skillex` であること。
