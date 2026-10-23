-- Phase 15 Task 15.7: 一括書込パスの更新経路（bulk upsert・data-platform §11 / §16.7）。
--
-- 0090 のステージングは新規作成の行だけを持っていた。ワークフローの `data.bulk_upsert` ノードは
-- 「既存レコードの更新」と「新規作成」を 1 回の取込で混ぜるので、行ごとに操作を持たせる。
--
-- - op = 0（作成）: 従来どおり。record_id は COPY 時に採番した新しい外部 ID。
-- - op = 1（更新）: record_id は更新対象の外部 ID、payload は merge patch。検証と適用は 1 件の更新と
--   同じ入口（crates/data/src/record_update.rs の update_locked）を行ロックの下で通す
--   （行述語つきロック・書込述語・rev の照合・マスク列への書込拒否・同じ検証器・outbox・監査）。
-- - expected_rev: 更新の楽観ロック（null は照合しない＝行ロックの下の後勝ち）。

alter table data_bulk_staging
    add column op smallint not null default 0 check (op in (0, 1)),
    add column expected_rev bigint;
