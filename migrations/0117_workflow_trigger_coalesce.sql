-- ファイル保存トリガの「続く保存を 1 回にまとめる」（BUG-28・D-38）。
--
-- 新しい版の保存（Office・ノートの自動保存など）は、同じファイルの保存が続く間は起動せず、途切れてから
-- 最後の保存で 1 回だけ起動する。待っている間の発火は trigger_firing の pending（next_attempt_at = 起動する
-- 時刻）として持ち、起動の再試行と同じ経路（retry_pending_firings）で起こす。
--
-- trigger_firing:
--   outcome        = coalesced を足す（後の保存にまとめた）。
--   coalesce_key   = まとめる単位（ファイルの id）。待っている pending を引く。
--   coalesce_since = まとめ始めた保存の時刻（保存が途切れなくても、ここから上限の時間で起動する）。

alter table trigger_firing drop constraint trigger_firing_outcome_check;
alter table trigger_firing
    add constraint trigger_firing_outcome_check
        check (outcome in ('pending', 'fired', 'skipped', 'suppressed_cycle', 'suppressed_depth',
                           'dropped_queue_full', 'gave_up', 'coalesced')),
    add column coalesce_key text,
    add column coalesce_since timestamptz;

-- 同じトリガ・同じファイルで待っている発火を引く。
create index trigger_firing_coalesce_idx
    on trigger_firing (tenant_id, trigger_id, coalesce_key)
    where outcome = 'pending' and coalesce_key is not null;
