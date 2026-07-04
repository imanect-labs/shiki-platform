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
- `accessTokenLifespan` は失効遅延（#91 H-1 参照）とのトレードオフで決める。
