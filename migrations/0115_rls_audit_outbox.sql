-- audit_log・storage_event_outbox を RLS の対象に入れる（#1074・RLS 段階導入）。
--
-- 0114 はこの 2 表を対象外にした。RLS の付いた表では、leakproof でない演算子・関数（jsonb の `->>`・
-- `?`・`@>`、配列の `@>` 等）を含む条件をインデックス条件に使えない（ポリシーより先に評価できない）。
-- この 2 表は jsonb の式インデックスと GIN で引いていたため、RLS を付けると文脈が未設定でも
-- テナント内の全行走査に落ちる（20 万行で 0.4 ms → 104 ms）。
--
-- そこで、引く値を普通の列（text）に持たせて btree で引く。text の `=` は leakproof なので、
-- RLS の下でもインデックス条件に使える。
--
-- - audit_log: `metadata ->> 'thread_id'` / `'message_id'` を `meta_thread_id` / `meta_message_id` に写す。
-- - audit_log の引用チャンク（`metadata -> 'cited_chunk_ids' ? <id>`）: 配列の演算子も leakproof で
--   ないので、別表 `audit_cited_chunk`（1 行 = 1 引用）に切り出す。
-- - storage_event_outbox: `payload ->> 'event_type'` / `'table_id'` を `payload_event_type` /
--   `payload_table_id` に写す。
--
-- 列は生成列（GENERATED ... STORED）にしない。追加の時点で表を丸ごと書き直し（ACCESS EXCLUSIVE）、
-- 大きな audit_log ではその間すべての監査の書込が止まるため。普通の列の追加は一瞬で終わり、値は
-- 挿入時のトリガで写し、既存の行はこの migration で埋める。行数の多い環境では、デプロイ前に手で
-- 埋めてインデックスを CONCURRENTLY で作っておく（docs/guides/db-maintenance.md §6）。
--
-- 列名に接頭辞（meta_ / payload_）を付けるのは、JOIN 先の表の `thread_id` / `table_id` 等と
-- 名前がぶつからないようにするため。

SET LOCAL lock_timeout = '5s';

-- ---------------------------------------------------------------------------------------------
-- 1. audit_log の thread_id / message_id
-- ---------------------------------------------------------------------------------------------
ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS meta_thread_id text;
ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS meta_message_id text;

CREATE OR REPLACE FUNCTION shiki_audit_log_extract()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.meta_thread_id := NEW.metadata ->> 'thread_id';
    NEW.meta_message_id := NEW.metadata ->> 'message_id';
    RETURN NEW;
END
$$;

DROP TRIGGER IF EXISTS audit_log_extract ON audit_log;
CREATE TRIGGER audit_log_extract
    BEFORE INSERT OR UPDATE OF metadata ON audit_log
    FOR EACH ROW EXECUTE FUNCTION shiki_audit_log_extract();

UPDATE audit_log
   SET meta_thread_id = metadata ->> 'thread_id',
       meta_message_id = metadata ->> 'message_id'
 WHERE (metadata ? 'thread_id' AND meta_thread_id IS NULL)
    OR (metadata ? 'message_id' AND meta_message_id IS NULL);

CREATE INDEX IF NOT EXISTS audit_log_meta_thread_idx
    ON audit_log (tenant_id, org, meta_thread_id, created_at DESC, id DESC)
    WHERE meta_thread_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS audit_log_meta_message_idx
    ON audit_log (tenant_id, org, meta_message_id, created_at DESC, id DESC)
    WHERE meta_message_id IS NOT NULL;
DROP INDEX IF EXISTS audit_log_tenant_thread_idx;
DROP INDEX IF EXISTS audit_log_tenant_message_idx;

-- ---------------------------------------------------------------------------------------------
-- 2. 引用チャンク（rag.search の `metadata.cited_chunk_ids`）
-- ---------------------------------------------------------------------------------------------
-- audit_log の行に従属する索引用の表。監査の保持期間で親を消すと一緒に消える（ON DELETE CASCADE）。
CREATE TABLE IF NOT EXISTS audit_cited_chunk (
    audit_id   bigint      NOT NULL REFERENCES audit_log (id) ON DELETE CASCADE,
    chunk_id   text        NOT NULL,
    tenant_id  text        NOT NULL,
    org        text        NOT NULL,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (audit_id, chunk_id)
);
CREATE INDEX IF NOT EXISTS audit_cited_chunk_lookup_idx
    ON audit_cited_chunk (tenant_id, org, chunk_id, created_at DESC, audit_id DESC);

