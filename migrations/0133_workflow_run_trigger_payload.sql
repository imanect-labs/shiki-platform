-- 手動の実行で試すきっかけの内容（D-93）。
--
-- 実行のダイアログで、スケジュールやファイル・テーブルの変更をきっかけにするワークフローを試すとき、
-- 入力とは別にきっかけの内容（`$from trigger` の源）を渡せるようにする。NULL なら今までどおり、きっかけの
-- 内容は run の入力と同じ（schedule / event の run は入力にペイロードを載せる・手動の run は入力そのもの）。
-- 既存の行は NULL のままなので、読み方は変わらない。
alter table workflow_run add column trigger_payload jsonb;
