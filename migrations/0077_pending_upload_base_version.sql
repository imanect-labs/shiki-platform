-- 内容更新（既存ファイルの新版アップロード）の楽観ロック（#827）。
--
-- ブラウザでテキストを編集して保存するとき、開いた版（base_version）を declare で申告する。
-- finalize の版更新を `version = base_version` の条件付き UPDATE にし、開いた後に他の保存が
-- 入っていれば 409 にする（黙って上書きしない）。NULL は従来どおり無条件の新版（アップロード
-- ダイアログからの置き換え・AI の書込など）。
alter table pending_upload
    add column if not exists base_version bigint;
