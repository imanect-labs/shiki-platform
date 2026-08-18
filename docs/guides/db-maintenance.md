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
```

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
