-- 監査ログのハッシュの版（#828）。
--
-- 版 2 から作成時刻（created_at）をハッシュに含める。既存の行は版 1（作成時刻を含まない）の
-- まま検証する（記録済みの行は書き換えない）。新しい行はアプリが版 2 を明示して書く。
alter table audit_log
    add column if not exists hash_version smallint not null default 1
        check (hash_version in (1, 2));
