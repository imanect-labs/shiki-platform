-- スレッドの進行中 run を引く部分索引（サイドバー履歴の「実行中」表示・thread の SELECT 列 run_status）。
--
-- 履歴の一覧はスレッドごとに「非端末の run があるか」を引く。既存の generation_run_thread_idx は
-- status を持たないため、run の多いスレッドでは全 run の行を読んで status を確かめることになる。
-- 進行中の run は全体のごく一部なので、非端末だけの部分索引にして索引だけで引けるようにする。
create index if not exists generation_run_live_thread_idx
    on generation_run (tenant_id, thread_id, created_at)
    where status in ('queued', 'running', 'waiting_approval');
