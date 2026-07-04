-- テナントレジストリ（SAAS.2 / #87）。プロビジョニング/削除のライフサイクル正本。
--
-- 設計上の不変条件:
--   * tenant_id は FGA 識別子/オブジェクトキーの名前空間（`<type>:<tenant_id>|<local>` /
--     `{tenant_id}/{org}/...`）に使われるため、禁止文字（`| : # @` 空白）は API 層の
--     validate_tenant_id で拒否済みの値のみが入る。
--   * 行は物理削除しない tombstone 方式: 削除済みは status='deleted' を残し、tenant_id の
--     再利用による名前空間衝突（旧テナントの残骸と新テナントの混線）を防ぐ。
--   * status: active（稼働中）→ deleting（撤去処理中・途中失敗は再実行で収束）→ deleted。
create table tenant (
    tenant_id    text        not null primary key,
    org          text        not null,
    display_name text        not null,
    status       text        not null default 'active'
        check (status in ('active', 'deleting', 'deleted')),
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);
