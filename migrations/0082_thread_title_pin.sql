-- サイドバー履歴の自動タイトル・ピン留め。
--
-- title_pending: タイトルが仮（作成時に入れた発話の切り詰め・文書名など）で、生成ワーカーが
--   最初の発話から要約タイトルを付けてよい状態。生成で確定したら false。ユーザーが改名したら
--   false（以後は自動で上書きしない）。生成の書き込みは `WHERE title_pending` の条件付き更新に
--   するので、生成中に改名されても改名が勝つ。既存の行は false（遡って生成しない）。
-- pinned_at: ピン留めした時刻（NULL＝ピン留めなし）。一覧の先頭の「ピン留め」に新しい順で出す。
--   一覧は owner 本人のものなので thread 列で持つ（共有相手ごとのピンは持たない）。
alter table thread
    add column title_pending boolean     not null default false,
    add column pinned_at     timestamptz;

-- ピン留めの一覧（本人・生存・ピン留めのみ）。
create index thread_owner_pinned_idx
    on thread (tenant_id, org, owner, pinned_at desc)
    where deleted_at is null and pinned_at is not null;
