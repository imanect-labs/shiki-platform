-- 消したテーブルの OpenFGA のタプルを撤去したかを残す（BUG-149）。
--
-- テーブルの削除は論理削除（deleted_at）で、これまでは OpenFGA の data_table のタプル（持ち主・共有・委譲）を
-- 残していた。残ったタプルは見えるテーブルの数を水増しし、作って消すだけで ListObjects の上限（1,000 件）に
-- 近づく。削除の commit の後にその場で撤去し、撤去を終えた時刻を authz_purged_at に残す
-- （crates/data/src/table_purge.rs）。撤去に失敗したもの・この版より前に消したもの（authz_purged_at が空のまま）は、
-- サーバーの掃除が古い順に撤去する。

alter table data_table add column authz_purged_at timestamptz;

-- 掃除が引く「消したのに撤去していない」行（消した順）。
create index data_table_authz_purge_idx
    on data_table (deleted_at)
    where deleted_at is not null and authz_purged_at is null;
