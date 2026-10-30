-- 取り込みの記録が無いファイルのまとめての取り込み（D-126・GAP-51）。
--
-- テナントの管理者が管理コンソールから始める。社内文書の検索を有効にする前に置いたファイルや、前の版の不具合で
-- 取り込みを飛ばしたファイルを、取り込みの待ち行列が溜まらないよう少しずつ outbox へ積む（1 回で全部は積まない）。
-- 進めるのは取り込みのリーダー（crates/rag/src/pipeline/backfill.rs）で、1 回の手順は 1 Tx（積んだイベントと
-- 続きの位置を同じ Tx で書く）なので、中断しても続きから進む。
--
-- - after_id: 確かめた最後のファイル（node.id の順に読む）。
-- - files_total: 始めたときのファイルの数（進み具合の分母・ごみ箱と AI エージェントの作業用のフォルダーを除く）。
-- - files_scanned・files_queued: 確かめた数と、取り込みに回した数。
-- - 同じ組織で動いているものは 1 つまで（部分一意索引）。
create table rag_backfill (
    id            bigserial   primary key,
    tenant_id     text        not null,
    org           text        not null,
    requested_by  text        not null,
    status        text        not null check (status in ('running', 'done')),
    after_id      uuid,
    files_total   bigint      not null,
    files_scanned bigint      not null default 0,
    files_queued  bigint      not null default 0,
    trace_id      text,
    started_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    finished_at   timestamptz
);

create unique index rag_backfill_running_idx on rag_backfill (tenant_id, org) where status = 'running';
create index rag_backfill_latest_idx on rag_backfill (tenant_id, org, started_at desc);
-- 進める順（いちばん長く進めていないもの）。
create index rag_backfill_due_idx on rag_backfill (updated_at, id) where status = 'running';

-- まとめての取り込みが、テナント・組織の生きているファイルを id の順に少しずつ読む（ほかのテナントの行を読まない）。
-- 分母の数え上げもこの索引だけで済む。
create index node_file_scan_idx on node (tenant_id, org, id)
    where kind = 'file' and deleted_at is null and system = false;
