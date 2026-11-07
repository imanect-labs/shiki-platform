-- 会話の検索フォルダ（ホームの「社内検索」でフォルダを選んで始めた会話・#965）。
--
-- この会話の社内文書検索（doc_search / doc_read）を、このフォルダの配下に絞る。絞り込みだけで
-- 権限は広げない（検索結果はこれまでどおり pre-filter と OpenFGA の post-filter を通る）。skill の
-- 知識スコープがあるときは両者の交差で絞る。名前は作成時点の表示名（改名には追随しない）。
alter table thread add column if not exists search_folder_id uuid;
alter table thread add column if not exists search_folder_name text;
