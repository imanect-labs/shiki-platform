-- 判断 API が期限内に受け付けた判断（BUG-45・GAP-08）。
--
-- 判断 API は、台帳の行をロックした Tx の中で期限（承認の期限と run の期限の早い方）を確かめてから承認者本人の
-- 権限で遷移し、遷移に成功した判断をこの列に書いて同じ Tx で commit する。期限の回収は台帳の行をロックしてから
-- 決めるので、受付の途中なら commit を待ち、この列があれば期限切れにせず、この判断で決める（承認の期限では、行は
-- 判断の状態・run は時間切れ、と分かれない）。期限を過ぎてから受付が行をロックしたら、遷移せずに断る。
-- run の期限（policies.run_timeout_sec）の回収はこの列を見ないので、run の期限では分かれ得る（engine.md §9.6・BUG-81）。
--
-- 形: {kind: approve|reject|return, to, by, rev, at}。停止（再実行・再開の待ち直しを含む）で NULL に戻す。
alter table workflow_approval add column accepted jsonb;
