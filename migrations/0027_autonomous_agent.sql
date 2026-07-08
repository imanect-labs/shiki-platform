-- Phase 5 W5: 自律エージェントの起動フラグとワークスペース（Task 5.1/5.4/5.11）。
--
-- autonomous=ON の run は agent-core を **Autonomous プロファイル**（長ホライズン・フルツール・予算・
-- 計画・承認）で駆動する。ワークスペースは thread ごとの Drive フォルダ（workspace_folder_id）で、
-- file CRUD/shell の書込は StorageService 経由で版管理・監査・再索引に乗る（Durable Workspace）。

-- この生成 run が自律プロファイルか（既定 false＝チャット制約版・後方互換）。
alter table generation_run add column if not exists autonomous boolean not null default false;

-- thread ごとのワークスペースフォルダ（初回自律 run で lazy 作成し、以降再利用）。
alter table thread add column if not exists workspace_folder_id uuid;
