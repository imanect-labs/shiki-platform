-- Phase 1 共有: role/部署の相手検索を支えるロールディレクトリ（#76・role 共有）。
--
-- 設計上の不変条件（directory_user と同一方針。migrations/0003_directory.sql と対）:
--   * 全行は tenant_id + org スコープ（SaaS マルチテナント前提）。検索は呼び出し元の
--     AuthContext の tenant_id（＋ org）で必ず pre-filter し、テナント越境の発見を防ぐ。
--   * これは「最小のロールディレクトリ」= 共有ダイアログのオートコンプリート/表示用の射影。
--     ロールメンバーシップの正本は OpenFGA の role タプル（role:<tenant>|<id>#member@user:...）で、
--     本テーブルは「どの role が存在し何と表示するか」だけを持つ（メンバー一覧は持たない）。
--   * role_id は Keycloak の role/group 由来（AD の OU/部署を含む）。group パスは `/` を含み得るため
--     text。FGA 側は role:<tenant>|<role_id>#member として名前空間化される（区切りは `|`）。
--   * 本番の正本 provisioning（Keycloak SCIM/group フル同期）は SK.6。現状はログイン時の
--     claim 同期（api callback）と dev_seed が投入する。
create table directory_role (
    role_id      text        not null,
    tenant_id    text        not null,
    org          text        not null,
    display_name text        not null,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    -- 同一ロールは 1 テナント 1 行。
    primary key (tenant_id, role_id)
);

-- 検索の pre-filter（tenant_id, org）＋ keyset 並び（display_name, role_id）を支える複合インデックス。
create index directory_role_search_idx
    on directory_role (tenant_id, org, display_name, role_id);
