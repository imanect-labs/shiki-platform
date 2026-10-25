# 内部整数キー（`record_int`）の計測

> Phase 15 Task 15.1 の受け入れ条件「uuid キーと `record_int` キーで索引テーブルの大きさと、
> 和集合・交差（Roaring）の速さを計測して記録する」の記録。設計の根拠は
> [data-platform.md §3.2](../data-platform.md)（`record_int` を入れる理由）。
> 索引テーブル本体は Task 15.2 の担当なので、ここでは §3.3 の `data_index_text` と同じ列構成の
> **一時表で試作**して測った。

## 再現手順

```bash
DATA_BENCH=1 STORAGE_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/shiki \
  cargo test -p shiki-data --release --test internal_key_bench_it -- --nocapture --test-threads=1
```

- 計測コード: [`crates/data/tests/internal_key_bench_it.rs`](../../crates/data/tests/internal_key_bench_it.rs)
  （`DATA_BENCH=1` のときだけ走る。レコード数は `DATA_BENCH_RECORDS` で変えられる）
- 環境（2026-10-22 計測）: PostgreSQL 16.15・4 vCPU のクラウド開発コンテナ・Rust リリースビルド。
  絶対値は環境で揺れるので、**比**を読むこと。

## 1. 索引テーブルの大きさ

20 万レコード × 5 フィールド = 100 万索引行。値は select 相当（1,000 種の短い文字列）。
主キー `(tenant_int, table_int, field_id, rec, ord)` と値索引 `(tenant_int, table_int, field_id, val, rec)` を
張り、`VACUUM ANALYZE` 後に `pg_relation_size` を測った。

| キー | 行数 | ヒープ MiB | 主キー MiB | 値索引 MiB | 合計 MiB | 1 行あたり B |
|---|---:|---:|---:|---:|---:|---:|
| uuid（§3.3 の列順） | 1,000,000 | 73.0 | 73.2 | 80.4 | 226.7 | 237.7 |
| record_int（§3.3 の列順） | 1,000,000 | 73.0 | 56.2 | 78.9 | 208.1 | 218.2 |
| uuid（幅の降順に整列） | 1,000,000 | 72.3 | 72.7 | 81.5 | 226.6 | 237.6 |
| record_int（幅の降順に整列） | 1,000,000 | 64.6 | 56.2 | 78.9 | 199.7 | 209.4 |

- **合計で 8.2%（§3.3 の列順）〜 11.9%（列順を整列）縮む**。§3.2 の見積もり「約 7%」と同程度か上回る。
- 効くのは**主キー（−23%）**。値索引は `val` の幅が支配的なので差が小さい（−2〜3%）。
  uuid の値索引はランダム挿入でページ分割が増えるため、計測ごとに 80〜85 MiB で揺れた。
- **ヒープは §3.3 の列順のままだと 1 バイトも縮まない**。`field_id int2` の直後に `int8` が来ると
  8 バイト境界へのパディングが入り、uuid（境界 1）から int8 にした節約をパディングが食う。
  固定長列を幅の降順（`table_int, record_int, tenant_int, field_id, ord, truncated, val`）に並べると
  ヒープも −11.5% になる。**Task 15.2 で索引テーブルを作るときは列順を整列させること**
  （主キー・索引の列順は変えなくてよい。並べ替えるのはテーブル定義の物理列順だけ）。

## 2. 和集合・交差の速さ

「見える行の集合」「OR の和集合」「交差」を Rust 側で計算する場合（query-optimizer.md §5.1・§6.4）の比較。
A と B は半分ずつ重なる。uuid 側は同じ要素数・同じ重なり方を uuid で再現した。各 7 回の中央値。

| ケース | 要素数 A/B | HashSet&lt;Uuid&gt; ∪ | HashSet&lt;Uuid&gt; ∩ | 整列 Vec&lt;Uuid&gt; ∪ | 整列 Vec&lt;Uuid&gt; ∩ | Roaring ∪ | Roaring ∩ | Roaring KiB | uuid 配列 KiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 密（単一テーブルが連続挿入） | 100 万 / 100 万 | 235.60 ms | 230.79 ms | 15.89 ms | 14.51 ms | 0.03 ms | 0.03 ms | 183 | 15,625 |
| 疎（100 テーブルが交互に挿入） | 100 万 / 100 万 | 221.83 ms | 236.13 ms | 18.99 ms | 16.54 ms | 2.24 ms | 1.48 ms | 1,970 | 15,625 |
| 小（可視行 1 万） | 1 万 / 1 万 | 0.68 ms | 0.38 ms | 0.13 ms | 0.11 ms | 0.01 ms | 0.01 ms | 20 | 156 |

- **Roaring（`RoaringTreemap`・64 ビット版）は整列済み uuid 配列のマージより 8〜500 倍速く、
  `HashSet<Uuid>` より 2〜4 桁速い**。メモリも uuid 配列の 1/8（疎）〜 1/85（密）。
- `record_int` はプール共通のシーケンスなので、テーブル内では他テーブルの挿入に挟まれて**疎**になる
  （§3.2）。疎のケースでも 100 万要素の和集合が 2 ms 台に収まり、§3.2 の「テーブル内では疎になるが
  Roaring は疎な集合も扱える」は実測で裏付けられた。
- 「密」は 1 テーブルだけが連続挿入した理想形（Roaring の run コンテナが効く）。実運用は「疎」寄りと見る。

## 結論

- 索引テーブルの容量は 8〜12% 縮み、主キーだけなら 23% 縮む。**列順を整列させないとヒープの節約が消える**
  ことが分かったので、Task 15.2 の DDL へ申し送る。
- 集合演算は Roaring で 1〜2 桁以上速くなり、Rust 側でビットマップ演算する設計（query-optimizer.md）の前提が成り立つ。
- `record_int` は外へ出さない（PIT-75）。カーソルは AEAD で暗号化する（`crates/data/src/cursor.rs`）。
