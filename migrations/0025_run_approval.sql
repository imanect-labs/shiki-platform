-- Phase 5 W3: 自律エージェントの承認ゲート（Task 5.6/5.10）。
--
-- 破壊系/egress/高コスト操作は実行前に停止し、run を waiting_approval にしてユーザーの承認を待つ。
-- 承認/却下は run_approval に記録し（誰が・いつ・どのツール呼び出しを）、監査と突合できる（NFR-6）。
-- 承認待ちの run は生成ワーカーが（ハートビートでリースを延長しつつ）ブロックし、決定 or キャンセルで再開する。

-- generation_run.status に waiting_approval を追加する（承認待ちの中断状態）。
alter table generation_run drop constraint if exists generation_run_status_check;
alter table generation_run add constraint generation_run_status_check
    check (status in ('queued', 'running', 'waiting_approval', 'done', 'failed', 'cancelled'));

-- 承認判定台帳。1 ツール呼び出し（tool_call_id）につき高々 1 決定（承認 or 却下）。
create table run_approval (
    run_id       uuid        not null references generation_run (run_id) on delete cascade,
    org          text        not null,
    tenant_id    text        not null,
    -- 承認対象のツール呼び出し（agent-core の ToolUse id）。
    tool_call_id text        not null,
    -- 承認を求めたツール名・入力プレビュー（監査・突合用）。
    tool_name    text        not null,
    decision     text        not null check (decision in ('approved', 'rejected')),
    -- 決定したユーザー（principal.id）。誰が許可/却下したかを監査に残す。
    decided_by   text        not null,
    decided_at   timestamptz not null default now(),
    -- (run_id, tool_call_id) で 1 決定に潰す（二重承認/却下の競合を拒否）。
    primary key (run_id, tool_call_id)
);

-- 待機中ワーカーが決定をポーリングで拾う経路（run 単位）。
create index run_approval_run_idx on run_approval (run_id, decided_at);
