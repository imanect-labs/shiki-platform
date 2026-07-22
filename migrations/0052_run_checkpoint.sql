-- 自律 run のチェックポイント永続化（#351）。
--
-- agent-core の Checkpoint（計画・消費・剪定後の履歴・ループ検出器）をステップ境界で
-- durable run 行へ保存する。ワーカーが run を claim/takeover した際にここから読み出して
-- `run_agent(..., resume, ...)` へ渡し、プロセス落ちでも計画・予算消費・失敗履歴を失わない。
-- durability はステップ境界のみ（生成途中の LLM ストリームは保存しない・design §4.4.1）。
-- 書込は fencing 一致時のみ（ゾンビ書込拒否）。端末確定（finalize）で NULL に落とす。
alter table generation_run add column if not exists checkpoint jsonb;
