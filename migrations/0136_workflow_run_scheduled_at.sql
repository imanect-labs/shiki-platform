-- スケジュールの run に予定の時刻を持たせる（GAP-15）。
--
-- スケジュールで動いた run の入力ときっかけの内容は null で、ワークフローが使える時刻は実行を始めた時刻（started_at・
-- 実行情報の date と started_at・D-97）だけだった。止まっていたあとの発火（catchup）や、多重度で待たされた run では、
-- 始めた時刻が予定の時刻（月曜 9:00 など）とずれ、「先週」「前日」を始めた日から数えると別の週・日になる。
--
--   scheduled_at = スケジュールの予定の時刻（schedule_occurrence.scheduled_at と同じ UTC の時刻）。スケジュールの run
--                  だけに入れ、手動の実行・できごとのきっかけ・別のワークフローから始めた run は null。実行情報の
--                  scheduled_at（`$from run`）は、null なら始めた時刻を返す。
--
-- 発火の記録（schedule_occurrence）は run を作った後に run_id を書くので、ワーカーが最初の step を取る時点では
-- まだ読めないことがある。予定の時刻は run を作る Tx で run の行に書く。
--
-- 前からある run は埋めない（null のまま・始めた時刻を返す）。

alter table workflow_run add column scheduled_at timestamptz;
