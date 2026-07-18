-- Task 11.6: WOPI ロック（Collabora 編集セッションの助言的ロック・PIT-44）。
--
-- WOPI 準拠の 30 分 TTL・lazy 解放（期限切れは次アクセス時に無視/削除）。
-- ロックは「編集排他」ではなく「AI を提案保存へ迂回させるシグナル」（PIT-44）。
-- ロック存在＝セッション実在ではない（クラッシュ残留があり得る）ため、認可の
-- 真実源にはしない（認可は毎 WOPI 呼び出しの OpenFGA check・fail-closed）。
--
-- tenant_id を保持し、全クエリで tenant スコープを強制する（隔離境界・day-1）。
create table office_lock (
    file_id    uuid primary key,
    lock_id    text not null,
    locked_by  text not null,
    tenant_id  text not null,
    expires_at timestamptz not null
);
