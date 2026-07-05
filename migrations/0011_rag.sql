-- Phase 2: RAG チャンクメタ（Task 2.2）とインジェスト・ジョブ状態（Task 2.8）。
--
-- 設計上の不変条件（docs/design.md §4.3）:
--   * チャンク本文の正本はこのテーブル。Qdrant / Tantivy には ID＋検索用データのみを持たせ、
--     move（authz_tags 再評価）や再索引を本文再取得なしで行えるようにする。
--   * authz_tags は名前空間化形式（`file:<tenant>|<id>` / `folder:<tenant>|<祖先id>`）のまま
--     格納する（PIT-1 (b) 権限定義オブジェクト方式。local へ剥がすと tenant 境界が消える）。
--   * chunk を OpenFGA オブジェクトにしない（PIT-7）。post-filter は file 粒度で行い、
--     chunk → file 対応はこのテーブルが持つ。
--   * embedding_model_version は埋め込み対象（leaf / table）にのみ付与し、
--     インデックス単位の version 固定（PIT-8 shadow index）と突合する。

create table rag_chunk (
    -- uuid5(node_id, version, ordinal) による決定的 ID。再インジェストは同 ID 上書き＝冪等。
    id            uuid        primary key,
    tenant_id     text        not null,
    org           text        not null,
    node_id       uuid        not null,
    version       bigint      not null,
    -- small-to-big: 検索は leaf/table、文脈提示は parent。親チャンク行は parent_id が null。
    parent_id     uuid,
    kind          text        not null check (kind in ('parent', 'leaf', 'table')),
    -- 文書内の出現順（node_id, version 内で一意）。
    ordinal       int         not null,
    page          int,
    -- 見出しの階層パス（例: {はじめに, 背景}）。引用表示とチャンク文脈に使う。
    heading_path  text[]      not null default '{}',
    content       text        not null,
    char_count    int         not null,
    authz_tags    text[]      not null,
    embedding_model_version text,
    created_at    timestamptz not null default now(),
    unique (node_id, version, ordinal)
);

-- 差替え（版更新・削除）とハイドレーションの走査経路。
create index rag_chunk_node_idx on rag_chunk (tenant_id, node_id, version);
-- small-to-big の親引き。
create index rag_chunk_parent_idx on rag_chunk (parent_id);

-- ---------------------------------------------------------------------------
-- rag_ingest_job: インジェストのドメイン状態（進捗・失敗の可視化＋冪等）。
-- 配送（リトライ/DLQ）は job_queue が担い、本テーブルは「どの版がどう処理されたか」の記録。
-- (tenant_id, node_id, version, op) が冪等キー: 同一版・同一 op の二重処理を防ぐ。
-- ---------------------------------------------------------------------------
create table rag_ingest_job (
    id         bigserial   primary key,
    tenant_id  text        not null,
    org        text        not null,
    node_id    uuid        not null,
    version    bigint      not null,
    op         text        not null,
    status     text        not null check (status in (
        'running', 'succeeded', 'skipped', 'failed', 'dead'
    )),
    attempts   int         not null default 0,
    last_error text,
    trace_id   text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (tenant_id, node_id, version, op)
);

-- 失敗ジョブ・進行中ジョブの一覧（運用可視化）。
create index rag_ingest_job_status_idx on rag_ingest_job (status, updated_at desc);