CREATE OR REPLACE FUNCTION shiki_audit_cited_chunk_sync()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.action = 'rag.search' AND jsonb_typeof(NEW.metadata -> 'cited_chunk_ids') = 'array' THEN
        INSERT INTO audit_cited_chunk (audit_id, chunk_id, tenant_id, org, created_at)
        SELECT NEW.id, c, NEW.tenant_id, NEW.org, NEW.created_at
          FROM jsonb_array_elements_text(NEW.metadata -> 'cited_chunk_ids') AS c
        ON CONFLICT DO NOTHING;
    END IF;
    RETURN NULL;
END
$$;

DROP TRIGGER IF EXISTS audit_log_cited_chunk ON audit_log;
CREATE TRIGGER audit_log_cited_chunk
    AFTER INSERT ON audit_log
    FOR EACH ROW EXECUTE FUNCTION shiki_audit_cited_chunk_sync();

INSERT INTO audit_cited_chunk (audit_id, chunk_id, tenant_id, org, created_at)
SELECT a.id, c, a.tenant_id, a.org, a.created_at
  FROM audit_log a, jsonb_array_elements_text(a.metadata -> 'cited_chunk_ids') AS c
 WHERE a.action = 'rag.search' AND jsonb_typeof(a.metadata -> 'cited_chunk_ids') = 'array'
ON CONFLICT DO NOTHING;

DROP INDEX IF EXISTS audit_log_cited_chunks_idx;

-- ---------------------------------------------------------------------------------------------
-- 3. storage_event_outbox の event_type / table_id
-- ---------------------------------------------------------------------------------------------
ALTER TABLE storage_event_outbox ADD COLUMN IF NOT EXISTS payload_event_type text;
ALTER TABLE storage_event_outbox ADD COLUMN IF NOT EXISTS payload_table_id text;

CREATE OR REPLACE FUNCTION shiki_outbox_extract()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.payload_event_type := NEW.payload ->> 'event_type';
    NEW.payload_table_id := NEW.payload ->> 'table_id';
    RETURN NEW;
END
$$;

DROP TRIGGER IF EXISTS storage_event_outbox_extract ON storage_event_outbox;
CREATE TRIGGER storage_event_outbox_extract
    BEFORE INSERT OR UPDATE OF payload ON storage_event_outbox
    FOR EACH ROW EXECUTE FUNCTION shiki_outbox_extract();

UPDATE storage_event_outbox
   SET payload_event_type = payload ->> 'event_type',
       payload_table_id = payload ->> 'table_id'
 WHERE (payload ? 'event_type' AND payload_event_type IS NULL)
    OR (payload ? 'table_id' AND payload_table_id IS NULL);

CREATE INDEX IF NOT EXISTS storage_event_outbox_table_change_idx
    ON storage_event_outbox (tenant_id, payload_table_id, id)
    WHERE payload_event_type = 'data.record.changed';
DROP INDEX IF EXISTS storage_event_outbox_data_change_idx;

-- ---------------------------------------------------------------------------------------------
-- 4. RLS（0114 と同じポリシー・移行モード）
-- ---------------------------------------------------------------------------------------------
DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['audit_log', 'storage_event_outbox', 'audit_cited_chunk'] LOOP
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format('DROP POLICY IF EXISTS shiki_tenant_isolation ON public.%I', t);
        EXECUTE format(
            'CREATE POLICY shiki_tenant_isolation ON public.%I '
            'USING ((SELECT shiki_rls_tenant()) IS NULL OR tenant_id = (SELECT shiki_rls_tenant())) '
            'WITH CHECK ((SELECT shiki_rls_tenant()) IS NULL OR tenant_id = (SELECT shiki_rls_tenant()))',
            t
        );
    END LOOP;
END
$$;
