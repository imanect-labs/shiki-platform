-- Phase 2 Task 2.8: 汎用ジョブキュー（crates/jobq）。
--
-- pgmq は採用せず vanilla Postgres（SKIP LOCKED ＋ visibility timeout）で自作する。
-- 理由: 拡張依存ゼロの可搬性（オンプレ持込 Postgres / マネージド PG の拡張 allowlist /
-- エアギャップ）と、Phase 10 workflow-engine（Postgres 上の自作 Durable Execution）と
-- 同一系譜の自前プリミティブに育てるため。
--
-- 役割分離:
--   * storage_event_outbox = ドメイン書込と同一 txn の耐久イベントログ兼 fan-out 点。
--   * job_queue             = per-consumer の配送機構（vt / リトライ / DLQ、消費後 DELETE）。
-- relay（crates/rag pipeline）が outbox → job_queue を同一 Postgres 内・単一 txn でコピーする。
--
-- 配信セマンティクス: at-least-once。
--   * claim: visible_at <= now() の行を FOR UPDATE SKIP LOCKED で確保し、
--     visible_at を now()+vt に進める（可視性タイムアウト）。attempts はここで +1。
--   * ack   = DELETE。fail = visible_at をバックオフ分延長（attempts >= max_attempts で DLQ へ）。
--   * consumer がクラッシュしても vt 経過で自動再配信される。

create table job_queue (
    id           bigserial   primary key,
    -- キュー名（'rag_ingest' など）。将来の chat run / 資料生成ジョブ / fan-out も同テーブル。
    queue        text        not null,
    tenant_id    text        not null,
    payload      jsonb       not null,
    -- attempts は claim 時にインクリメント（=配信試行回数）。
    attempts     int         not null default 0,
    max_attempts int         not null default 5,
    -- 可視時刻。未来 = 配信抑止（claim 済み or バックオフ待ち）。
    visible_at   timestamptz not null default now(),
    trace_id     text,
    enqueued_at  timestamptz not null default now()
);

-- claim の走査経路（queue 別・可視時刻順）。id を include し index-only で FIFO を保つ。
create index job_queue_claim_idx on job_queue (queue, visible_at) include (id);

-- DLQ: max_attempts 消化したジョブの終着点。人手または管理 API で requeue する。
create table job_queue_dead (
    id           bigint      primary key,          -- 元 job_queue.id を引き継ぐ
    queue        text        not null,
    tenant_id    text        not null,
    payload      jsonb       not null,
    attempts     int         not null,
    max_attempts int         not null,
    trace_id     text,
    enqueued_at  timestamptz not null,
    died_at      timestamptz not null default now(),
    last_error   text        not null
);

create index job_queue_dead_queue_idx on job_queue_dead (queue, died_at desc);
