-- データ変更トリガ（GAP-07）の土台: イベント発火の結果と再試行・run の重複防止・因果の鎖・候補の索引。
--
-- trigger_firing:
--   outcome         = 1 イベント × 1 トリガの結果（pending は起動の一時失敗で再試行を待つ・D-21）。
--                     既存の行は従来どおり「発火済み」。run を作れなかった既存の行は打ち切り扱いにする。
--   attempts / next_attempt_at / payload / cause_run_id = 再試行の材料（起動したら payload は消す）。
-- workflow_run:
--   trigger_event_id = 発火元の outbox イベント。(trigger_id, trigger_event_id) の部分一意で、run の作成と
--                      firing の記録の間に落ちても、再配送・再試行で run を二重に作らない（PIT-31）。
--   trigger_chain / cause_run_id = 因果の鎖（祖先ワークフロー）と由来の run。自分の書込で自分を起こす
--                      循環と、連鎖の深さを抑止する（D-26）。workflow.start の子にも引き継ぐ。

alter table trigger_firing
    add column outcome text not null default 'fired'
        check (outcome in ('pending', 'fired', 'skipped', 'suppressed_cycle', 'suppressed_depth',
                           'dropped_queue_full', 'gave_up')),
    add column attempts int not null default 0,
    add column next_attempt_at timestamptz,
    add column payload jsonb,
    add column cause_run_id uuid;

update trigger_firing set outcome = 'gave_up' where run_id is null;

-- 再試行の走査（時刻の来た pending だけ）。
create index trigger_firing_retry_idx
    on trigger_firing (next_attempt_at)
    where outcome = 'pending';

alter table workflow_run
    add column trigger_event_id bigint,
    add column trigger_chain uuid[] not null default '{}',
    add column cause_run_id uuid;

create unique index workflow_run_trigger_event_uq
    on workflow_run (tenant_id, trigger_id, trigger_event_id)
    where trigger_event_id is not null;

-- queued の上限の判定（1 ワークフローの queued 件数）は既存の workflow_run_queued_idx（0032）を使う。

-- データ変更トリガの候補（テーブル束縛）。
create index workflow_trigger_table_idx
    on workflow_trigger (tenant_id, source, (spec->'scope'->>'table'))
    where enabled and kind = 'event';

-- データ変更の wait(event) の候補（テーブル・行の束縛）。
create index wait_subscription_record_idx
    on wait_subscription (tenant_id, source, (spec->'scope'->>'table'), (spec->'scope'->>'record'))
    where kind = 'event' and not fired;
