# DB パフォーマンスの観測

Postgres のクエリ性能・vacuum 状況を見るための手順書。「遅い」と言われたとき、どこを順に見るか。

> 監視スタック全体の位置づけは [design.md](../design.md) §4.9。本書はその Postgres 部分の詳細。
> テーブル個別の autovacuum 設定は `migrations/0063_autovacuum_hot_tables.sql` が正。

## なぜ SaaS（pganalyze 等）を土台にしないか

pganalyze・Datadog DBM 等は良いツールだが、**中身は `pg_stat_statements` を読んでいるだけ**である。そして本プロダクトはオンプレ／エアギャップ配布を前提とするため、

- SaaS 版は顧客のクエリ形状（＝業務の形）を外部へ送る。持ち込めない。
- 通信自体が通らない環境がある。
- 自己ホスト版は有償かつ運用部品が増え、design.md §1 の「新規ステートフル依存ゼロ・部品点数最小」と衝突する。

よって **`pg_stat_statements` ＋ `auto_explain` ＋ `postgres_exporter` → 既存 Prometheus/Grafana** を正の構成とする。既に Prometheus・Grafana・Tempo・Loki が居るので、新しい監視基盤を足すのではなく Postgres の口を 1 つ生やすだけで済む。

自社クラウド環境に限って商用ツールを足すのは有りだが、**オンプレ顧客の障害調査で手札が無くなる**ため、正の手順は必ず OSS 側で完結させること。

## 起動

```bash
docker compose --profile observability up -d
```

- `pg_stat_statements` / `auto_explain` は `shared_preload_libraries` で読み込む（compose の `postgres` サービスに設定済み）。**後付けには再起動が要る。**
- 統計を読む extension は DB ごとに必要。`init-multiple-dbs.sh` が初回起動時に作る。**既存ボリュームでは再実行されない**ので、その場合は 1 回だけ手で:
  ```bash
  docker compose exec postgres psql -U postgres -d shiki \
    -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements'
  ```
- Prometheus は `postgres-exporter:9187` を scrape する（`deploy/otel/prometheus.yml`）。

## 1. どのクエリが DB を食っているか

```sql
SELECT calls,
       round(total_exec_time::numeric)             AS total_ms,
       round(mean_exec_time::numeric, 2)           AS mean_ms,
       round((100 * total_exec_time / sum(total_exec_time) OVER ())::numeric, 1) AS pct,
       query
  FROM pg_stat_statements
 ORDER BY total_exec_time DESC
 LIMIT 20;
```

> **`mean_exec_time` で並べてはいけない。** ワークフローの claim のように「1 回 1ms 未満だがワーカーが常時ポーリングする」クエリは平均では絶対に浮かんでこないが、合計では上位に来る。**「速いけど超高頻度」が DB を殺す**のが典型的な壊れ方で、平均値だけ見ていると一生気づけない。

リセットして期間を区切りたいとき: `SELECT pg_stat_statements_reset();`

## 2. 遅かった瞬間の実行計画

「本番では遅かったのに、後から手で流すと速い」を潰すための仕掛け。`auto_explain` が 200ms を超えたクエリの計画を Postgres のログへ吐く。

```bash
docker compose logs postgres | grep -A 30 "duration:"
```

`auto_explain.log_timing` は既定で **off** にしてある。ノード単位の計時はそれ自体が重く、`log_analyze` と併用する際に off にするのが定石だからである。行数とバッファ数は取れるので、計画の形（`Seq Scan` / `Sort` / `Bitmap` への退化）を見る用途には十分足りる。

## 3. 掃除（vacuum）が追いついているか

```sql
SELECT relname, n_live_tup, n_dead_tup,
       round(100.0 * n_dead_tup / NULLIF(n_live_tup, 0), 1) AS dead_pct,
       autovacuum_count, last_autovacuum
  FROM pg_stat_user_tables
 ORDER BY n_dead_tup DESC
 LIMIT 20;
```

**`n_dead_tup` が増え続けているテーブルは掃除が負けている。** そのテーブルの partial index を使うクエリが道連れで劣化する（`step_execution` の `ready` index → ワークフローの claim が典型）。

`migrations/0063` の閾値はすべて初期値なので、ここを見ながら調整する。**アラートを張るならこの指標。**

使われていない index も同じビュー系で分かる。書き込みを遅くしているだけなので削除候補:

```sql
SELECT relname, indexrelname, idx_scan
  FROM pg_stat_user_indexes
 WHERE idx_scan = 0
 ORDER BY relname;
```

## 4. 今この瞬間 DB を詰まらせているのは誰か

```sql
SELECT application_name, state, wait_event_type, wait_event, count(*)
  FROM pg_stat_activity
 GROUP BY 1, 2, 3, 4
 ORDER BY count(*) DESC;
```

`application_name` は接続時に必ず設定してある（`shiki-server` / `shiki-admin`）。これが空欄だと全接続が `unknown` で並び、障害時に切り分けができない。

> **既知の限界**: ワークフローワーカーは shiki-server の in-process サブシステム（engine.md §1）で API と同一プールを共有するため、`application_name` では両者を区別できない。クエリ単位の切り分けは `pg_stat_statements` 側（クエリテキストで正規化される）で行う。

## 5. index の膨張を実測する

`n_dead_tup` は「掃除待ちの行」であって「index が膨らんでいるか」ではない。実サイズを見る:

```sql
SELECT indexrelname, pg_size_pretty(pg_relation_size(indexrelid)) AS size, idx_scan
  FROM pg_stat_user_indexes
 WHERE relname = 'step_execution'
 ORDER BY pg_relation_size(indexrelid) DESC;
```

**vacuum は index のエントリを回収するが物理サイズは戻さない。** 一度膨らんだ index を縮めるには `REINDEX CONCURRENTLY` が要る。

## 負荷テストで見る指標

負荷試験時に見るべきものは、HTTP のレスポンスタイムではない（ワークフローは非同期なので意味を持たない）。

| 指標 | なぜ |
|---|---|
| **step が ready になってから running になるまでの待ち時間（p50/p99）** | 「ワークフローが動き出さない」という体感に直結する SLI。アプリ側で計測して OTel に出す |
| run 投入から全 step 完了までの時間 | エンドツーエンド |
| `n_dead_tup` の推移 | 短時間の試験では絶対に出ない劣化。数時間走らせて初めて見える |

`claim` の並行スケーラビリティは `pgbench` のカスタムスクリプトで直接測れる。並行度を 1/4/8/16/32 と上げて、スループットが比例して伸びるか・どこで頭打ちになるかを見る。

```bash
pgbench -f claim.sql -c 32 -j 8 -T 60 -P 5 --no-vacuum
```

**backlog を意図的に深くしたシナリオを必ず含めること。** 実行待ちが 0 件の状態では、claim の計算量に関する問題は原理的に再現しない（#438 がまさにそれで、テストでも空いている本番でも見えなかった）。
