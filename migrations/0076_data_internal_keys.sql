-- Phase 15 Task 15.1: 内部整数キー（tenant_int / table_int / record_int）＋ tenant → pool の継ぎ目。
--
-- 設計（docs/data-platform.md §3.2・§4・§12 第 2 段・§13 M1）:
--   * 索引テーブル（Task 15.2）は 1 レコードにつき複数行を持つため、キー幅がそのまま容量に効く。
--     外部 ID（uuid / tenant_id text）はそのまま残し、内部キーとして整数を足す。
--   * **採番は不変・再利用しない**。行を消してもシーケンスは戻らない。
--   * **`record_int` は外へ出さない**（API・SSE・監査・OpenFGA は uuid のまま）。プール共通の連番は
--     他テナントの作成ペースを推測させるため（PIT-75）。
--   * `tenant_int` と `table_int` は**採番サービス経由**（`crates/storage/src/tenant.rs`）で採る。
--     列 default にしないのは、プール分割（第 2 段）後に各プールのシーケンスが独立して
--     衝突するのを避けるため（採番は常にレジストリのプールで行う・docs/data/shard-safety.md）。
--   * フィールド識別子の 3 層化（JSONB キーを `f{id}` へ書き換え）は**テーブル単位のジョブ**
--     （`crates/data/src/key_migration.rs`・jobq）で行う。ここでは状態列 `key_format` を足すだけ。

-- ---------------------------------------------------------------------------
-- ① tenant → (tenant_int, pool) の配置レジストリ
-- ---------------------------------------------------------------------------
-- `tenant` 台帳はプロビジョニング（管理 API）経由の行しか持たない。構造化データは台帳未登録の
-- テナント（単一テナント構成・テスト）でも使われるため、配置は別表で**必要時に採番**する。
-- 行は物理削除しない（tenant_int を再利用しないための tombstone・PURGE_RETAINED）。
create sequence tenant_int_seq as integer minvalue 1;

create table tenant_placement (
    tenant_id  text        not null primary key,
    -- 内部整数キー（int4・不変・再利用しない）。採番は tenant.rs の ensure_placement のみ。
    tenant_int integer     not null,
    -- 所属プール（第 2 段のプール分割の継ぎ目）。現状は 'default' の 1 プールのみ。
    pool_id    text        not null default 'default'
        check (pool_id ~ '^[a-z0-9_-]{1,64}$'),
    created_at timestamptz not null default now()
);
create unique index tenant_placement_int_idx on tenant_placement (tenant_int);

-- 既存テナント（台帳 ∪ 構造化データを持つテナント）へ作成順に採番する。
insert into tenant_placement (tenant_id, tenant_int)
select tenant_id, nextval('tenant_int_seq')
from (
    select tenant_id, min(created_at) as first_seen
    from (
        select tenant_id, created_at from tenant
        union all
        select tenant_id, created_at from data_table
    ) s
    group by tenant_id
    order by first_seen, tenant_id
) ordered;

-- ---------------------------------------------------------------------------
-- ② data_table: tenant_int / table_int / フィールド識別子の形式
-- ---------------------------------------------------------------------------
-- table_int はグローバル連番（後のパーティションキー・Task 15.2）。採番はレジストリ側の
-- シーケンスから行い、列 default にはしない（上記）。
create sequence data_table_int_seq as bigint minvalue 1;

alter table data_table
    add column tenant_int    integer,
    add column table_int     bigint,
    -- フィールド識別子の形式: 1 = v1（JSONB キー = フィールド名）/ 2 = v2（JSONB キー = "f{id}"）。
    -- 既存行は 1。移行ジョブがテーブル単位で 2 へ進める（一方向・冪等）。新規作成は 2。
    add column key_format    smallint not null default 1
        check (key_format in (1, 2)),
    -- 次に採番するフィールド ID（不変・再利用しない。削除済みフィールドの ID も再利用しない
    -- ためスキーマ JSONB の外に持つ）。0..15 はシステム擬似列に予約・上限 32767（int2）。
    add column next_field_id integer  not null default 16
        check (next_field_id between 16 and 32768);

update data_table t
   set tenant_int = p.tenant_int
  from tenant_placement p
 where p.tenant_id = t.tenant_id;

with ordered as (
    select tenant_id, id, row_number() over (order by created_at, tenant_id, id) as rn
    from data_table
)
update data_table t
   set table_int = o.rn
  from ordered o
 where o.tenant_id = t.tenant_id and o.id = t.id;

-- 既存の最大値の次から採番する（行が無ければ 1 から）。
select setval(
    'data_table_int_seq',
    greatest((select max(table_int) from data_table), 1),
    (select count(*) > 0 from data_table)
);

alter table data_table
    alter column tenant_int set not null,
    alter column table_int  set not null,
    alter column key_format set default 2;

create unique index data_table_table_int_idx on data_table (table_int);

-- ---------------------------------------------------------------------------
-- ③ data_record: tenant_int / table_int / record_int
-- ---------------------------------------------------------------------------
-- record_int はプール共通のシーケンス（テーブルごとの連番は採番行への挿入集中で詰まるため
-- 採らない・data-platform §3.2）。ADD COLUMN の volatile default は既存行にも 1 行ずつ評価され、
-- **既存レコードへ採番される**。一意性は (tenant_int, table_int, record_int) で担保する。
create sequence data_record_int_seq as bigint minvalue 1 cache 100;

alter table data_record
    add column tenant_int integer,
    add column table_int  bigint,
    add column record_int bigint not null default nextval('data_record_int_seq');

alter sequence data_record_int_seq owned by data_record.record_int;

update data_record r
   set tenant_int = t.tenant_int,
       table_int  = t.table_int
  from data_table t
 where t.tenant_id = r.tenant_id and t.id = r.table_id;

alter table data_record
    alter column tenant_int set not null,
    alter column table_int  set not null;

-- 索引テーブルから本体を引く経路（Task 15.2 以降のページ本体取得はこちら）。
create unique index data_record_int_idx on data_record (tenant_int, table_int, record_int);
