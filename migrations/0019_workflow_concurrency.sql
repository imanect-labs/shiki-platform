-- Phase 10 Task 10.5: 並行制御（concurrency カウンタ＋wait タイマ・engine.md §8）。
--
-- concurrency_counter = 3 階層（global/workflow/node）の実行中カウント。claim 直後に
-- current < limit を満たす行だけ +1 予約し、超過は拒否でなく順番待ち（ready+backoff・engine.md §8.2）。
-- wait_subscription = wait ノードのタイマ／イベント待ち（発火時に対象 step を直接 terminal 化）。
--
-- 規約: 全行 tenant_id not null・複合 PK（#91）。

create table concurrency_counter (
    tenant_id  text        not null,
    -- スコープ種別（global=テナント全体 / workflow=ワークフロー単位 / node=ノード単位）。
    scope_kind text        not null check (scope_kind in ('global', 'workflow', 'node')),
    -- スコープキー（global=''、workflow=workflow_id、node=workflow_id|node_id）。
    scope_key  text        not null,
    -- 同時実行の上限。
    limit_n    int         not null check (limit_n >= 0),
    -- 現在の実行中カウント。
    current_n  int         not null default 0 check (current_n >= 0),
    updated_at timestamptz not null default now(),
    primary key (tenant_id, scope_kind, scope_key)
);

create table wait_subscription (
    tenant_id  text        not null,
    run_id     uuid        not null,
    step_path  text        not null,
    -- timer=時刻待ち / event=イベント待ち。
    kind       text        not null check (kind in ('timer', 'event')),
    -- timer: 発火時刻。event: null（source/filter は spec）。
    wake_at    timestamptz,
    -- event 待ちの source（storage.write 等）とフィルタ。
    source     text,
    spec       jsonb       not null default '{}'::jsonb,
    -- 消込済みフラグ（発火して terminal 化したら true）。
    fired      boolean     not null default false,
    created_at timestamptz not null default now(),
    primary key (tenant_id, run_id, step_path)
);

-- 満期タイマの走査（未消込のみ）。
create index wait_subscription_timer_idx
    on wait_subscription (wake_at)
    where kind = 'timer' and not fired;

-- イベント待ちのマッチ走査（未消込のみ）。
create index wait_subscription_event_idx
    on wait_subscription (tenant_id, source)
    where kind = 'event' and not fired;
