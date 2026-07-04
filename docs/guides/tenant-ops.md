# テナント運用 Runbook（SAAS.2 / SAAS.5）

SaaS のテナント作成・削除・名前空間移行の運用手順。実装: #87（プロビジョニング/削除）・#89（reconciliation・移行 CLI）。

## 前提

- `auth.provisioner_client_id` / `auth.provisioner_client_secret` が設定されていること
  （未設定なら `/admin/*` はルート自体が存在しない＝fail-closed）。
- provisioner は Keycloak の confidential client（service account 有効・realm-management ロール
  `manage-users` / `manage-realm` / `query-*` 付与・audience `shiki-api`）。dev は realm import 済み。

## テナント作成（1 操作）

```bash
# 1. provisioner トークンを取得（client_credentials）
TOKEN=$(curl -s -X POST "$KEYCLOAK/realms/shiki/protocol/openid-connect/token" \
  -d grant_type=client_credentials \
  -d client_id=shiki-provisioner -d client_secret="$PROVISIONER_SECRET" | jq -r .access_token)

# 2. テナント作成
curl -s -X POST "$SHIKI/admin/tenants" \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{
    "tenant_id": "acme",
    "display_name": "Acme Inc.",
    "admin_email": "admin@acme.example.com"
  }'
# → 201 { tenant_id, org, status, admin_user_id, temp_password }
```

- `temp_password` は**新規作成時のみ一度だけ**返る（サーバに保存されない）。初回ログインで変更必須。
- 冪等: 同一リクエストの再実行は既存を返す（password は null）。
- `tenant_id` は FGA/オブジェクトキーの名前空間になるため `| : # @` 空白は不可。
- 削除済み（tombstone）の tenant_id は再利用**不可**（名前空間衝突防止）。別 id を使う。

## テナント削除（破壊的・不可逆）

```bash
curl -s -X DELETE "$SHIKI/admin/tenants/acme" -H "Authorization: Bearer $TOKEN"
# → 204
```

撤去順（全段冪等・途中失敗は同一リクエスト再実行で収束）:
1. tenant 行を `deleting` へ
2. セッション全失効（Redis）
3. Keycloak: `tenant` 属性一致ユーザーと org グループを削除
4. データ purge: FGA タプル（node/role/org）→ オブジェクト `{tenant}/` prefix → DB 行（1 txn・FK 順）
5. tenant 行を `deleted`（tombstone）へ

**audit_log は削除証跡として保持**される（`tenant.purge` エントリがチェーンに残る）。
完全消去（GDPR 型）が要件になったら別途手当てする。

## role/部署メンバーシップの reconciliation

ログイン時に IdP claims（`roles` ＋ `groups`＝AD 部署）と FGA の直接 role タプルを **diff 同期**する。
部署異動・ロール剥奪は**次回ログインで自動反映**（それまではセッション TTL 内は旧権限が残る点に注意。
即時失効が必要ならセッション削除を併用）。role タプルの正は IdP claims — **FGA へ手動で role
member タプルを足しても次回ログインで剥がれる**。本番の SCIM/グループ完全同期は SK.6。

## テナント名前空間の移行（shiki-admin retenant）

**メンテナンスウィンドウ中**（対象テナントの書込停止）に実行する。既定は dry-run。

```bash
# SAAS.1 以前の旧無印識別子 → 名前空間形式（P1 フォロー）
shiki-admin retenant --legacy --to default            # dry-run（件数レポート）
shiki-admin retenant --legacy --to default --execute

# cell→pool 移行（SAAS.5）: tenant_id リネーム
shiki-admin retenant --from default --to acme
shiki-admin retenant --from default --to acme --execute
```

- 対象: FGA タプル（node/role/org・subject 込み）・オブジェクトキー（copy→delete）・
  DB 全テーブル（rename 時。audit_log/tenant 行含む）・セッション（rename 時に失効）。
- 冪等: 再実行はコピー済み/削除済み/移行済みをスキップして収束する。
- 他テナントの識別子には触れない（移行元名前空間に属さないタプルは skipped として報告）。
