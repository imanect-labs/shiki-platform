-- Phase 6 UX: エージェントモードのワークスペース作成場所を利用者が選べるようにする。
--
-- 既定（NULL）は Drive 直下に agent-workspace-<thread> を作る（0027 の現行挙動）。
-- 「親フォルダを選んで配下に新規作成」を選んだ場合、その親をここに保存し、初回自律 run の
-- ensure_workspace がこの親の下にワークスペースフォルダを作る。
-- 「既存フォルダをそのままワークスペースにする」場合は 0027 の workspace_folder_id を直接設定する
-- ため、この列は使わない（両立可能・排他ではない）。
alter table thread add column if not exists workspace_parent_folder_id uuid;
