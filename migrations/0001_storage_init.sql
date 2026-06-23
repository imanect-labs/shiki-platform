-- Phase 1 ストレージ基盤のスキーマ（docs/roadmap/phase-1.md Task 1.1/1.2/1.9）。
--
-- 設計上の不変条件:
--   * 全テーブルは org スコープ（SaaS マルチテナント前提・PIT-14 で blob は org 名前空間）。
--   * 実体（バイト）は ObjectStore に content-addressing で保存し、メタ/ツリーは Postgres。
--     ノードに直接バイトを持たせない（blob_sha256 参照のみ）。
--   * 階層は closure table で表現し、移動/継承クエリを高速化する（PIT-16: move は単一 txn）。
--   * 監査ログは append-only（アプリ経路）。改竄耐性は過大主張しない（PIT-12）。

-- ---------------------------------------------------------------------------
-- blob: org スコープの content-addressed ストア（重複排除 + 参照カウント）。
-- PIT-14: 同一バイトでも org が異なれば別行（hash 存在オラクル/越境参照破壊を防ぐ）。
-- object_key は ObjectStore 上のキー（'{org}/{sha256}'）。
-- ---------------------------------------------------------------------------
create table blob (
    org          text        not null,
    sha256       text        not null,            -- 内容ハッシュ（hex 小文字・64桁）
    size_bytes   bigint      not null check (size_bytes >= 0),
    content_type text        not null default 'application/octet-stream',
    object_key   text        not null,            -- ObjectStore 上のキー '{org}/{sha256}'
    refcount     bigint      not null default 0 check (refcount >= 0),
    created_at   timestamptz not null default now(),
    primary key (org, sha256)
);

-- refcount=0 の GC 候補スキャン用（参照ゼロになった blob を将来回収する）。
create index blob_gc_idx on blob (org) where refcount = 0;

-- ---------------------------------------------------------------------------
-- node: フォルダ + ファイル（closure-table ツリー・ファイルは葉）。
-- parent_id が NULL のノードは org ルート直下（Phase 1 はファイルのみ・フォルダは 1.5）。
-- ---------------------------------------------------------------------------
create table node (
    id           uuid        primary key default gen_random_uuid(),
    org          text        not null,
    tenant_id    text        not null,
    kind         text        not null check (kind in ('folder', 'file')),
    name         text        not null,
    parent_id    uuid        references node (id),
    -- ファイルの実体参照（フォルダは NULL）。blob への複合 FK。
    blob_sha256  text,
    size_bytes   bigint,
    content_type text,
    -- バージョニングの種（履歴テーブルは 1.7）。更新ごとに増やす。
    version      bigint      not null default 1,
    -- 論理削除（ゴミ箱）。
    deleted_at   timestamptz,
    created_by   text        not null,             -- 作成ユーザー id（subject）
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    constraint node_blob_fk
        foreign key (org, blob_sha256) references blob (org, sha256),
    -- ファイルは blob を必ず参照し、フォルダは参照しない。
    constraint node_kind_payload check (
        (kind = 'file' and blob_sha256 is not null and size_bytes is not null)
        or
        (kind = 'folder' and blob_sha256 is null)
    )
);

-- 同一フォルダ内の名前一意（生存ノードのみ・org＋tenant スコープ）。
-- 論理削除済みは衝突対象外（復元時にアプリが再衝突チェックする）。
-- tenant_id を含める: 同一 org 識別子を複数 tenant が共有しても、tenant をまたいで
-- 同名作成をブロックしない（全サービスクエリは org＋tenant でスコープするため整合）。
-- NULLS NOT DISTINCT: ルート直下（parent_id IS NULL）でも NULL を同値扱いして
-- 同名を弾く（Postgres は既定で NULL を相異なる値として扱い一意が効かないため。要 PG15+）。
create unique index node_sibling_name_uidx
    on node (org, tenant_id, parent_id, name) nulls not distinct
    where deleted_at is null;

create index node_parent_idx on node (org, parent_id) where deleted_at is null;
create index node_blob_idx on node (org, blob_sha256);

-- ---------------------------------------------------------------------------
-- node_closure: 祖先/子孫（自分自身を depth 0 で含む）。
-- 配下一括取得・パンくず・move の整合に使う（PIT-16: move は祖先ロック下の単一 txn）。
-- ---------------------------------------------------------------------------
create table node_closure (
    org        text not null,
    ancestor   uuid not null references node (id),
    descendant uuid not null references node (id),
    depth      int  not null check (depth >= 0),
    primary key (ancestor, descendant)
);

create index node_closure_desc_idx on node_closure (descendant);
create index node_closure_anc_idx on node_closure (ancestor);

-- ---------------------------------------------------------------------------
-- pending_upload: 二相アップロードの宣言状態（declare → presigned PUT → finalize）。
-- declare 時に作成し、finalize で内容ハッシュ検証→昇格後に削除する。
-- 中断（finalize されない）行は将来 staging GC と併せて掃除する。
-- ---------------------------------------------------------------------------
create table pending_upload (
    upload_id       uuid        primary key default gen_random_uuid(),
    org             text        not null,
    tenant_id       text        not null,
    parent_id       uuid        references node (id),
    name            text        not null,
    content_type    text        not null,
    declared_sha256 text        not null,
    declared_size   bigint      not null check (declared_size >= 0),
    staging_key     text        not null,
    created_by      text        not null,
    created_at      timestamptz not null default now()
);

-- ---------------------------------------------------------------------------
-- audit_log: 全データ操作と認可判定の構造化記録（Task 1.9）。
-- append-only at app path（PIT-12: DB レベルの不変性は主張しない）。
-- prev_hash/entry_hash はハッシュチェーンの種（将来の整合検証用・best-effort）。
-- trace_id は OTel と共有し Langfuse 突合の土台にする（design §4.9）。
-- ---------------------------------------------------------------------------
create table audit_log (
    id          bigserial   primary key,
    tenant_id   text        not null,
    org         text        not null,
    actor       text        not null,              -- 実行ユーザー id（subject）
    action      text        not null,              -- 'file.upload_url.issue' 等
    object_type text        not null,              -- 'file' | 'folder' | 'blob'
    object_id   text        not null,
    decision    text        not null check (decision in ('allow', 'deny')),
    trace_id    text,
    metadata    jsonb       not null default '{}'::jsonb,
    prev_hash   text,
    entry_hash  text,
    created_at  timestamptz not null default now()
);

create index audit_log_object_idx on audit_log (org, object_type, object_id, created_at);
create index audit_log_actor_idx on audit_log (org, actor, created_at);
