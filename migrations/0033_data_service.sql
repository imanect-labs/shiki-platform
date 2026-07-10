-- Phase 9 Task 9.2: 構造化データサービス（スキーマレジストリ＋ record 共有テーブル）。
--
-- 設計（docs/design.md §4.10・docs/roadmap/phase-9.md）:
--   * **ランタイム DDL を打たない**: アプリごとの CREATE TABLE はせず、全テナント・全テーブルの
--     行を data_record に JSONB で格納する。フィルタ/ソートは宣言フィールドへの式インデックスで賄う。
--   * 権限の第1層はテーブル ReBAC（OpenFGA `data_table` 型・viewer/editor/owner）。
--     第2層の行レベル述語（row_policy）は Task 9.3 で追加する（スキーマ JSONB 内に保持）。
--   * 規約: 全行 tenant_id not null・PK は tenant 先頭の複合キー（#91）。
--     テナント消去（SAAS.2）は data_table の削除で record / registry へ CASCADE する。

create table data_table (
    tenant_id      text        not null,
    id             uuid        not null default gen_random_uuid(),
    org            text        not null,
    -- 所有ミニアプリ（Task 9.13 の同意インストールが設定。NULL = スタンドアロンテーブル）。
    app_id         uuid,
    -- tenant×org 内で一意の参照名（ミニアプリのマニフェストがこの名前で束縛する）。
    name           text        not null,
    -- TableSchema JSON（fields/validations。9.3 以降で row_policy / field_policy /
    -- aggregate_min_rows / fsm_ref を同じ JSONB に追記する）。正本は crates/data の型。
    schema         jsonb       not null,
    -- スキーマ改訂カウンタ（additive 変更ごとに +1。式インデックスの再適用判定に使う）。
    schema_version bigint      not null default 1,
    -- 作成者（principal.id）。権限の正本は OpenFGA の owner タプル。
    created_by     text        not null,
    -- 論理削除（アンインストール時の archive・Task 9.13）。
    deleted_at     timestamptz,
    created_at     timestamptz not null default now(),
    updated_at     timestamptz not null default now(),
    primary key (tenant_id, id)
);

-- 名前解決（生存行のみ一意・削除後の名前再利用を許す）。
create unique index data_table_name_idx
    on data_table (tenant_id, org, name)
    where deleted_at is null;

-- アプリ所有テーブルの束縛検索（Task 9.8 の「所有テーブルのみ」リソース束縛が引く）。
create index data_table_app_idx
    on data_table (tenant_id, app_id)
    where deleted_at is null and app_id is not null;

create table data_record (
    tenant_id  text        not null,
    id         uuid        not null default gen_random_uuid(),
    table_id   uuid        not null,
    org        text        not null,
    -- レコード本体（フィールド名 → 値の JSONB。型検証はサーバ書込時に強制）。
    data       jsonb       not null,
    -- 楽観ロック用リビジョン（1 始まり・更新ごとに +1。競合は 409）。
    rev        bigint      not null default 1,
    -- レコード所有者（principal.id・行ポリシー `$user.id` 述語の既定材料）。
    owner      text        not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (tenant_id, id),
    foreign key (tenant_id, table_id)
        references data_table (tenant_id, id) on delete cascade on update cascade
);

-- テーブル内一覧の既定並び（updated_at 降順 keyset）。
create index data_record_table_idx
    on data_record (tenant_id, table_id, updated_at desc, id desc);

-- 式インデックスの台帳。宣言フィールド → 実インデックス名の対応を持ち、
-- スキーマ改訂時の差分適用（作成・不要になったものの削除）を冪等にする。
create table data_index_registry (
    tenant_id  text        not null,
    table_id   uuid        not null,
    -- インデックス対象のフィールド名（TableSchema.fields[].name）。
    field      text        not null,
    -- 実 DDL のインデックス名（決定的命名 dr_<table_id 短縮>_<field ハッシュ>）。
    index_name text        not null,
    -- インデックス種別（btree_text / btree_numeric / gin / unique_text 等）。
    kind       text        not null,
    created_at timestamptz not null default now(),
    primary key (tenant_id, table_id, field),
    foreign key (tenant_id, table_id)
        references data_table (tenant_id, id) on delete cascade on update cascade
);

-- インデックス名はクラスタ全体で一意（PostgreSQL の制約と一致させる）。
create unique index data_index_registry_name_idx
    on data_index_registry (index_name);
