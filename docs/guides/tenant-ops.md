# テナント運用 Runbook（SAAS.2 / SAAS.5）

SaaS のテナント作成・削除・名前空間移行の運用手順。実装: #87（プロビジョニング/削除）・#89（reconciliation・移行 CLI）・
#811（org group のテナント修飾）。

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
- Keycloak の org group は **`/tenants/<tenant_id>/<org>`** に作られ、初期 admin はそこへ入る（#811）。
  realm は全テナント共用なので、group をテナントで修飾して同じ org slug の別テナントと共有させない。

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

## 監査ログの保持期間

```bash
curl -s -X PUT "$SHIKI/admin/tenants/acme/audit-retention" \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"retention_days": 730}'
# → 204

# 同じことを CLI で（--actor は自己申告として metadata に残る）
shiki-admin audit-retention --tenant acme --days 730
```

- 既定 365 日・範囲 90〜3650 日（範囲外は 400）。active でないテナントは 404（#831）。
- 期限を過ぎた行は日次で消す（不可逆・外部への退避はしない）。間隔は `storage.audit_retention.interval_secs`
  （既定 1 日）、`storage.audit_retention.enabled=false` で止められる。
- 実データを変える操作の行（ハッシュチェーン）は先頭から消し、消した最後の行を `audit_chain_anchor` に
  境目として残す。チェーンの検証（`GET /org-audit/verify`）はこの境目から始まる。消したことは
  `audit.retention.purge` として、設定の変更は `tenant.audit_retention.set` として監査に残る。
- **テナント撤去の記録（`tenant.purge`）は消さない。** 撤去後は tombstone に残った保持期間で、ほかの行を
  消し続ける。テナント台帳に無いテナント（オンプレなど）は既定の 365 日で消す。
- **短くする方向は次回の日次処理で効く。** 監査や法令で長く要るなら縮める前に書き出しておく。

## テナント削除（破壊的・不可逆）

```bash
curl -s -X DELETE "$SHIKI/admin/tenants/acme" -H "Authorization: Bearer $TOKEN"
# → 204
```

撤去順（全段冪等・途中失敗は同一リクエスト再実行で収束）:
1. tenant 行を `deleting` へ
2. セッション全失効（Redis）
3. Keycloak: `tenant` 属性一致ユーザーと `/tenants/<tenant>` subtree（org・部署 group）を削除
4. データ purge: FGA タプル（node/role/org。org は登録簿の 1 つに限らずテナント配下の全 org・#816）→ オブジェクト `{tenant}/` prefix → DB 行（1 txn・FK 順）
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

**group の置き場所（AD 連携顧客の運用要件・#811）**: org と部署の group は必ず
**`/tenants/<tenant_id>/<org>[/<部署>...]`** の配下に置く（LDAP group マッパーの `groups.path`・IdP マッパーの
`group` もこの配下を指す）。サーバは解決済み tenant_id の配下だけを org/部署とみなし、role id はテナントルート
からの相対パス（`<org>/<部署>`）になる。

- multi（SaaS）では、テナント無修飾の group（`/acme`・`/acme/eng`）や他テナント配下の group は
  **org にも部署 role にもならない**（org は `default` に落ちる）。
- single（オンプレ）は `/tenants/<auth.tenant_id>/...` を優先し、無ければ従来どおり直下の group（`/acme`）を使う。
  従来解釈の結果は #811 以前と同じ（org は groups claim の**先頭要素**の先頭セグメント・ソートしない）。
  自テナント配下の group が 1 つでもあれば、そのユーザーの旧形式 group は org にも部署にも使わない。
- `tenants` はトップレベル group 名として予約。テナントルート（`/tenants/<tenant>`）自体は org にならない。
  **オンプレの注意**: 既存 realm に `tenants` という名前のトップレベル group があると、その配下は従来解釈でも
  org/部署として読まれなくなる（他テナントのものとみなす）。該当する場合はアップグレード前に名前を変える。

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
  `tenant.retenant` エントリ（from/to 込み）を、新テナントで連結行を持つ**各 org** の chain へアンカーとして記録する。
  チェーンの検証（`GET /org-audit/verify`・#828）はこの記録の `from` を読み、境目より前の行を
  旧 tenant_id で自動的に検証する。
- **IdP の tenant 属性と group**: リネーム後、Keycloak ユーザーの `attributes.tenant` と group
  `/tenants/<from>` → `/tenants/<to>` を CLI が自動追従する（provisioner 設定時）。未設定の場合は**手動で更新するまで新ログインが
  旧テナント名前空間に落ちる**（org/部署も解決されない）ため、必ず更新すること。`/tenants/<to>` が既にあると
  group はリネームせず、警告を出して非 0 で終わる。`/tenants/<from>` 配下の org・部署 group のツリーを
  `/tenants/<to>` 配下へ統合し（メンバー・group のロール割り当て・属性も移す）、移し終えたら `/tenants/<from>` を削除する。

