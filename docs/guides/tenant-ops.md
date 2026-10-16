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

- `temp_password` は新規作成時に返る（サーバに保存されない）。初回ログインで変更必須。
- 冪等: 同一リクエストの再実行は既存を返す。初期 admin が**初回ログイン前**（`UPDATE_PASSWORD` 未消化）なら
  一時パスワードを再発行して返し、ログイン済みなら password は null（#91 M-6）。username が別テナントの
  既存ユーザーと衝突した場合は乗っ取らずに拒否する（別の `admin_username` を指定する）。
- `tenant_id` と `org` は FGA/オブジェクトキーの名前空間になるため、`|` `:` `#` `@` `/`・空白・制御文字・
  空文字・予約名 `.` / `..` は不可（正本は `authz::validate_tenant_id`＝`crates/authz/src/ident.rs`）。
- 削除済み（tombstone）の tenant_id は再利用**不可**（名前空間衝突防止）。別 id を使う。
- ⚠️ **実 Keycloak での初期 admin ログインは未検証**（#630）。自動テストは Keycloak をモックしており、
  realm（`deploy/keycloak/shiki-realm.json`）には User Profile の設定が無い。Admin REST で書く
  `attributes.tenant` が Keycloak 26.2 で保持されないと `tenant` claim が付かず、multi モードでは
  ログインが拒否される。作成後、Keycloak 上で初期 admin に `tenant` 属性が入っていることを確認すること。

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
- ⚠️ この変更（と `PUT /admin/tenants/{id}/autonomous-policy`）は**現状 audit_log に残らない**（#460）。
  変更の記録が要る場合は運用側で別途残す。

## テナント削除（破壊的・不可逆）

```bash
curl -s -X DELETE "$SHIKI/admin/tenants/acme" -H "Authorization: Bearer $TOKEN"
# → 204
```

撤去順（全段冪等・途中失敗は同一リクエスト再実行で収束）:
1. tenant 行を `deleting` へ
2. セッション全失効（Redis）
3. Keycloak: `tenant` 属性一致ユーザーを削除。org グループは**他の未削除テナントが同じ org slug を
   使っていない場合のみ**削除する。その後、競合で作られたセッションをもう一度失効させる
4. データ purge（順序が重要・#420）:
   1. 構造化データ: data_table の物理インデックスと FGA タプル → data_table 行
   2. storage: FGA タプル（node〔file/folder〕・role・organization・artifact・secret・thread）→
      オブジェクト `{tenant}/` と `miniapp-bundle/{tenant}/` prefix → DB 行（1 txn・FK 順）
   3. RAG: Qdrant／Tantivy の索引（outbox とジョブ行を消した後に回し、索引の再生成を防ぐ）
5. tenant 行を `deleted`（tombstone）へ

> ⚠️ レコード個別共有の `data_record:<tenant>|<id>` タプルは**回収されず OpenFGA に残る**（#718）。
> tombstone で同じ tenant_id は再利用できないため到達はしないが、user id を含むタプルが残る。

DB 行の対象は pg_catalog（`pg_class`／`pg_attribute`）から `tenant_id` 列を持つ実テーブルを**実行時に導出**し、FK 依存の
子→親順で削除する（`storage::tenant_scope`・#420）。`information_schema` は現在のロールに権限がある列しか
返さず、制限ロールで動かすと対象が黙って縮むため使わない。保持するのは `audit_log`（削除証跡）と `tenant`
（tombstone）だけで、それ以外は自動的に対象になる —— 新しいテーブルが増えても消し忘れない。
撤去したテーブル数と行数は `tenant.purge` 監査の `tables_purged` / `rows_deleted_direct` に残る。
`rows_deleted_direct` は各 `DELETE` が**直接**消した行数で、`tenant_id` を持たず親から
`ON DELETE CASCADE` される従属行（`generation_event` / `collab_update` 等）は含まない
——消滅証明に使う際はこの定義に注意する。

