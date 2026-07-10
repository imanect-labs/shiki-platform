-- Phase 9 Task 9.4: artifact.kind へ data_view（保存ビュー）を追加する。
--
-- 保存ビュー = 宣言的クエリ＋表示設定のバージョン付きアーティファクト（6.1 共通枠・
-- ReBAC 共有）。実行はクエリチョークポイント経由で、閲覧者本人の行述語・フィールド
-- マスクを常に再評価する（作成者の権限を引き継がない）。

alter table artifact drop constraint artifact_kind_check;
alter table artifact add constraint artifact_kind_check
    check (kind in ('workflow', 'ui_spec', 'mini_app', 'skill', 'script', 'data_view'));
