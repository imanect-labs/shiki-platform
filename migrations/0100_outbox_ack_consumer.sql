-- 3 回目の検証 3-D-1: `processed_at` で ack するコンシューマ（RAG の relay）の登録台帳。
--
-- outbox の GC は「登録済みの全台帳コンシューマへ配送済み」に加えて `processed_at IS NOT NULL` を
-- 必須にしていた。`processed_at` を立てるのは RAG の relay だけなので、RAG を無効にした配備では
-- 1 行も GC されず、構造化データのレコード変更（Phase 15 から outbox に載る）で outbox が無制限に
-- 増えていた。`processed_at` を待つべきかを「この配備で RAG の relay が動いたことがあるか」で決める
-- ために、台帳コンシューマ（`outbox_consumer`）と同じく登録を永続化する。RAG の relay は起動時に
-- ここへ登録し、GC はここに 1 行でもあれば `processed_at` を待つ（無ければ待たない）。
create table outbox_ack_consumer (
    name          text        primary key,
    registered_at timestamptz not null default now()
);

-- 既存の配備で RAG の relay が動いたことがあれば、登録済みとして引き継ぐ（移行直後、RAG の relay が
-- 登録し直すより先に GC が未 ack の行を消さないため）。
insert into outbox_ack_consumer (name)
select 'rag'
where exists (select 1 from storage_event_outbox where processed_at is not null)
   or exists (select 1 from job_queue where queue = 'rag_ingest')
   or exists (select 1 from job_queue_dead where queue = 'rag_ingest')
   or exists (select 1 from rag_chunk)
   or exists (select 1 from rag_ingest_job);
