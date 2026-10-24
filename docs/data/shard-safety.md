# シャード安全の禁止事項

> [data-platform.md §12 第 2 段](../data-platform.md)（テナント単位のプール分割）の前提を
> **今のうちに壊さない**ための規則。実分割は需要が出てからだが、継ぎ目（`PoolRouter`）と
> 禁止事項は Phase 15 Task 15.1 で先に入れた。**継ぎ目を後から入れるのは高い**（全ストアの
> コンストラクタと全クエリ経路を触り直すことになる）。

## 継ぎ目

- `crates/storage/src/pool_router.rs` の `PoolRouter` が `tenant → pool` 解決の単一チョークポイント。
  - `for_tenant(tenant_id)` … テナントの行を読み書きするプール（分割後は `tenant_placement.pool_id` で解決）
  - `registry()` … 配置レジストリ（`tenant_placement`）と横断一意の採番を置くプール（分割後も 1 か所）
  - `all()` … テナント横断の保守処理が**各プールへ問い合わせて合算**するための列挙
- `crates/data` の `DataStore` は接続プールを保持せず、毎回 `for_tenant(&ctx.tenant_id)` から引く。
- 内部整数キーの採番は `crates/storage/src/tenant.rs` の採番サービスだけが行う
  （`ensure_placement` = `tenant_int`・`allocate_table_int` = `table_int`）。

## 禁止事項

| # | 禁止事項 | 理由 | 検査 |
|---|---|---|---|
| S1 | 構造化データの層（`crates/data`）が接続プールを直接持つ・参照する | プール分割の差し替え点が散る | CI: `scripts/check-shard-safety.sh` と `crates/data/tests/shard_safety_lint.rs` |
| S2 | グローバル連番に依存する（`nextval` 等を各所で直接呼ぶ・列 default でテナント横断一意の番号を振る） | 各プールが独立にシーケンスを持つと**シャード跨ぎで衝突する**。`table_int` / `tenant_int` は採番サービス経由 | CI: 同上（`crates/data/src` での直接呼び出しを禁止） |
| S3 | tenant 条件の無い SQL で `data_*` を読み書きする | テナント横断の読み書きは分割後に成立しない（＋ day-1 の tenant スコープ不変条件 #91） | CI: `shard_safety_lint.rs`（SELECT/UPDATE/DELETE は `tenant_id =` か `tenant_int =`、INSERT は tenant 列を要求） |
| S4 | テナント横断の JOIN を書く（`data_*` 表どうしの JOIN を含む） | 分割後は別ノードの表になる | CI: `shard_safety_lint.rs`（`data_*` 表どうしの JOIN は注記なしでは不可） |
| S5 | テナント横断の集計を単一クエリで書く | 分割後は 1 クエリで全テナントを見られない。**各プールへ問い合わせて合算**する（`PoolRouter::all`） | レビュー（S3 の注記付き例外が該当箇所の一覧になる） |
| S6 | 外部キーがテナント境界を跨ぐ | 分割後は親子が別ノードになり得る | 既存規約: 全 FK は `tenant_id` を含む複合キー（migration 0061・`tenant_scope.rs` の導出が前提にしている） |

### 表の分類（Phase 15 で追加・監査 B1-8）

S3・S4 の対象を取りこぼさないよう、lint（`crates/data/tests/shard_safety_lint.rs`）は **migrations の `create table data_*`
（後で `drop table` されたものを除く）を網羅的に照合**し、全てがどちらかに分類されていることを求める
（`every_data_table_in_migrations_is_classified`）。新しい `data_*` 表を足したら、どちらかへ載せないと CI が落ちる。

| 分類 | 内容 | 例 |
|---|---|---|
| **テナント境界の表**（`DATA_TABLES`） | S3・S4 の対象。加えて**テナント消去（`DataStore::purge_tenant`）の対象に載っていること**を照合する（`every_tenant_data_table_is_purged`） | `data_record`・索引表・統計 4 表・取込のステージング など |
| **システム表**（`SYSTEM_TABLES`・理由つき） | テナントに属さない。tenant 条件を持たず、purge の対象でもない。S3・S4 の対象外 | `data_planner_calib`（オプティマイザの較正用の固定表・migration 0098。既知の合成データだけで顧客データを含まない。全プールに同じ内容があり、較正はレジストリのプールで読む） |

統計 4 表（`data_table_stats` / `data_field_stats` / `data_value_count` / `data_link_stats`）は、加えて PIT-75 の検査
（`optimizer_statistics_are_only_touched_by_planner_and_stats_writers`）で、SQL を `query/planner/` と `stats/` にだけ許す。

### 例外の書き方

保守スイープ（識別子移行ジョブ等）のように**意図的に**テナントを跨ぐ SQL は、直前 3 行以内に
理由つきの注記を置く。注記の無い横断 SQL は CI が落とす。

```rust
// shard-safety: allow(cross-tenant) 保守スイープ。プールごとに問い合わせる（合算は Rust 側）。
let targets: Vec<(String, Uuid)> = sqlx::query_as(
    "SELECT tenant_id, id FROM data_table WHERE key_format = 1 ORDER BY tenant_id, id LIMIT $1",
)
```

## 分割時に必要になる作業（今はやらない）

- `PoolRouter::for_tenant` を `tenant_placement.pool_id` の解決に差し替える（リクエストスコープで 1 回引く）。
- **`record_int` の衝突回避**: `data_record_int_seq` はプールごとに存在する。テナントを別プールへ引っ越す
  （第 3 段の専用棟を含む）ときは、移行先のシーケンスを移行元の最大値より先へ `setval` してから行をコピーする
  （一意性は `(tenant_int, table_int, record_int)` で担保しているので、同じテーブル内で衝突しなければよい）。
- 監査ログ（`DataStore` の事後監査は現状レジストリのプールに書く）の置き場を決め直す。
- 引越中の整合（無停止移行）は data-platform §12.1 の残課題。
