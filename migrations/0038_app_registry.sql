-- Phase 9 Task 9.13a: 汎用アーティファクト・レジストリ（不変 publish）。
--
-- ミニアプリ（mini_app_code）を内部レジストリへ不変 publish する。kind 列を持ち、
-- skill ストア（miniapp-platform.md §4）等へ将来そのまま流用できる汎用設計。
-- 同一 (tenant, kind, name, version) の再 publish は禁止（不変・yank のみ可）。
-- 規約: 全行 tenant_id not null（SaaS マルチテナント・#91）。

create table registry_entry (
    tenant_id       text        not null,
    id              uuid        not null default gen_random_uuid(),
    org             text        not null,
    -- 登録種別（artifact.kind と対応・skill 等へ流用するため汎用）。
    artifact_kind   text        not null,
    -- 公開名（tenant×kind 内で一意の識別子）。
    name            text        not null,
    -- semver（publish の一意キー）。
    version         text        not null,
    -- 実体の artifact（不変バージョン付き本文）。
    artifact_id     uuid        not null,
    artifact_version bigint     not null,
    -- マニフェスト/本文の digest（改竄検知・署名対象）。
    manifest_digest text        not null,
    -- 公開者（principal.id）。
    publisher       text        not null,
    -- 信頼ティア（first_party / in_house / marketplace）。
    trust_tier      text        not null,
    -- 署名（ed25519・first-party/オフラインインポート時・Task 9.13b で使用）。
    signature       bytea,
    -- 取り下げ（yank）フラグ。不変性を保ちつつ新規インストールを止める。
    yanked          boolean     not null default false,
    created_at      timestamptz not null default now(),
    primary key (tenant_id, id),
    -- 同一 (tenant, kind, name, version) は 1 度きり（不変 publish）。
    unique (tenant_id, artifact_kind, name, version)
);

-- 名前解決（最新 version の検索・インストール時に引く）。
create index registry_entry_name_idx
    on registry_entry (tenant_id, artifact_kind, name, created_at desc);
