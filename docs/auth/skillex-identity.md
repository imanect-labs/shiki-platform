# shiki × skillex アイデンティティ連携設計（Phase 0）

> 正本: docs/design.md §4.1.1 / docs/roadmap/parallel-tracks.md（SK トラック）。
> 本書は Phase 0 時点の方針確定と、skillex 用 client・トークンの契約を示す。

## 方針（確定）

- **統一は SaaS 版限定**。オンプレは shiki・skillex とも認証基盤を切り離して単独運用する
  （外部依存ゼロ）。本連携は SaaS 共有コントロールプレーンでのみ有効。
- **shiki Keycloak = 共有アイデンティティプール**。skillex はこの realm（`shiki`）へ
  フェデレートし、**User は統一**。サービスへの入場券と管理者バッジも統一。
- **認可（館内ルール）は各サービスが保持・分離**。skillex の DLC/訓練権限・設定は skillex 側、
  shiki の ReBAC・部署・設定は shiki 側。Keycloak が持つのは「利用可否＋サービス管理者か」の
  粗い粒度のみ。
- **利用量計測は分離（集約値のみ請求へ）／請求は統一（Org 単位 1 請求・サービス別内訳）**。
- shiki-server の AuthN 向き先は設定で差し替え（SaaS=共有 issuer ⇔ オンプレ=ローカル Keycloak）。
  本リポジトリの `crates/api` 設定 `auth.issuer` / `auth.jwks_uri` がその継ぎ目。

## 3 層境界

1. **User = 統一**（Keycloak の同一ユーザー）。
2. **サービスへの入場券 ＋ 管理者バッジ = 統一**（粗いサービスロール）。
3. **館内ルール（細かい認可 / 設定）= 分離**（各サービス内：shiki=ReBAC、skillex=DLC 権限）。

## skillex 用 client / トークン契約（Phase 0）

`deploy/keycloak/shiki-realm.json` に定義する skillex client が契約の正本（SaaS 版では
shiki repo `contracts/` に切り出す。Phase 0 は realm export を正本とする）。

- client_id: `skillex`（confidential、`serviceAccountsEnabled=true`）。
- 取得方法（machine-to-machine）: OAuth2 client_credentials grant。
  - エンドポイント: `POST {issuer}/protocol/openid-connect/token`
  - パラメータ: `grant_type=client_credentials`, `client_id=skillex`, `client_secret=<secret>`
  - Phase 0 の dev secret: `skillex-dev-secret`（本番は環境ごとに発行・ローテーション）。
- トークンの想定クレーム:
  - `aud`: `shiki-llm`（DLC/LLM 利用の対象 audience。audience mapper で付与）。
  - `iss`: `{KC_PUBLIC_URL}/realms/shiki`。
  - `azp`: `skillex`。
- ユーザー委譲が要る将来のフロー（skillex UI からユーザー代理で shiki LLM を叩く等）は
  authorization code + PKCE、または token-exchange を後続フェーズで追加する
  （Phase 9 の app-gateway 二重ゲートと同じ confused-deputy 防御方針に揃える）。

## skillex 側をブロックしないこと

- skillex は並行進行中。本設計は **realm に client を 1 つ足すだけ**で、skillex 側の実装に
  前提を強制しない（client_credentials での疎通確認まで shiki 側で完結）。
- skillex 側は issuer / token エンドポイント / `aud=shiki-llm` のみを前提にすればよい。

## 検証（Phase 0 受け入れ）

`docker compose up` 後、client_credentials でアクセストークンを取得し `aud` を確認する:

```sh
curl -s -X POST http://localhost:8081/realms/shiki/protocol/openid-connect/token \
  -d grant_type=client_credentials \
  -d client_id=skillex \
  -d client_secret=skillex-dev-secret | jq -r .access_token \
  | cut -d. -f2 | base64 -d 2>/dev/null | jq '{aud, iss, azp}'
```

→ `aud` に `shiki-llm` が含まれ、`iss` が `…/realms/shiki`、`azp` が `skillex` であること。
