# DB 運用手順（大規模テーブルへの index 適用・fillfactor の反映）

migration に**書けない**、あるいは**書くと危険な** DB 作業をここへ集約する。個々の migration
コメントに散らすと、その migration を読んだ人にしか届かない（#448）。

対象は「既に大きく育った本番 DB」だけである。**新規 DB・CI・小規模環境では何もしなくてよい。**

§1 は migration 0062（#438）、§2 は migration 0063（#440）を前提にする。どちらも未マージなら
その節はまだ効かない（対象の index / 設定が存在しない）。

関連: `docs/guides/db-observability.md`（何が遅いかを見つける側・#442。**未マージ**なので、
このファイルが無ければ先に #442 を入れる）・
[tenant-ops.md](./tenant-ops.md)（テナント運用）。

---

## 1. 大規模テーブルへの index 追加（デプロイ前に手で作る）

### なぜ手順が要るのか

通常の `CREATE INDEX` はビルド中、対象表への `INSERT` / `UPDATE` / `DELETE` をブロックする
（読みは通る）。数百万行のテーブルでは分単位になり得るので、起動時 migration
（`crates/api/src/main.rs` の `sqlx::migrate!`）の中で走ると、その間 run の遷移・claim・
heartbeat・副作用 journal の書込が止まる。ローリング更新中なら旧バージョンが動いたまま止まる。

### なぜ `CREATE INDEX CONCURRENTLY` を migration に書かないのか

**この経路では deadlock するため。** sqlx の migrator は**セッションレベルの advisory lock** を
保持したまま migration を適用する。一方 `CONCURRENTLY` は完了前に「実行中の全トランザクションの
終了」を待つ。複数インスタンスが同時起動すると循環する。4 並列での実測:

```text
40P01 deadlock detected
  Process A waits for ExclusiveLock on advisory lock; blocked by B.
  Process B waits for ShareLock on virtual transaction 6/46; blocked by C.
  Process C waits for ExclusiveLock on advisory lock; blocked by A.
```

つまり `CONCURRENTLY` を migration に置くと「短い書込ブロック」が「**起動時デッドロックで
アプリが上がらない**」に化ける。悪化なので採らない。

`-- no-transaction` を付けても解決しない。1 ファイルに複数ステートメントを書くと Postgres が
暗黙トランザクションを張るため `25001 CREATE INDEX CONCURRENTLY cannot run inside a transaction
block` になり、1 文に割っても上の advisory lock 問題は残る。

### 手順

**デプロイ前**に psql から、**単独セッションで 1 文ずつ**実行する。migration 側はすべて
`CREATE INDEX IF NOT EXISTS` なので、先に作ってあれば no-op になる。

```sql
-- #438 / migration 0062: step claim を ready 専用で O(1) にする index
CREATE INDEX CONCURRENTLY IF NOT EXISTS step_ready_global_idx
    ON step_execution (next_retry_at) WHERE status = 'ready';

-- #444 / migration 0064: 保持期間 GC の候補走査
CREATE INDEX CONCURRENTLY IF NOT EXISTS workflow_run_gc_idx
    ON workflow_run (tenant_id, finished_at)
    WHERE status IN ('succeeded', 'failed', 'cancelled');

CREATE INDEX CONCURRENTLY IF NOT EXISTS effect_journal_gc_idx
    ON effect_journal (tenant_id, created_at);

-- #829 / migration 0082: 監査ログを 1 回答・1 会話の記録で絞る（持つ行だけの部分索引）
CREATE INDEX CONCURRENTLY IF NOT EXISTS audit_log_tenant_message_idx
    ON audit_log (tenant_id, org, (metadata ->> 'message_id'), created_at DESC, id DESC)
    WHERE metadata ? 'message_id';

CREATE INDEX CONCURRENTLY IF NOT EXISTS audit_log_tenant_thread_idx
    ON audit_log (tenant_id, org, (metadata ->> 'thread_id'), created_at DESC, id DESC)
    WHERE metadata ? 'thread_id';

-- #879 / migration 0083: サイドバー履歴のピン留め一覧・スレッドの run を thread_id で引く
CREATE INDEX CONCURRENTLY IF NOT EXISTS thread_owner_pinned_idx
    ON thread (tenant_id, org, owner, pinned_at DESC)
    WHERE deleted_at IS NULL AND pinned_at IS NOT NULL;

CREATE INDEX CONCURRENTLY IF NOT EXISTS generation_run_thread_idx
    ON generation_run (tenant_id, thread_id, created_at);

-- #965 / migration 0108: ホームの「今日のこと」の承認待ちを actor で引く（承認待ちだけの部分索引）
CREATE INDEX CONCURRENTLY IF NOT EXISTS generation_run_waiting_actor_idx
    ON generation_run (tenant_id, actor)
    WHERE status = 'waiting_approval';
```

