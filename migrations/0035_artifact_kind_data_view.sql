-- Phase 9 Task 9.4: artifact.kind へ data_view（保存ビュー）を追加する。
--
-- 保存ビュー = 宣言的クエリ＋表示設定のバージョン付きアーティファクト（6.1 共通枠・
-- ReBAC 共有）。実行はクエリチョークポイント経由で、閲覧者本人の行述語・フィールド
-- マスクを常に再評価する（作成者の権限を引き継がない）。

-- 追加は NOT VALID で入れ、既存行の検証は VALIDATE で分離する（移行中の書込停止を避ける・
-- CodeRabbit 指摘）。CHECK の緩和（許可 kind の追加）なので既存行は必ず合格する。
alter table artifact drop constraint artifact_kind_check;
alter table artifact add constraint artifact_kind_check
    check (kind in ('workflow', 'ui_spec', 'mini_app', 'skill', 'script', 'data_view')) not valid;
alter table artifact validate constraint artifact_kind_check;
