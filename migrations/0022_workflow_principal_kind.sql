-- Phase 10 Stage A 結線: workflow_run に実行主体の種別を持つ（W3）。
--
-- interactive run は起動ユーザーの権限で、schedule/event run は workflow プリンシパルの委譲権限で
-- 実行する。ワーカーがノード実行時に正しい AuthContext（user:… / workflow:…）を組めるよう、
-- run 開始時に確定した principal の種別を保持する。既定 'user'（既存行・セッション互換）。
alter table workflow_run
    add column principal_kind text not null default 'user'
        check (principal_kind in ('user', 'workflow'));