## org group のテナント修飾への移行（shiki-admin migrate-org-groups）

#811 以前は org group を素の slug（`/acme`）で作っていたため、同じ slug を使うテナント同士で group
（とそこに付いた client role・マッパー・属性）を共有し得た。新しいサーバはテナント修飾の
`/tenants/<tenant>/<org>` しか org/部署として読まない（multi）。稼働中の SaaS テナントが無い前提で、
**停止して一括移行**する（二重読みの移行期間は設けない）。

1. shiki-server を停止する。multi（SaaS）では旧サーバは新パスを読めず、新サーバは旧パスを読まないため、混在させない。
   single（オンプレ）の新サーバは自テナント配下が無ければ旧形式（`/acme`）も読むが、移行中の group 変更を
   利用者から隔離するため同じく停止して行う。
2. dry-run で計画を確認する:
   ```bash
   shiki-admin migrate-org-groups            # dry-run（旧 group ごとの複製先・移動件数・警告）
   shiki-admin migrate-org-groups --only acme --exclude ops-admins   # 対象を明示・除外
   ```
   - 対象は **org と分かっている旧トップレベル group だけ**: tenant 登録簿の org、既に `/tenants/<t>/<slug>` へ
     移した slug、`--only` で明示した slug（`--exclude` で除外）。LDAP 同期・運用用のトップレベル group を
     `/tenants/<t>/` へ複製すると org 候補に紛れて org が入れ替わり得るため、それ以外は「対象外」と出して触らない。
     single（オンプレ）は登録簿が空のことが多いので `--only` で指定する。
   - 対象の旧 group を部署ごと読み、移行先テナントごとに
     `/tenants/<tenant>/<org>/...` へ複製する計画を出す。移行先は multi ならユーザーの `tenant` 属性、
     single なら `auth.tenant_id`。メンバーの居ない旧 group は、tenant 登録簿で同じ org を使うテナントへ複製する。
   - `tenant` 属性が無い・不正なメンバーは**動かさない**（旧 group も残る）。⚠️ として名前と旧パスが出るので、
     属性を直して再実行する。
   - 旧 group の **client role / realm role の割り当て・属性はコピーしない**（件数だけ出る）。旧 group パスを参照する
     IdP マッパー・LDAP group マッパー（`groups.path`）・既定 group も一覧に出る。必要なものは新パスへ手で付け直す。
     これらを取得できなかった場合は、実際の参照ではなく「⚠️ 調べられなかった realm 設定（手で確認要）」として
     種別だけが出る。その場合は dry-run の一覧を信用せず、Keycloak 側で手動確認を終えるまで `--execute` しない。
3. 実行する:
   ```bash
   shiki-admin migrate-org-groups --execute
   ```
   旧 group ごとに、新 group を用意 → 各メンバーを新 group へ追加してから旧 group から外す → 全員を移せた
   旧 group を subtree ごと削除 → その旧 group の計画に含まれる全移行先テナントのセッションを失効（再ログインで
   新しい groups claim を取らせる。移動の成否を問わず、計画に載ったテナントは失効させてから次へ進む・途中で
   失敗した場合も、その旧 group の移行先までは失効させてから止まる）。全段冪等で、
   途中で失敗しても再実行で収束する（移動だけ済んで削除に失敗した旧 group も、移行済み slug として削除まで進む）。
   旧 group から外せないメンバー（LDAP_ONLY の同期 group 等）は ⚠️ で報告し、その旧 group は残す。
   このときは全計画を処理した後に**非 0 で終了**する（旧 group と付いた role が残るため成功扱いにしない）。
   LDAP 側のマッパー設定を直してから再実行する。再実行して「移行する旧 org group はありません」になれば完了。
4. 新しい shiki-server を起動する。dev の compose は realm を初回だけ import するため、既存の Keycloak
   ボリュームを使い続けるなら同じ CLI で移行するか、Keycloak の DB を作り直す。

## アップグレード時の注意

- **#816 のリリースは旧バイナリと並走させない（非ローリングで切り替える）**: 監査チェーンの直列化ロックが
  32bit → 64bit キーに変わり、新旧が同じ (tenant, org) を別キーでロックするため、並走中の chained 書込で
  ハッシュチェーンが分岐し得る。compose は単一コンテナなので停止 → 起動の順で入れ替える。
