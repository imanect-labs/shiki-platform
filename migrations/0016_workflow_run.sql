-- Phase 10 Task 10.2: run/step 永続化＋ワーカー（engine.md §2.2 の DDL スケッチが正）。
--
-- workflow_run = run の正。step_execution = step の正＝チェックポイントの粒度。
-- run_event = 追記ログ（(tenant_id, run_id, seq) で exactly-once）。
-- effect_journal = チョークポイント側の冪等記録（内部能力の副作用を高々 1 回・PIT-31）。
--
-- 規約: 全行 tenant_id not null・PK は tenant 先頭の複合キー（#91）。全クエリ tenant スコープ。

create table workflow_run (
    tenant_id       text        not null,
    run_id          uuid        not null default gen_random_uuid(),
    org             text        not null,
    -- 実行対象のワークフロー artifact（kind=workflow）と開始時にピンした版。
    workflow_id     uuid        not null,
    version         bigint      not null,
    -- トリガ種別（interactive / schedule / event）と実体化トリガ id（該当時）。
    trigger_kind    text        not null check (trigger_kind in ('interactive', 'schedule', 'event')),
    trigger_id      text,
    -- 実行主体（interactive=本人 subject local id / schedule|event=workflow プリンシパル）。
    principal       text        not null,
    -- 委譲者（schedule|event のみ・監査/失権検知に使う）。
    delegator       text,
    status          text        not null default 'queued'
                    check (status in ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    -- run 入力（≤256KB・超過は blob へ spill する運用だが Stage A はインライン）。
    input           jsonb       not null default '{}'::jsonb,
    -- 開始時にピンした IR のスナップショット（version 不変・ワーカーがノード params/retry を引く）。
    ir_snapshot     jsonb       not null default '{}'::jsonb,
    -- 協調キャンセル。
    cancel_requested boolean    not null default false,
    -- 失敗理由（delegation_invalid / run_timeout / node error 等）。
    fail_reason     text,
    -- OTel 相関。
    trace_id        text,
    started_at      timestamptz,
    finished_at     timestamptz,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now(),
    primary key (tenant_id, run_id)
);

-- queued な run を claim するワーカー走査（作成順）。
create index workflow_run_claim_idx
    on workflow_run (tenant_id, created_at)
    where status = 'queued';

create table step_execution (
    tenant_id       text        not null,
    run_id          uuid        not null,
    -- step の識別子。静的ノードは node_id、map 要素は `<map_id>[<index>].<node_id>`。
    step_path       text        not null,
    node_id         text        not null,
    status          text        not null default 'pending'
                    check (status in ('pending', 'ready', 'running', 'waiting_timer',
                                      'waiting_event', 'succeeded', 'failed', 'skipped')),
    attempt         int         not null default 0,
    -- ready の再スケジュール時刻（リトライ backoff / concurrency 順番待ち）。
    next_retry_at   timestamptz not null default now(),
    -- wait(duration/until) の起床時刻。
    wake_at         timestamptz,
    -- claim/リース（durable プリミティブ）。
    lease_owner     text,
    lease_expires_at timestamptz,
    fencing_token   bigint      not null default 0,
    -- 出力（≤256KB・超過 spill）と確定した出力ポート（terminal 遷移で確定・エッジ状態の導出元）。
    output          jsonb,
    taken_ports     text[]      not null default '{}',
    error           jsonb,
    -- 冪等キー `wf:{tenant_id}:{run_id}:{step_path}`（attempt 非依存）。
    idempotency_key text        not null,
    langfuse_trace_id text,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now(),
    primary key (tenant_id, run_id, step_path),
    foreign key (tenant_id, run_id)
        references workflow_run (tenant_id, run_id) on delete cascade
);

-- ready な step を claim する走査（次実行時刻順）。
create index step_ready_idx
    on step_execution (tenant_id, next_retry_at)
    where status = 'ready';

-- リース失効した running step の回収走査（sweeper）。
create index step_lease_idx
    on step_execution (tenant_id, lease_expires_at)
    where status = 'running';

-- wait_timer の起床走査（スケジューラ）。
create index step_wake_idx
    on step_execution (tenant_id, wake_at)
    where status = 'waiting_timer';

create table run_event (
    tenant_id   text        not null,
    run_id      uuid        not null,
    -- run ごと単調増加の seq（真実のソースの追記順序）。
    seq         bigint      not null,
    kind        text        not null,
    payload     jsonb       not null default '{}'::jsonb,
    created_at  timestamptz not null default now(),
    -- (tenant_id, run_id, seq) unique で追記を exactly-once に潰す（#91）。
    primary key (tenant_id, run_id, seq),
    foreign key (tenant_id, run_id)
        references workflow_run (tenant_id, run_id) on delete cascade
);

create table effect_journal (
    tenant_id       text        not null,
    -- 冪等キー（step 冪等キー、または script/skill 内の連番付き `#cN`）。
    idempotency_key text        not null,
    -- 操作のダイジェスト `sha256(api名＋正規化パラメータ)`（キー衝突かつ digest 不一致は permanent）。
    op_digest       text        not null,
    -- 記録済み結果の要約（再実行時に no-op で返す）。
    result_summary  jsonb       not null default '{}'::jsonb,
    created_at      timestamptz not null default now(),
    -- UNIQUE(tenant_id, idempotency_key) で内部能力の副作用を高々 1 回にする。
    primary key (tenant_id, idempotency_key)
);
