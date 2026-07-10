-- Phase 9 Task 9.6/9.13: ミニアプリ・インストール台帳（同意付与スコープ＋Keycloak client）。
--
-- 公開 API ゲートウェイの二重ゲート第2段が参照する正本: リクエストの Bearer トークンの
-- azp（=登録 client_id）からこの行を引き、granted_scopes（同意で付与＝requested の部分集合）
-- と所有アプリ（app_id）を得る。granted_scopes を毎リクエスト突合することで、同意の失効
-- （revoked / scope 縮小）が即時反映される（token の scope クレームだけに依存しない）。
--   * B1 = public client（authcode+PKCE・secret なし）／B2 = confidential client（token-exchange）。
--   * 実インストール/プロビジョンは Task 9.13b（PR9）が単一 Tx＋補償で行う。本 PR は台帳＋
--     ゲートウェイ参照のみ（開発時は直接 insert で fixture）。
--   * 規約: 全行 tenant_id not null・PK は tenant 先頭の複合キー（#91・SaaS マルチテナント）。

create table app_installation (
    tenant_id         text        not null,
    id                uuid        not null default gen_random_uuid(),
    org               text        not null,
    -- インストール対象ミニアプリ（mini_app_code artifact id＝registry の解決先）。
    app_id            uuid        not null,
    -- 表示/監査用のアプリ名・バージョン（registry_entry 由来）。
    app_name          text        not null,
    installed_version text        not null,
    -- 同意で付与されたスコープ（granted ⊆ requested・二重ゲートの scope 上限）。
    granted_scopes    text[]      not null default '{}',
    -- Keycloak クライアント id（B1=public+PKCE / B2=confidential）。未登録は NULL。
    client_id_b1      text,
    client_id_b2      text,
    -- ライフサイクル（active=有効 / revoked=アンインストール済み・token 有効期限内でも 403）。
    status            text        not null default 'active',
    -- インストールを同意した管理者（principal.id）。
    installed_by      text        not null,
    created_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now(),
    primary key (tenant_id, id),
    -- テナント内で 1 アプリ 1 インストール（再インストールは同一行更新）。
    unique (tenant_id, app_id)
);

-- ゲートウェイの azp → インストール解決（B1/B2 それぞれの client_id で引く）。
-- partial index で NULL client を除外し、登録済み client のみ即時解決する。
create index app_installation_client_b1_idx
    on app_installation (tenant_id, client_id_b1)
    where client_id_b1 is not null;
create index app_installation_client_b2_idx
    on app_installation (tenant_id, client_id_b2)
    where client_id_b2 is not null;
