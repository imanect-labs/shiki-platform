# テナント運用 Runbook（SAAS.2 / SAAS.5）

SaaS のテナント作成・削除・名前空間移行の運用手順。実装: #87（プロビジョニング/削除）・#89（reconciliation・移行 CLI）。

## 前提

- `auth.provisioner_client_id` / `auth.provisioner_client_secret` が設定されていること
  （未設定なら `/admin/*` はルート自体が存在しない＝fail-closed）。
- `auth.tenancy=multi` であること（`SHIKI__AUTH__TENANCY=multi`）。single のままテナント作成を呼ぶと
  409 `tenancy_single` で拒否される（single は全ユーザーを固定テナントに解決するため、作ったテナントに誰も入れない・#808）。
  single＋provisioner 設定時は起動ログに warn が出る。
- provisioner は Keycloak の confidential client（service account 有効・realm-management ロール
  `manage-users` / `manage-realm` / `query-*` 付与・audience `shiki-api`）。dev は realm import 済み。
- realm の User Profile で `tenant` / `roles` を**宣言済み・管理者のみ閲覧/編集**であること
  （`deploy/keycloak/README.md`「User Profile」・#809）。宣言が無いと Keycloak 26 は admin REST 経由の
  `tenant` も保存せず、作成したテナントの初期 admin がログインできない（プロビジョニングが黙って効かない）。

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
- 初期管理者には org の `member`・`admin`（管理コンソール）・`audit_viewer`（監査ログ・#634）が付く。
  `audit_viewer` 導入前に作ったテナントは、同じリクエストを再実行するか、下の `shiki-admin audit-viewer grant`
  で付与するまで、誰も監査ログを読めない。

## 監査閲覧者の付与・剥奪（shiki-admin audit-viewer）

監査ログ（管理コンソール › 監査ログ・`/org-audit/*`）を読める人（`organization#audit_viewer`）を付ける・外す。
admin とは独立なので、管理者でない監査担当者もここで付与する（メンバー管理の画面ができるまでの運用経路）。
即時実行（dry-run は無い）。

```bash
shiki-admin audit-viewer grant  --tenant acme --org acme --user <ユーザーの sub>
shiki-admin audit-viewer revoke --tenant acme --org acme --user <ユーザーの sub>
```

- 付与・剥奪は監査に `org_audit.viewer.grant` / `org_audit.viewer.revoke` として残り、ハッシュチェーンに連結する。
  記録できなければ FGA の変更を戻す。
- 監査の `actor` 列は固定値 `cli`。`--actor <id>`（省略時は `$SUDO_USER` → `$USER`）は自己申告として
  metadata の `actor_claimed` / `actor_source` に残る（retenant と同じ扱い）。

## ワークフロー実行履歴の保持期間

```bash
curl -s -X PUT "$SHIKI/admin/tenants/acme/workflow-retention" \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"retention_days": 30}'
# → 204
```

- 既定 90 日・範囲 1〜3650 日（範囲外は 400）。active でないテナントは 404。
- 起算は **run が terminal になった時刻**。実行中の run は保持期間を跨いでも消えない。
- 期限切れの run と、その step/イベント/wait 購読・副作用 journal を日次 GC が消す（不可逆）。
- **短くする方向は次回の日次 GC で効く。** 監査要件で履歴が要るなら縮める前に退避する。
- GC の状態確認・詰まったときの調べ方は [db-maintenance.md](./db-maintenance.md) §3。

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

DB 行の対象は `information_schema` から `tenant_id` 列を持つ実テーブルを**実行時に導出**し、FK 依存の
子→親順で削除する（`storage::tenant_scope`・#420）。保持するのは `audit_log`（削除証跡）と `tenant`
（tombstone）だけで、それ以外は自動的に対象になる —— 新しいテーブルが増えても消し忘れない。
撤去したテーブル数と行数は `tenant.purge` 監査の `tables_purged` / `rows_deleted_direct` に残る。
`rows_deleted_direct` は各 `DELETE` が**直接**消した行数。#815（migration 0080）以降は
`generation_event` / `collab_update` / `app_function_run` も `tenant_id` を持ち、子→親順で直接消されて
この数に含まれる（`tenant_id` を持たない表は全体共有の `maintenance_schedule` / `outbox_consumer` /
`scheduler_lease` だけ）。0080 適用前の環境では、これらは親から `ON DELETE CASCADE` で消え、
数に含まれない——消滅証明に使う際はこの定義に注意する。

