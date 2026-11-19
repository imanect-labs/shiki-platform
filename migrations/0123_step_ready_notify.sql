-- #1334: step が実行可能（status = 'ready'）になったら、待機中の workflow ワーカーを起こす。
--
-- ワーカーは claim が空振りすると眠り、この通知（jobq の待機と同じチャネル `jobq`・ペイロードは
-- `workflow_step_ready`）か、次の ready step の next_retry_at で起きる（crates/jobq の Waker）。
-- 以前は 200ms ごとに claim を打ち続け、アイドル時の DB 問い合わせの大半を占めていた。
--
-- ready にする SQL は起動・前進（advance）・運用操作（ops）・並行上限の順番待ち（limits）・リース
-- 失効回収・待機明けなどに散らばっているので、各 SQL に通知を足さず、表のトリガ 1 か所で拾う
-- （足し忘れた経路があると、その step は次の保険の起床まで待たされる）。
--
-- NOTIFY はコミット時に配られ、同じトランザクション内の同じ通知は 1 通にまとめられる（ready を
-- 大量に作る前進でも通知は 1 通）。ready 以外への更新（claim の running 化など・ホットパス）では
-- WHEN 句で関数を呼ばない。

create or replace function step_execution_notify_ready() returns trigger
language plpgsql as $$
begin
    perform pg_notify('jobq', 'workflow_step_ready');
    return null;
end $$;

create trigger step_execution_ready_insert
    after insert on step_execution
    for each row when (new.status = 'ready')
    execute function step_execution_notify_ready();

create trigger step_execution_ready_update
    after update of status, next_retry_at on step_execution
    for each row when (new.status = 'ready')
    execute function step_execution_notify_ready();
