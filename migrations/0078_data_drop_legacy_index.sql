-- Task 15.3: 旧方式（v1）の索引を撤去する（data-platform §13 M4）。
--
-- 読取は pivot 索引テーブル＋クエリコンパイラ v2 へ切り替わった（crates/data/src/query/）。
-- 宣言フィールドごとの partial 式インデックス（dr_*・ランタイム DDL で leaf パーティションに張っていた）と
-- その台帳 data_index_registry を落とす。これで**ランタイム DDL は完全にゼロ**になる。
-- v1 の既定並び（updated_at 降順）の索引 data_record_table_idx も、擬似列 updated_at（field_id 2）の
-- 値索引（data_index_text_val_idx）に置き換わったので落とす。
--
-- 既存データへの影響: 索引は速さにしか効かない（読取の正しさは v2 の索引テーブルか、索引が未完成なら
-- 本体経路で保たれる）。unique の強制は data_unique の主キーが担う（Task 15.2 から二重書込済み。
-- 台帳が未初期化のテーブルは初回アクセス時に unique を同期構築してから書込を通す）。
do $$
declare
    r record;
begin
    -- 台帳の外に残ったものも含め、data_record（の leaf パーティション）に張られた dr_* を全て落とす。
    for r in
        select i.indexname
          from pg_indexes i
         where i.schemaname = current_schema()
           and i.tablename ~ '^data_record(_p[0-9]+)?$'
           and i.indexname ~ '^dr_[0-9a-z_]+$'
    loop
        execute format('drop index if exists %I', r.indexname);
    end loop;
end $$;

drop table if exists data_index_registry;
drop index if exists data_record_table_idx;
