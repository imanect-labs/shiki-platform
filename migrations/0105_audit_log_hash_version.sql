-- 監査ログのハッシュの版（#828）。
--
-- 版 2 から作成時刻（created_at）をハッシュに含める。既存の行は版 1（作成時刻を含まない）の
-- まま検証する（記録済みの行は書き換えない）。新しい行はアプリが版 2 を明示して書く。
--
-- 定数 default の列追加はメタデータだけで済む。CHECK は NOT VALID で付けて既存行の全件検証を
-- 避け（新しい行は検証される）、audit_log への書き込みを長く止めない（0067 と同じ）。既存行は
-- すべて default の 1 なので、VALIDATE しなくても制約に反する行は無い。
alter table audit_log
    add column hash_version smallint not null default 1;
alter table audit_log
    add constraint audit_log_hash_version_check
        check (hash_version in (1, 2)) not valid;