停止時間を取れる環境なら、事前作成せずにそのまま migration に任せてもよい（通常の
`CREATE INDEX` なので、構築の間だけ対象表への書込が止まる）。

途中で失敗すると**無効な index が残り、プランナに使われないまま書込コストだけ増える**。
必ず確認して落としてからやり直す。

```sql
SELECT indexrelid::regclass, indrelid::regclass
  FROM pg_index WHERE NOT indisvalid;

DROP INDEX CONCURRENTLY <上で出た index 名>;
```

進捗は `pg_stat_progress_create_index` で見える。

```sql
SELECT phase, blocks_done, blocks_total, tuples_done, tuples_total
  FROM pg_stat_progress_create_index;
```

---

## 2. `fillfactor` を既存データへ反映する

`ALTER TABLE ... SET (fillfactor = ...)`（migration 0063 / #440）は**既存のヒープページを
書き換えない**。新しい余白は以降の `INSERT` / `UPDATE` で作られるページにしか効かないので、
既に満杯のページに載っている行はしばらく HOT update にならず、**設定の効果が出ない**。

対象は「migration 0063 より前から存在し、かつ fillfactor を設定した」極小テーブル 2 つだけで、
いずれも数ページなので一瞬で終わる（`maintenance_schedule` にも fillfactor=50 を設定しているが、
0064 で新規作成される表なので最初から新しい fillfactor で書かれる＝対象外）。
どちらも `ACCESS EXCLUSIVE` を取り、かつトランザクション内で実行できないため migration には
置けない。**稼働中の DB で 1 度だけ**実行する。

```sql
VACUUM FULL concurrency_counter;
VACUUM FULL scheduler_lease;
```

ロックを避けたい場合は `pg_repack` でも同じ効果が得られる。新規 DB では最初から新しい
fillfactor でページが作られるので**何もしなくてよい**。

反映されたかは HOT update 率で確認する（`n_tup_hot_upd / n_tup_upd` が 1 に近ければ成功）。

```sql
SELECT relname, n_tup_upd, n_tup_hot_upd,
       round(100.0 * n_tup_hot_upd / nullif(n_tup_upd, 0), 1) AS hot_pct
  FROM pg_stat_user_tables
 WHERE relname IN ('concurrency_counter', 'scheduler_lease');
```

---

## 3. 実行履歴 GC の運用

保持期間は `tenant.workflow_retention_days`（初期値 90 日・範囲 1〜3650 日）。設定は
provisioner トークンで:

```bash
curl -X PUT "$API/admin/tenants/$TENANT/workflow-retention" \
  -H "Authorization: Bearer $PROVISIONER_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"retention_days": 30}'
```

**短くする方向は次回の日次 GC で即座に効く。** 監査要件で履歴が要る場合は縮める前に退避すること。

GC は `workflow.enabled` と独立して動く（保持はコンプライアンス側の義務であり、新規実行の
可否とは別）。状態は 2 つのテーブルで見る。

```sql
-- 日次投入の台帳（last_enqueued_at が 24h 以上前なら次のポーリングで積まれる）
SELECT * FROM maintenance_schedule WHERE job_name = 'workflow_history_gc';

-- 投入済み/再試行待ちのジョブ（visible_at が未来なら失敗してバックオフ中）
SELECT id, attempts, max_attempts, visible_at FROM job_queue WHERE queue = 'workflow_gc';

-- 試行上限を超えて DLQ へ落ちたジョブ（last_error に理由が入る）
SELECT id, attempts, enqueued_at, last_error FROM job_queue_dead WHERE queue = 'workflow_gc';
```

削除件数は `実行履歴 GC を実行しました` のログ（`runs_deleted` / `journal_deleted` /
`truncated`）に出る。`truncated=true` が続く場合はバッチ上限に張り付いている＝流入が削除を
上回っているので、保持日数か `MAX_BATCHES`（`crates/workflow-engine/src/run/store/gc.rs`）を
見直す。

---

## 4. DB ロール: 実行時ロールと migration の所有者を分ける（#1030）

shiki-server は**実行時**と**migration** で別の接続を使う。superuser とテーブル所有者は RLS を迂回するので、
実行時は DML だけを持つ非 superuser のロールで接続する（RLS 段階導入の第 1 段）。

| 設定 | 接続するロール | 権限 |
| --- | --- | --- |
| `database.url`（`SHIKI__DATABASE__URL`） | `shiki_runtime` のメンバ（compose では `shiki_app`） | 全テーブルの SELECT / INSERT / UPDATE / DELETE・sequence の利用・`shiki_analyze` の実行 |
| `database.migrate_url`（`SHIKI__DATABASE__MIGRATE_URL`） | テーブルの所有者（migration を流すロール） | DDL |

