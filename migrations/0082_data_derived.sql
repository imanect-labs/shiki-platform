-- Phase 15 Task 15.4: リンク／ルックアップ／ロールアップ／数式の材料化規則（docs/data-platform.md §7・PIT-71）。
--
-- 材料化（Materialization::Invariant かつ indexed の派生列）の値は**索引テーブル（data_index_num /
-- data_index_text）の行としてだけ**持つ（本体 JSONB には書かない）。表示は常に閲覧者本人の権限で読取時に
-- 計算し、材料化した行はフィルタ・ソートにだけ使う。降格（PerViewer へ）のときは索引行・再計算待ち・
-- 依存登録を**同一トランザクションで**消す（材料化データの残留＝漏洩源を作らない）。
--
-- ① 再計算待ち（参照先レコードの変更で陳腐化した材料化値）。行がある (テーブル, 列) はクエリ対象外
--    （フィルタ・ソートは 409）。gen は同じ行への再要求で増え、ジョブは読んだ世代の行だけを消す
--    （ジョブの処理中に来た再要求を取りこぼさない）。
create table data_derived_pending (
    table_int   bigint      not null,
    record_int  bigint      not null,
    gen         bigint      not null default 1,
    tenant_int  integer     not null,
    field_id    smallint    not null check (field_id >= 0),
    updated_at  timestamptz not null default now(),
    primary key (tenant_int, table_int, field_id, record_int)
);

-- ② 依存登録: 材料化した派生列（table_int, field_id）が参照先テーブル（ref_table_int）のどのリンク列を
--    辿るか。参照先の書込・スキーマ改訂・個別共有の Tx から影響する参照元を 1 本の索引で引く。
create table data_derived_dep (
    ref_table_int bigint   not null,
    table_int     bigint   not null,
    tenant_int    integer  not null,
    field_id      smallint not null check (field_id >= 0),
    link_field_id smallint not null check (link_field_id >= 0),
    -- リンクの対称列（参照先側の field_id）。あれば影響する参照元を参照先の逆方向行から主キーで引く
    -- （無ければ参照元パーティション内のリンク行を参照先で絞る）。
    sym_field_id  smallint check (sym_field_id >= 0),
    primary key (tenant_int, ref_table_int, table_int, field_id, link_field_id)
);
create index data_derived_dep_src_idx on data_derived_dep (tenant_int, table_int);

-- ③ レコード個別共有タプル（OpenFGA data_record）が 1 件でも作られたテーブルか（§7.4 の第 4 階層）。
--    OpenFGA の data_record タプルはテーブルに紐づかないため、共有の入口（DataStore::share_record）が
--    タプルを書く**前に**同じ行で立てる。単調（解除しても戻さない）。
--    既存のテーブルは本移行より前に共有されたかを知る手段が無いので、安全側（true）で初期化する。
alter table data_table add column has_record_shares boolean not null default false;
update data_table set has_record_shares = true;
