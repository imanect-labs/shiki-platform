-- 会話の AI の試運転（try_workflow・T04・D-197）の台帳。
--
-- 会話の AI は、保存したワークフローの版を、承認カードを経て話した本人の権限で試しに実行する。試運転 1 回に 1 行。
-- 二重起動の主な歯止めは承認カード（承認は呼び出し 1 つに 1 つ）で、この台帳は次のための補助である。
--
-- - 同じ会話・同じ話した人・同じワークフローの同じ版・同じ入力（input_hash）の前の行の run が実行中・順番待ちなら、
--   新しい run を作らずに合流する。終わっていれば、rerun が無い限り前回の結果を返す。
-- - 新しい run は、会話の 1 回の生成（chat_run_id）あたり 5 本まで（行の数で数える）。
-- - 試運転の LLM の費用は、観測を返すたびに、まだ会話の予算に積んでいない差分だけを積む（reported_*）。
--
-- 行は起動の前に入れる（run_id = null は起動の途中）。起動に失敗した行・起動されなかった行（on_trigger_overflow =
-- skip）は消す。null のまま 2 分を過ぎた行は古い予約として、次の試運転のときに消す。
--
--   seq           = 会話の 1 回の生成の中の通し番号（1 から）。
--   invoked_by    = 話した人（principal.id）。共有した会話で別の人が起動した run には合流しない。
--   input_hash    = sha256（キーを並べ直した {input, trigger, scheduled_at}）の 16 進。
--   run_id        = 起動した workflow_run（null = 起動の途中）。実行履歴の GC で run が消えても行は残る
--                   （消えた run は行が無いものとして扱う）ので、外部キーは張らない。
--   chat_trace_id = 会話の生成の trace_id（監査・観測と相関する）。

create table workflow_trial_run (
    tenant_id            text        not null,
    org                  text        not null,
    thread_id            uuid        not null references thread (id) on delete cascade,
    chat_run_id          uuid        not null references generation_run (run_id) on delete cascade,
    seq                  int         not null check (seq >= 1),
    invoked_by           text        not null,
    workflow_id          uuid        not null,
    version              bigint      not null check (version >= 1),
    input_hash           text        not null,
    run_id               uuid,
    chat_trace_id        text,
    reported_tokens      bigint      not null default 0 check (reported_tokens >= 0),
    reported_cost_micros bigint      not null default 0 check (reported_cost_micros >= 0),
    created_at           timestamptz not null default now(),
    primary key (tenant_id, chat_run_id, seq)
);

-- 前の行を引く（同じ会話・同じ話した人・同じ版・同じ入力の新しい順）。
create index workflow_trial_run_key_idx on workflow_trial_run
    (tenant_id, thread_id, invoked_by, workflow_id, version, input_hash, created_at desc);

-- RLS（0114 と同じポリシー・移行モード）。新しいテナント表は、作る migration で付ける（docs/guides/db-maintenance.md §5）。
alter table workflow_trial_run enable row level security;
alter table workflow_trial_run force row level security;
create policy shiki_tenant_isolation on workflow_trial_run
    using ((select shiki_rls_tenant()) is null or tenant_id = (select shiki_rls_tenant()))
    with check ((select shiki_rls_tenant()) is null or tenant_id = (select shiki_rls_tenant()));
