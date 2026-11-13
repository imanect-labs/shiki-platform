-- D-10 T09: 会話の対象（ワークフロー・ミニアプリ）と目的（app_builder）を thread に持つ。
--
-- ワークフローエディタ・ミニアプリの画面の「AI に依頼」パネルから作った会話は、開いていた対象
-- （origin_artifact_*）を持つ。ワーカーは run の開始時に、その対象を**発話者の権限で**読み直し、
-- 「この会話は〇〇の編集画面に紐づく」と system に注記する（読めなければ注記しない）。
-- /apps の「AI と作る」から始めた会話は、対象を持たずに purpose = 'app_builder' を持つ。
-- 対象か目的を持つ会話は、最初から作成用の予算（chat.app_builder.*）で動く。
--
-- 表示名（origin_artifact_name）は作成時点の名前を非正規化して持つ（履歴の表示用・改名には追随しない）。
-- 注記に使う名前と版は run の開始時に読み直すので、ここの名前は信じない。
--
-- 注: 0047 の origin_note_* と同じく表示・グルーピング・誘導のためのメタで、認可の真実源ではない。
-- 対象の閲覧・編集の権限は artifact の ReBAC で、会話の閲覧権とは別（暗黙に共有しない・fail-closed）。
alter table thread add column if not exists origin_artifact_id uuid;
alter table thread add column if not exists origin_artifact_kind text;
alter table thread add column if not exists origin_artifact_name text;
alter table thread add column if not exists purpose text;

alter table thread drop constraint if exists thread_origin_artifact_kind_check;
alter table thread add constraint thread_origin_artifact_kind_check
    check (origin_artifact_kind is null or origin_artifact_kind in ('workflow', 'mini_app'));

-- id と種類は揃って持つ（片方だけの行を作らない）。
alter table thread drop constraint if exists thread_origin_artifact_pair_check;
alter table thread add constraint thread_origin_artifact_pair_check
    check ((origin_artifact_id is null) = (origin_artifact_kind is null));

-- 文書の会話（origin_note_id）と同時には持たない（どちらの画面の会話か 1 つに決める）。
alter table thread drop constraint if exists thread_origin_one_check;
alter table thread add constraint thread_origin_one_check
    check (origin_note_id is null or origin_artifact_id is null);

alter table thread drop constraint if exists thread_purpose_check;
alter table thread add constraint thread_purpose_check
    check (purpose is null or purpose in ('app_builder'));

-- 対象の画面の会話一覧（テナント内・その対象・未削除・更新日降順）を引く。0047 と同じ理由で CONCURRENTLY は使わない。
create index if not exists thread_origin_artifact_idx
    on thread (tenant_id, org, origin_artifact_id, updated_at desc)
    where origin_artifact_id is not null and deleted_at is null;