- `migrate_url` が未設定なら、従来どおり `url` で migration を流す（単一ロールの構成）。`url` を実行時ロールに
  替えて `migrate_url` を付け忘れると、起動時の migration が権限不足で止まる（fail-fast）。
- `shiki_runtime`（NOLOGIN のグループロール）は migration 0113 が作る。migration の実行ロールに CREATEROLE が
  無い環境（マネージド Postgres 等）では、先に運用が作っておく:
  ```sql
  CREATE ROLE shiki_runtime NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
  ```
- ログイン用のロールは運用が作り、`shiki_runtime` のメンバにする（パスワードを migration に書かないため）:
  ```sql
  CREATE ROLE shiki_app LOGIN PASSWORD '...' IN ROLE shiki_runtime;
  GRANT CONNECT ON DATABASE shiki TO shiki_app;
  ```
  compose では `postgres-roles` サービス（`deploy/compose/postgres/runtime-role.sh`）が起動のたびに冪等に作る。
- **migration の所有者ロールを替えたとき**は、新しい所有者で既定権限を付け直す（既定権限は作ったロールごとに効く。
  付け忘れると、以後の migration で増えた表に実行時ロールが触れず、初回アクセスで 42501 になる）:
  ```sql
  ALTER DEFAULT PRIVILEGES FOR ROLE <新しい所有者> IN SCHEMA public
      GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shiki_runtime;
  ALTER DEFAULT PRIVILEGES FOR ROLE <新しい所有者> IN SCHEMA public
      GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO shiki_runtime;
  ```
- 実行時の ANALYZE（構造化データのパーティションの統計・#986）は、PG16 ではテーブル所有者にしかできないため、
  `data_*` に絞った SECURITY DEFINER 関数 `shiki_analyze(regclass, skip_locked)` を経由する。
- 実行時ロールは `_sqlx_migrations`（migration の台帳）に触れない。

## 5. テナント表の RLS（#1067・段階導入の第 2 段）

migration 0114 は、`tenant_id`（NOT NULL）を持つテナント表に RLS を付ける（`ENABLE` と `FORCE`、ポリシー
`shiki_tenant_isolation`）。`data_*` と、`tenant_id` が NULL を取り得る表（`outbox_delivery_seq`）は対象外。

- **今は移行モード。** トランザクションのテナント文脈（`app.tenant_id`）が未設定なら従来どおり通し、設定されて
  いればその `tenant_id` の行だけを読み書きさせる。文脈は `storage::db_tenant::begin_tenant_tx`（または
  `set_tenant`）がトランザクション限り（`SET LOCAL` 相当）で設定する。既存の SQL は第 3 段でクレートごとに
  この実行器へ移し、全経路を移したら未設定を拒否する厳格モードへ切り替える。
- **テナント横断の保守処理**は `storage::db_tenant::begin_cross_tenant_tx` を使う。トランザクションの中で
  `shiki_maintenance`（NOLOGIN・BYPASSRLS）に `SET LOCAL ROLE` する。`shiki_runtime` には INHERIT FALSE /
  SET TRUE で付与してあるので、ログイン用のロール（`shiki_app`）に追加の付与は要らない。
- `shiki_maintenance` は migration 0114 が作る。BYPASSRLS のロールを作れるのは superuser（PG16 では CREATEROLE
  かつ自身が BYPASSRLS）だけなので、migration の実行ロールにその権限が無い環境では先に運用が作っておく:
  ```sql
  CREATE ROLE shiki_maintenance NOLOGIN NOSUPERUSER BYPASSRLS NOCREATEDB NOCREATEROLE;
  ```
  マネージド Postgres で BYPASSRLS のロールを作れない場合は、第 3 段の厳格化の前に横断処理の扱いを決め直す
  （移行モードのうちは、横断処理は文脈を設定しないだけで動く）。
- **migration の所有者ロールを替えたとき**は、§4 の既定権限に加えて `shiki_maintenance` にも同じ既定権限を付け直す。
- **新しいテナント表を足す migration** は、同じ `ENABLE` / `FORCE` / `CREATE POLICY shiki_tenant_isolation` を
  付ける（式は 0114 と同じ）。付け忘れは `crates/storage/tests/rls_it.rs` が落とす。
- psql で調べるとき、superuser は RLS を迂回する。アプリと同じ見え方を確かめるには `SET ROLE shiki_app` してから
  `SELECT set_config('app.tenant_id', '<tenant>', false)` する。
