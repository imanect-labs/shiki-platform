-- Phase 15 Task 15.12（監査 B1-2）: オプティマイザの単価較正用の固定表。
--
-- 起動時の較正ベンチ（crates/data/src/query/planner/cost.rs）は以前、共有 DB 上で一時表を作り
-- CREATE INDEX を打っていた（15.3 の「ランタイム DDL ゼロ」に反する）。表と索引をここで一度だけ作り、
-- 較正は読取だけにする。
--
-- - **テナントに属さないシステム表**（顧客データを含まない・既知の合成データだけ）。tenant 条件を持たず、
--   テナント消去（purge_tenant）の対象でもない。shard 安全 lint では「システム表」として許可リストに載せる。
-- - 全プール（シャード）で同じ内容（マイグレーションは各プールに適用される）。較正はレジストリのプールで読む。
-- - 行数・値の形は cost.rs の CALIB_ROWS / 計測 SQL と一致させること（変えるなら新しい migration で作り直す）。
create table data_planner_calib (
    k int4 not null,
    v text collate "C" not null,
    r int8 not null,
    d jsonb not null
);

insert into data_planner_calib
select 1, 'v' || (g % 97), g,
       jsonb_build_object('a', g, 'b', repeat('x', 160), 'c', g % 13)
  from generate_series(1, 20000) g;

create index data_planner_calib_val on data_planner_calib (k, v, r);
create unique index data_planner_calib_pk on data_planner_calib (r);

analyze data_planner_calib;
