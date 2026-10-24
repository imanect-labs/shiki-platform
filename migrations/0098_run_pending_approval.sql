-- 検証 C-P1-3: 承認待ちのツール呼び出しを run に永続化する。
--
-- 承認要求（SSE `approval_requested`）を出す前に、待っているツール呼び出し（tool_call_id・ツール名・
-- 要求時刻）をここへ書く。承認 API はこの値と照合し（承認待ちでない・別の呼び出しの id への決定を
-- 黙って記録しない）、監査にはクライアントの申告ではなくこのツール名を残す。決定を拾って再開する
-- ときに消す。画面の再読込後も、どの呼び出しが承認待ちかを run から引ける。
alter table generation_run add column pending_approval jsonb;
