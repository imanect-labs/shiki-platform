-- ノードの最終更新者（updated_by・Task 11P.10）。
-- 「誰が最後に編集したか」をドライブ一覧/詳細/バージョン履歴に表出するための列。
-- 既存行は created_by で backfill（作成＝最初の更新）。以後の全書込メソッドが設定する
-- （内容更新/リネーム/移動/削除/復元／AI 編集は AI 主体名義）。
alter table node
    add column updated_by text not null default '' ;

-- backfill: 既存ノードの最終更新者を作成者とみなす。
update node set updated_by = created_by where updated_by = '';

-- 以後は明示設定を必須にする（空文字の既定は backfill 用の一時措置）。
alter table node
    alter column updated_by drop default;
