-- データ変更の wait(event) の受付の突合（BUG-42）。
--
-- 行に束縛した wait は停止時に行の rev（suspend_rev）を読んでから checkpoint を commit するまでの間に、テーブル
-- 単位の wait は outbox の位置（after_event_id）を読んでから commit するまでの間に、購読がまだ見えないまま relay が
-- 配送済みにした変更を取りこぼし、timeout まで起きなかった。承認の受付の突合（workflow_approval.reconciled_at）と
-- 同じ形で、購読の commit の後に 1 回だけ、取りこぼし得た変更を突き合わせ直す。
--
-- reconciled_at = 突合を済ませた時刻。突合の要らない購読（ファイルの保存の wait・承認・時刻の wait）は作る時点で
-- 印を付け、走査の索引に載せない。既存の未発火の購読も同じく印を付ける（データ変更の wait は 1 回だけ突き合わせる）。
-- 発火済みの購読は走査も索引も見ず、待ち直すときは UPSERT が印を付け直すので書き換えない（run の保持期間ぶんの
-- 履歴を 1 つの Tx で書き換えない・BUG-81）。

alter table wait_subscription add column reconciled_at timestamptz;

update wait_subscription set reconciled_at = now()
 where not fired
   and (kind <> 'event' or coalesce(source, '') not like 'data.%' or spec ? 'approval');

-- 突合の走査（未発火で、まだ突き合わせていないものだけ・作成の古い順）。
create index wait_subscription_reconcile_idx
    on wait_subscription (created_at)
    where kind = 'event' and not fired and reconciled_at is null;