> ⚠️ 2026-08 以前はテーブル名を手で列挙しており、**51 テーブル中 13 しか消していなかった**。
> チャット履歴・RAG の本文・構造化データ・ワークフロー履歴・利用量が撤去後も残っていた（#420）。

**audit_log は削除証跡として保持**される（`tenant.purge` エントリがチェーンに残る）。
完全消去（GDPR 型）が要件になったら別途手当てする。

## role/部署メンバーシップの reconciliation

ログイン時に IdP claims（`roles` ＋ `groups`＝AD 部署）と FGA の直接 role タプルを **diff 同期**する。
部署異動・ロール剥奪は**次回ログインで自動反映**（それまではセッション TTL 内は旧権限が残る点に注意。
即時失効が必要ならセッション削除を併用）。role タプルの正は IdP claims — **FGA へ手動で role
member タプルを足しても次回ログインで剥がれる**。本番の SCIM/グループ完全同期は SAAS.6。

## シークレット暗号文の行束縛への移行（shiki-admin secrets-reencrypt）

#814 以降、シークレットの暗号文は AAD で行（`tenant_id`・`org`・`secret_id`）に束縛される（`enc_version=1`）。
それ以前に作られた行（`enc_version=0`）は読み出し時に遅延で移行されるが、**使われない行は旧版のまま残り、
暗号素材を別の行へ複製すると復号できてしまう**。デプロイ後に一度、明示的に移行する。

```bash
shiki-admin secrets-reencrypt                     # dry-run（旧版の件数だけ表示）
shiki-admin secrets-reencrypt --execute           # 行単位に再暗号化（DEK 再利用・冪等・稼働中に実行可）
shiki-admin secrets-reencrypt --execute --batch 500
```

- 復号できない行は書き換えずに残し、件数を表示して**非 0 で終了**する（改竄・鍵不一致の疑い。調査対象）。
- 新版で書かれた行は旧バイナリでは復号できない。**移行後のバイナリのロールバックは不可**と考えること。
- **旧バイナリと新バイナリを並走させない**（ローリング更新しない・停止して入れ替える）。旧バイナリの rotate は
  `enc_version` を書かないため、新バイナリが移行した行を旧バイナリが書き直すと復号不能な行ができる。
- バックアップ内の旧版暗号文は引き続き旧形式で有効（保持期限の経過で消える）。

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
  「DB 全テーブル」は `information_schema` から `tenant_id` 列を持つ実テーブルを**実行時に導出**する
  （`storage::tenant_scope`・#420）。テーブル名を手で列挙していた頃は 51 中 11 しか移行せず、
  チャット履歴・RAG 本文・構造化データ・ワークフロー履歴・利用量が旧テナントに取り残されていた。
  dry-run は**行のある全テーブルの件数**を出すので、実行前に対象範囲を必ず確認すること。
- 冪等: 再実行はコピー済み/削除済み/移行済みをスキップして収束する。
- **実行者の記録**: 監査の `actor` 列は固定値 **`cli`**（＝CLI 経由の管理操作）。`--actor <id>`
  （省略時は `$SUDO_USER` → `$USER`）は**認証されていない自己申告**なので actor 列には入れず、
  `tenant.retenant` の metadata に `actor_claimed`（申告 ID）と `actor_source`
  （`flag` / `sudo_user` / `user` / `none`）として残る。**実行者を調べるときは actor 列ではなく
  metadata を見ること**。CLI は DB/FGA/S3 の資格情報を直接持つため OpenFGA 認可は境界にならないが、
  誰が実行したと主張したかは追えるようにしている（#420）。
- **副産物を移送できないテナントは拒否**する（fail-closed・#420）。artifact/secret/構造化データ/
  RAG/ワークフロー/ミニアプリの行があると、FGA タプル・索引・バンドル実体が旧テナントに残り
  不整合になるため、dry-run の時点で止まる。該当サブシステムの移行実装が入るまで移行できない。
- 他テナントの識別子には触れない（移行元名前空間に属さないタプルは skipped として報告）。
- **監査チェーンの注意**: audit_log の `entry_hash` は tenant_id を含んで計算されるため、
  リネーム**以前**の chained エントリは**旧 tenant_id で検証**する必要がある。CLI は
  `tenant.retenant` エントリ（from/to 込み）を新テナントの chain へアンカーとして記録する。
- **IdP の tenant 属性**: リネーム後、Keycloak ユーザーの `attributes.tenant` を CLI が
  自動追従する（provisioner 設定時）。未設定の場合は**手動で更新するまで新ログインが
  旧テナント名前空間に落ちる**ため、必ず更新すること。
