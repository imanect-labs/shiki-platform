-- Task 15.3: 複数ソート用の複合索引（data-platform §5.3・オンデマンド材料化）。
--
-- pivot 索引はフィールドごとに別行なので、(第 1 キー, 第 2 キー, …, record_int) の複合順序を持つ btree が無い。
-- 第 1 キーの同値群がページの K 倍（既定 8）を超えるソートの組み合わせは、その組み合わせの全順序を
-- **1 列のバイト列（順序を保つ符号化）**として行に持ち、値索引 1 本で歩く。
-- 物理本数はパーティションあたり 2 本（PK・値索引）の定数で、テナント数・テーブル数・ソートの組み合わせ数に
-- 依存しない（**ランタイム DDL は打たない**。材料化は行の追記）。
--
-- 生成は索引の自動昇格（§10）と同じ機構: data_sort_set に building で登録（data_table 行の FOR UPDATE の下）→
-- 書込は write-through → jobq でバックフィル → active。building の組はクエリに使わない。
create table data_sort_set (
    tenant_int    integer     not null,
    table_int     bigint      not null,
    sort_set_id   smallint    not null,
    -- ソートの組み合わせ（"16:a,17:d" = field_id と向き）。
    spec          text        not null,
    state         text        not null check (state in ('building', 'active')),
    -- バックフィルの進捗（record_int の位置と、開始時点の最大 record_int）。
    cursor_record bigint      not null default 0,
    high_water    bigint      not null default 0,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    primary key (tenant_int, table_int, sort_set_id),
    unique (tenant_int, table_int, spec)
);

create table data_index_composite (
    tenant_int  integer  not null,
    table_int   bigint   not null,
    sort_set_id smallint not null,
    record_int  bigint   not null,
    -- 全順序（ソートキー → record_int）を保つバイト列（query/planner/keys.rs の encode_pos）。
    val         bytea    not null,
    primary key (tenant_int, table_int, sort_set_id, record_int)
) partition by hash (table_int);

do $$
declare
    n integer := (select count(*) from pg_inherits where inhparent = 'data_record'::regclass);
begin
    perform data_create_hash_partitions('data_index_composite', n);
end $$;

create index data_index_composite_val_idx
    on data_index_composite (tenant_int, table_int, sort_set_id, val);
