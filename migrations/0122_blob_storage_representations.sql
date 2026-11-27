-- 論理 blob の識別・refcount はそのまま、物理表現とダウンロード用 raw の寿命を分離する。
SET LOCAL lock_timeout = '5s';
ALTER TABLE blob
    ADD COLUMN storage_manifest jsonb,
    ADD COLUMN storage_bytes bigint CHECK (storage_bytes >= 0),
    ADD COLUMN storage_reads integer CHECK (storage_reads >= 0),
    ADD COLUMN storage_checked_at timestamptz,
    ADD COLUMN raw_available boolean NOT NULL DEFAULT true,
    ADD COLUMN raw_expires_at timestamptz NOT NULL DEFAULT (now() + interval '7 days'),
    ADD COLUMN raw_audit_at timestamptz NOT NULL DEFAULT now();

-- 導入前の S3 署名 URL の最大寿命（7日）を既存行だけ保護する。
-- 新しい raw の lease は reader/最適化処理が実際の TTL で設定する。
ALTER TABLE blob ALTER COLUMN raw_expires_at SET DEFAULT now();

CREATE INDEX blob_storage_backfill_idx ON blob (size_bytes, created_at, tenant_id, org, sha256)
    WHERE storage_checked_at IS NULL;
CREATE INDEX blob_storage_raw_gc_idx ON blob (tenant_id, org, raw_expires_at)
    WHERE storage_manifest IS NOT NULL AND raw_available;
CREATE INDEX blob_storage_raw_audit_idx ON blob (tenant_id, org, raw_audit_at)
    WHERE storage_manifest IS NOT NULL AND NOT raw_available;
CREATE INDEX node_version_blob_lookup_idx
    ON node_version (tenant_id, org, blob_sha256, node_id, version);
CREATE INDEX node_current_blob_idx ON node (tenant_id, org, blob_sha256)
    WHERE blob_sha256 IS NOT NULL;

-- このキューのpayloadはorg/node_id/after_versionのみ。他キューの大きいJSONは索引に入れない。
-- RLSでは式indexを使えない。nullable列を同期し、STORED生成列による全表rewriteを避ける。
ALTER TABLE job_queue ADD COLUMN storage_coalesce_key text;
CREATE FUNCTION blob_storage_job_key() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.storage_coalesce_key := CASE WHEN NEW.queue = 'storage_version_optimize'
        THEN md5((NEW.payload - 'after_version')::text) END;
    RETURN NEW;
END $$;
CREATE TRIGGER blob_storage_job_key_trigger BEFORE INSERT OR UPDATE OF queue, payload ON job_queue
    FOR EACH ROW EXECUTE FUNCTION blob_storage_job_key();
-- 導入前にこの専用キューはない。他のキューの既存行には触れない。
UPDATE job_queue SET payload = payload WHERE queue = 'storage_version_optimize';
CREATE INDEX job_queue_version_storage_pending_idx ON job_queue (tenant_id, storage_coalesce_key)
    WHERE queue = 'storage_version_optimize' AND attempts = 0;

-- 登録を先に commit してからバイトを書く。公開前に落ちた実体も台帳から回収できる。
CREATE TABLE blob_storage_object (
    tenant_id text NOT NULL,
    org text NOT NULL,
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    protected_until timestamptz NOT NULL,
    collected_at timestamptz,
    refcount bigint NOT NULL DEFAULT 0 CHECK (refcount >= 0),
    PRIMARY KEY (tenant_id, org, sha256)
);
CREATE INDEX blob_storage_object_gc_idx ON blob_storage_object (tenant_id, org, protected_until) WHERE refcount = 0;

-- GCは組織ごとに期限を進め、保護中の多数のblobを毎分再走査しない。
CREATE TABLE blob_storage_scope (
    tenant_id text NOT NULL,
    org text NOT NULL,
    next_check_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org)
);
CREATE INDEX blob_storage_scope_due_idx ON blob_storage_scope (next_check_at);

-- refcount = node_version 行数とは別の、物理実体への参照。基準版も断片も同じ FK で保護する。
CREATE TABLE blob_storage_ref (
    tenant_id text NOT NULL,
    org text NOT NULL,
    blob_sha256 text NOT NULL,
    object_sha256 text NOT NULL,
    PRIMARY KEY (tenant_id, org, blob_sha256, object_sha256),
    FOREIGN KEY (tenant_id, org, blob_sha256)
        REFERENCES blob (tenant_id, org, sha256) ON DELETE CASCADE DEFERRABLE,
    FOREIGN KEY (tenant_id, org, object_sha256)
        REFERENCES blob_storage_object (tenant_id, org, sha256) DEFERRABLE
);
CREATE INDEX blob_storage_ref_object_idx
    ON blob_storage_ref (tenant_id, org, object_sha256);

-- GC は参照付きの全実体を anti-join 走査しない。物理参照の増減と同じ Tx で数える。
CREATE FUNCTION blob_storage_ref_count() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE blob_storage_object SET refcount = refcount + 1
         WHERE tenant_id = NEW.tenant_id AND org = NEW.org AND sha256 = NEW.object_sha256;
        RETURN NEW;
    ELSE
        UPDATE blob_storage_object SET refcount = refcount - 1,
               protected_until = GREATEST(protected_until, clock_timestamp() + interval '1 day')
         WHERE tenant_id = OLD.tenant_id AND org = OLD.org AND sha256 = OLD.object_sha256;
        RETURN OLD;
    END IF;
END $$;
CREATE TRIGGER blob_storage_ref_count_trigger AFTER INSERT OR DELETE ON blob_storage_ref
    FOR EACH ROW EXECUTE FUNCTION blob_storage_ref_count();

-- バックアップ開始前に取得し、終了/ハートビートまで raw と物理実体の回収を止める。
CREATE TABLE blob_storage_hold (
    tenant_id text NOT NULL,
    org text NOT NULL,
    id uuid NOT NULL DEFAULT gen_random_uuid(),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (tenant_id, org, id)
);

DO $$
DECLARE t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['blob_storage_object', 'blob_storage_ref', 'blob_storage_hold', 'blob_storage_scope'] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY shiki_tenant_isolation ON %I '
            'USING ((SELECT shiki_rls_tenant()) IS NULL OR tenant_id = (SELECT shiki_rls_tenant())) '
            'WITH CHECK ((SELECT shiki_rls_tenant()) IS NULL OR tenant_id = (SELECT shiki_rls_tenant()))', t);
    END LOOP;
END $$;
