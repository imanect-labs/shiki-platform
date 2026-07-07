-- Phase 10 Task 10.3: トリガ（スケジューラ＋イベントマッチング・engine.md §5）。
--
-- schedule_occurrence = スケジュール発火の冪等記録（(workflow_id, scheduled_at) unique で
-- 二重投入を防ぐ・PIT-31）。trigger_firing = イベント発火の冪等記録（(trigger_id, event_id)
-- unique・outbox 1 件につき最大 1 run）。scheduler_lease = リーダーリース（単一行 CAS・多重発火防止）。
--
-- 規約: 全行 tenant_id not null（scheduler_lease を除く・これはインスタンス横断の単一リーダー）。

create table schedule_occurrence (
    tenant_id    text        not null,
    workflow_id  uuid        not null,
    trigger_id   text        not null,
    -- tz 解決後の UTC 論理時刻。
    scheduled_at timestamptz not null,
    -- 作成した run（skip 溢れ時は null）。
    run_id       uuid,
    created_at   timestamptz not null default now(),
    -- (workflow_id, trigger_id, scheduled_at) unique で同一 occurrence の二重投入を防ぐ。
    primary key (tenant_id, workflow_id, trigger_id, scheduled_at)
);

create table trigger_firing (
    tenant_id  text        not null,
    trigger_id text        not null,
    -- outbox イベント id（冪等キー）。
    event_id   bigint      not null,
    run_id     uuid,
    created_at timestamptz not null default now(),
    -- (trigger_id, event_id) unique で outbox 1 件につき最大 1 run。
    primary key (tenant_id, trigger_id, event_id)
);

-- リーダーリース（インスタンス横断で単一のスケジューラループだけがループを回す・engine.md §5.1）。
-- 単一行（id=1）を CAS で奪い合う。tenant を跨ぐグローバル調停のため tenant_id は持たない。
create table scheduler_lease (
    id          int         primary key default 1 check (id = 1),
    owner       text        not null,
    expires_at  timestamptz not null
);