> ⚠️ 2026-08 以前はテーブル名を手で列挙しており、**51 テーブル中 13 しか消していなかった**。
> チャット履歴・RAG の本文・構造化データ・ワークフロー履歴・利用量が撤去後も残っていた（#420）。

**audit_log は削除証跡として保持**される（`tenant.purge` エントリがチェーンに残る）。
完全消去（GDPR 型）が要件になったら別途手当てする。

## role/部署メンバーシップの reconciliation

ログイン時に IdP claims（`roles` ＋ `groups`＝AD 部署）と FGA の直接 role タプルを **diff 同期**する。
部署異動・ロール剥奪は**次回ログインで自動反映**（それまではセッション TTL 内は旧権限が残る点に注意。
即時失効が必要ならセッション削除を併用）。role タプルの正は IdP claims — **FGA へ手動で role
member タプルを足しても次回ログインで剥がれる**。本番の SCIM/グループ完全同期は SAAS.6。

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
  「DB 全テーブル」は pg_catalog から `tenant_id` 列を持つ実テーブルを**実行時に導出**する
  （`storage::tenant_scope`・#420。理由は上の「テナント削除」と同じ）。テーブル名を手で列挙していた頃は 51 中 11 しか移行せず、
  チャット履歴・RAG 本文・構造化データ・ワークフロー履歴・利用量が旧テナントに取り残されていた。
  dry-run は**行のある全テーブルの件数**を出すので、実行前に対象範囲を必ず確認すること。
- 冪等: 再実行はコピー済み/削除済み/移行済みをスキップして収束する。
- **実行者の記録**: 監査の `actor` 列は固定値 **`cli`**（＝CLI 経由の管理操作）。`--actor <id>`
  （省略時は `$SUDO_USER` → `$USER`）は**認証されていない自己申告**なので actor 列には入れず、
  `tenant.retenant` の metadata に `actor_claimed`（申告 ID）と `actor_source`
  （`flag` / `sudo_user` / `user` / `none`）として残る。**実行者を調べるときは actor 列ではなく
  metadata を見ること**。CLI は DB/FGA/S3 の資格情報を直接持つため OpenFGA 認可は境界にならないが、
  誰が実行したと主張したかは追えるようにしている（#420）。
- **副産物を移送できないテナントは拒否**する（fail-closed・#420・rename 時のみ）。移送できるのは
  `storage::tenant_scope::RETENANT_SIDECAR_COMPLETE` に挙がったテーブル（node 系・pending_upload・blob・共有リンク・
  outbox・directory・audit_log・tenant）だけで、それ以外（thread/message などのチャット履歴・artifact・secret・
  構造化データ・RAG・ワークフロー・ミニアプリ・llm_usage 等）に 1 行でもあると、FGA タプル・索引・
  バンドル実体が旧テナントに残り不整合になるため、dry-run の時点で止まる。FGA の移送も node/role/org のみ。
- ⚠️ **このため cell→pool 移行は、チャットやアーティファクトを一度でも使ったテナントでは実質使えない**（#717）。
  該当サブシステムの副産物移行が入るまで、移行できるのはドライブ（ファイル・フォルダ）と共有リンクだけを使ったテナントに限られる。
- 他テナントの識別子には触れない（移行元名前空間に属さないタプルは skipped として報告）。
- **監査チェーンの注意**: audit_log の `entry_hash` は tenant_id を含んで計算されるため、
  リネーム**以前**の chained エントリは**旧 tenant_id で検証**する必要がある。CLI は
  `tenant.retenant` エントリ（from/to 込み）を新テナントの chain へアンカーとして記録する。
- **IdP の tenant 属性**: リネーム後、Keycloak ユーザーの `attributes.tenant` を CLI が
  自動追従する（provisioner 設定時）。未設定の場合は**手動で更新するまで新ログインが
  旧テナント名前空間に落ちる**ため、必ず更新すること。
