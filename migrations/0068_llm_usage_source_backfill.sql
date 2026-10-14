-- 既存の llm_usage 行に利用元を付ける（0067 で列を追加・#553）。
--
-- 判別できるのはミニアプリ経由（app_id あり）だけで、残りは 'other' のまま。0067 と分けているのは、
-- 列・制約の追加（ACCESS EXCLUSIVE）を先にコミットして解放し、ここでは行ロックだけで更新するため
-- （会計の INSERT を全件更新のあいだ止めない）。
update llm_usage set source = 'miniapp' where app_id is not null and source = 'other';
