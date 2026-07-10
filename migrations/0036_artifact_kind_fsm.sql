-- Phase 9 Task 9.10: artifact.kind へ fsm（宣言的 status 遷移ガード）を追加する。
--
-- 旧「軽量FSMエンジン」は廃止（miniapp-platform.md §1）。FSM は data サービスの
-- 宣言的ガード（record の status フィールド＋遷移認可）へ縮退し、副作用（通知/転記/AI）は
-- Phase 10 の workflow-engine へ委譲する（遷移コミット → outbox イベント → トリガ）。
-- 遷移認可は Task 9.3 の行述語（PolicyExpr）を actor として再利用する。

alter table artifact drop constraint artifact_kind_check;
alter table artifact add constraint artifact_kind_check
    check (kind in ('workflow', 'ui_spec', 'mini_app', 'skill', 'script', 'data_view', 'fsm'));
