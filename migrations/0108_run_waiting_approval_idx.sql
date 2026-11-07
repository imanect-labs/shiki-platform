-- ホームの「今日のこと」: 自分の承認を待っている run の一覧（#965）。
--
-- 承認できるのは run の actor 本人だけ（chat::store::approval の confused-deputy 対策）なので、
-- 一覧は tenant_id ＋ actor で引く。承認待ちの run は全体のごく一部なので部分インデックスにする。
create index if not exists generation_run_waiting_actor_idx
    on generation_run (tenant_id, actor)
    where status = 'waiting_approval';
