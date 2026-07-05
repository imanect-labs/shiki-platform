-- Phase 1 ストレージ: バージョニング（Task 1.7）と書込イベント発行（Task 1.8）。
--
-- 設計上の不変条件:
--   * 版記録・イベント発行は StorageService の書込メソッド内で、メタ書込と同一 txn。
--   * 全テーブルは org + tenant_id スコープ（SaaS マルチテナント前提）。
--   * 版カウンタは node.version を単一の正とする。node_version には「内容を持つ版」
--     （create / 内容更新 / 版復元）のみ記録する（rename/move はメタ版で blob 変化なし）。
--   * refcount = node_version 行数（1 版 = 1 参照）。soft-delete では減らさない（LbvQZ）。

-- ---------------------------------------------------------------------------
-- node_version: ファイル内容版の履歴（Task 1.7）。
-- content-addressing により同一内容の版は同じ blob を共有する（破壊的編集の安全網）。
-- 版番号は node.version と一致し、内容変化が無い rename/move 版は欠番になる（正常）。
-- ---------------------------------------------------------------------------
create table node_version (
    node_id      uuid        not null references node (id),
    version      bigint      not null,            -- その時点の node.version
    org          text        not null,
    tenant_id    text        not null,
    blob_sha256  text        not null,            -- この版の内容（content-addressed）
    size_bytes   bigint      not null,
    content_type text        not null,
    author       text        not null,            -- この版を作成した subject
    created_at   timestamptz not null default now(),
    primary key (node_id, version),
    -- 版が参照する blob は必ず実在する（bump_blob 後に記録）。
    constraint node_version_blob_fk
        foreign key (org, blob_sha256) references blob (org, sha256)
);

-- 履歴一覧（新しい版から）を高速に引く。
create index node_version_node_idx on node_version (node_id, version desc);

-- 既存ファイルノードを現行版で backfill する（過去の内容版は存在しないため現状を 1 行）。
insert into node_version
    (node_id, version, org, tenant_id, blob_sha256, size_bytes, content_type, author, created_at)
select id, version, org, tenant_id, blob_sha256, size_bytes,
       coalesce(content_type, 'application/octet-stream'), created_by, created_at
from node
where kind = 'file' and blob_sha256 is not null;

-- ---------------------------------------------------------------------------
-- pending_upload に内容更新の対象を追加（Task 1.7）。
-- NULL = 新規ファイル作成（既存挙動）、Some = 既存ファイルへの新版アップロード。
-- ---------------------------------------------------------------------------
alter table pending_upload
    add column target_node_id uuid references node (id);

-- ---------------------------------------------------------------------------
-- storage_event_outbox: 書込ドメインイベントの outbox（Task 1.8）。
-- 書込と同一 txn で INSERT し、購読側（Phase 2 ingestion）が at-least-once で消費する。
-- pgmq への relay / DLQ / リトライは消費者がいる Phase 2（Task 2.8）で配線する。
-- ---------------------------------------------------------------------------
create table storage_event_outbox (
    id           bigserial   primary key,
    org          text        not null,
    tenant_id    text        not null,
    node_id      uuid        not null,
    version      bigint      not null,            -- (node_id, version) が冪等キー
    op           text        not null check (op in (
        'create', 'update', 'rename', 'move', 'delete', 'restore'
    )),
    actor        text        not null,            -- 実行ユーザー id（subject）
    trace_id     text,                            -- OTel と共有し増分索引を同一トレースに紐付け
    -- 消費者の利便/冪等のための詳細（kind, blob_sha256, parent 詳細, subtree_count 等）。
    payload      jsonb       not null default '{}'::jsonb,
    created_at   timestamptz not null default now(),
    -- ack 済みフラグ。NULL = 未処理。claim → 処理 → mark_processed の順で at-least-once。
    processed_at timestamptz
);

-- 未処理イベントを FIFO（id 昇順）で取り出す（poll）。
create index storage_event_outbox_unprocessed_idx
    on storage_event_outbox (id) where processed_at is null;
