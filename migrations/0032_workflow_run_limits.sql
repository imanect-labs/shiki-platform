-- Phase 10 #199（10.5 完了）: run timeout の実行時強制＋queued promote の走査 index。
--
-- timeout_at = run 作成時に policies.run_timeout_sec から確定（scheduler tick が超過を失敗化）。

alter table workflow_run add column if not exists timeout_at timestamptz;

create index if not exists workflow_run_timeout_idx
    on workflow_run (timeout_at)
    where status = 'running';

-- max_parallel_runs の queued 滞留を古い順に promote する走査（engine.md §8.3）。
create index if not exists workflow_run_queued_idx
    on workflow_run (tenant_id, workflow_id, created_at)
    where status = 'queued';
