-- 監査ログの保持期間（#831）。
--
-- 既定 365 日。運用者がテナントごとに 90〜3650 日で変えられる（human 判断・2026-10-22）。
-- 期限を過ぎた行は日次で削除する。テナント撤去後も tombstone 行に残った値で削除を続ける
-- （撤去の記録 tenant.purge は消さない）。
alter table tenant
    add column if not exists audit_retention_days int not null default 365
        check (audit_retention_days between 90 and 3650);

-- 保持期間で消したチェーンの境目（tenant_id ＋ org ごとに 1 行）。
--
-- 実データを変える操作の行はハッシュチェーンでつながっている（#828）。先頭から消すと、残った
-- 先頭の行の prev_hash は消えた行を指す。消した最後の連結行の id と entry_hash をここに残し、
-- チェーンの検証はこの境目から始める（残った先頭の行の prev_hash が anchor_hash と一致すること）。
create table if not exists audit_chain_anchor (
    tenant_id   text        not null,
    org         text        not null,
    -- 消した最後の連結行の id と entry_hash。
    anchor_id   bigint      not null,
    anchor_hash text        not null,
    -- これまでに保持期間で消した行数（連結行・連結しない行の合計）。
    purged_rows bigint      not null default 0,
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, org)
);
