-- Phase 11-pre Task 11P.1: Yjs 共同編集ドキュメントの永続化（docs/design.md §4.8.1 / PIT-37）。
--
-- 設計上の不変条件:
--   * 真実は Yjs ドキュメント（update log ＋ snapshot）。md はシリアライズ形式（Task 11P.2）。
--   * update log は追記型で無限肥大するため（PIT-37①）、一定件数ごとに snapshot へ圧縮し
--     取り込み済み update 行を削除する。ロード = snapshot ＋ 残 update の適用。
--   * tenant_id / org を day-1 から保持する（SaaS 隔離境界・アンビエント権限の禁止）。
--   * authz はドキュメント単位＝対応する node の ReBAC（file:<id> の viewer/editor）を
--     接続時＋定期に再チェックする（PIT-37②）。本テーブルに独自の権限は持たせない。

-- ---------------------------------------------------------------------------
-- collab_doc: ノード 1 ファイルにつき 1 行。snapshot は yrs update v1 エンコーディング
-- （全状態を 1 update に merge したもの）。snapshot_seq はそれが取り込んだ最終 seq。
-- ---------------------------------------------------------------------------
create table collab_doc (
    node_id      uuid        primary key references node (id) on delete cascade,
    org          text        not null,
    tenant_id    text        not null,
    -- 全状態 snapshot（yrs update v1）。NULL は「まだ snapshot 無し＝update log が全て」。
    snapshot     bytea,
    -- snapshot に取り込み済みの最終 update seq（これ以下の collab_update は削除済み）。
    snapshot_seq bigint      not null default 0,
    -- 次に発番する update seq（単調増加・hub 直列化の永続カウンタ）。
    next_seq     bigint      not null default 1,
    -- md シリアライズ保存で反映済みの node.version（Task 11P.2 の外部書込検出に使う）。
    saved_node_version bigint,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);

-- テナント内一覧・GC スキャン用。
create index collab_doc_tenant_idx on collab_doc (tenant_id, org);

-- ---------------------------------------------------------------------------
-- collab_update: 追記型 update log（yrs update v1）。snapshot 圧縮時に
-- seq <= snapshot_seq の行を削除する。author は監査用（AI 編集は AI 主体名義）。
-- ---------------------------------------------------------------------------
create table collab_update (
    node_id    uuid        not null references collab_doc (node_id) on delete cascade,
    seq        bigint      not null,
    payload    bytea       not null,
    author     text        not null,
    created_at timestamptz not null default now(),
    primary key (node_id, seq)
);
