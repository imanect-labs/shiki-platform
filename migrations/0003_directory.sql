-- Phase 1 共有: 共有ダイアログの相手検索を支えるディレクトリ（Task 1.10 / #20）。
--
-- 設計上の不変条件:
--   * 全テーブルは tenant_id + org スコープ（SaaS マルチテナント前提）。検索は呼び出し元の
--     AuthContext の tenant_id（＋ org）で必ず pre-filter し、テナント越境の発見を防ぐ。
--   * これは「最小のディレクトリ」。dev では dev_seed が投入し、本番の正本ユーザー provisioning
--     （Keycloak からの同期・部署/ロール）は roadmap SAAS.2 / #76 が担う。ここは検索可能な
--     プロフィール（email / 表示名）の射影に限る。
--   * email / display_name は検索対象のため NOT NULL。user_id は OIDC `sub`。
create table directory_user (
    user_id      text        not null,
    tenant_id    text        not null,
    org          text        not null,
    email        text        not null,
    display_name text        not null,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    -- 同一ユーザーは 1 テナント 1 行（テナント跨ぎの同一 sub は別レコード扱い）。
    primary key (tenant_id, user_id)
);

-- 検索の pre-filter（tenant_id, org）＋ keyset 並び（email, user_id）を支える複合インデックス。
create index directory_user_search_idx
    on directory_user (tenant_id, org, email, user_id);
