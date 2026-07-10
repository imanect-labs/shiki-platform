-- Phase 9 Task 9.1: artifact.kind へ mini_app_code（コードベース・ミニアプリ）を追加する。
--
-- Phase 6 の mini_app（A=宣言的）に対し、mini_app_code（B=コードベース）はマニフェストを
-- body に持つ。A/B は同一テーブル・同一 version＋ReBAC＋監査経路に乗る（design §4.10）。
-- CHECK 緩和のため NOT VALID＋VALIDATE で移行中の書込停止を避ける。

alter table artifact drop constraint artifact_kind_check;
alter table artifact add constraint artifact_kind_check
    check (kind in ('workflow', 'ui_spec', 'mini_app', 'skill', 'script', 'data_view', 'fsm', 'mini_app_code')) not valid;
alter table artifact validate constraint artifact_kind_check;
