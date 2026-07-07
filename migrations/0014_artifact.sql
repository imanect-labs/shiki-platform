-- Phase 6 Task 6.1（Phase 10 Stage A で前倒し・#121）: 共有可能アーティファクト共通基盤。
--
-- prompt template / UI スペック / ミニアプリ / ワークフロー IR / skill / script を統一的に扱う
-- 共通枠。**不変バージョン追記方式**: artifact_version は INSERT のみで、過去バージョンは
-- 変更されない（version カウンタの正は artifact.current_version）。
-- 権限の正本は OpenFGA の artifact 型（owner/editor/viewer・thread と同型の非階層共有）。
--
-- 規約: 全行 tenant_id not null・PK は tenant 先頭の複合キー（#91）。
-- テナント消去（SAAS.2）は artifact を tenant_id で削除すれば version へ CASCADE する。

create table artifact (
    tenant_id       text        not null,
    id              uuid        not null default gen_random_uuid(),
    org             text        not null,
    -- 種別（閉じた集合）。Stage A は workflow のみ使用、他は Phase 6/9/10 Stage B の予約。
    kind            text        not null check (kind in (
        'workflow', 'prompt_template', 'ui_spec', 'mini_app', 'skill', 'script'
    )),
    -- tenant×kind 内で一意の参照名（ワークフロー name 等・ir.md §2 の一意性は artifact 層が担保）。
    name            text        not null,
    -- 作成者（subject の local id ＝ principal.id）。OpenFGA の owner タプルが権限の正本。
    owner           text        not null,
    -- 最新バージョン番号（artifact_version.version の単一の正・追記ごとに +1）。
    current_version bigint      not null default 0,
    -- 論理削除（一覧・名前解決から除外。バージョン履歴は保持）。
    deleted_at      timestamptz,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now(),
    primary key (tenant_id, id)
);

-- 名前解決（保存時検証 V4 やワークフロー起動 API が name → artifact を引く）。
-- 生存行のみ一意（削除後の名前再利用を許す）。
create unique index artifact_name_idx
    on artifact (tenant_id, kind, name)
    where deleted_at is null;

-- 自分のアーティファクト一覧（kind 絞り込み・更新日降順・生存のみ）。
create index artifact_owner_idx
    on artifact (tenant_id, org, owner, kind, updated_at desc)
    where deleted_at is null;

create table artifact_version (
    tenant_id   text        not null,
    artifact_id uuid        not null,
    -- 追記ごとに +1 の不変バージョン（1 始まり）。
    version     bigint      not null,
    -- バージョンの本文（IR・テンプレート等の JSON）。追記後は変更されない。
    body        jsonb       not null,
    -- この版を作成した subject（principal.id）。
    created_by  text        not null,
    created_at  timestamptz not null default now(),
    primary key (tenant_id, artifact_id, version),
    foreign key (tenant_id, artifact_id)
        references artifact (tenant_id, id) on delete cascade
);
