# Keycloak realm（dev 専用）

`shiki-realm.json` は **開発・CI 専用の realm fixture** です。**本番環境にそのまま import してはいけません**（#91 L-5）。

## 本番使用が禁止な理由

この fixture は開発利便のために意図的に緩い設定を含みます:

- `"sslRequired": "none"` — 平文 HTTP を許可。本番は `"external"` 以上必須。
- 平文の client secret（`shiki-web-dev-secret` / `skillex-dev-secret` /
  `shiki-provisioner-dev-secret`）— そのまま使うと **provisioner service account 経由で
  `/admin/tenants` のテナント作成/削除まで奪取され得る**。
- redirect URI / `webOrigins` にワイルドカード（`/*` / `+`）— トークン奪取面が広い。
- ユーザーパスワードの直書き。

## 本番 realm の要件

- realm は別管理（IaC / Keycloak 管理 API）で構築し、この JSON を流用しない。
- client secret は Vault 等のシークレットストアから注入（リポジトリに置かない）。
- `sslRequired = external`、redirect URI / `webOrigins` を実ホストに限定。
- provisioner client は confidential・最小権限（realm 管理の必要スコープのみ）。
- `accessTokenLifespan` は失効遅延とのトレードオフで決める。

## Back-Channel Logout（#91）

`shiki-web` client には OIDC Back-Channel Logout を設定済み（dev fixture）:

- `backchannel.logout.url` = `http://localhost:10067/auth/backchannel-logout`（BFF の受け口）。
- `backchannel.logout.session.required` = `true`（logout_token に `sid` を載せ、当該 SSO
  セッションのみ失効させる）。

これにより、管理者が Keycloak でユーザーを**無効化/削除**すると Keycloak が BFF へ
`logout_token` を POST し、`accessTokenLifespan`（dev は 1800s）を待たずにサーバ側セッションが
即時失効する。BFF は logout_token の署名・iss・aud（= client_id）・logout イベント宣言・
`nonce` 不在を検証してから失効する（`crates/api/src/routes/auth/backchannel_logout.rs`）。

**本番では** `backchannel.logout.url` を実ホスト（BFF の公開 URL）へ差し替えること。
BFF は Keycloak から到達可能である必要がある（同一ネットワーク or 到達可能な URL）。
