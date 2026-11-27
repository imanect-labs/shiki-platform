-- 専用テストDBだけで実行する。合成メタデータは最後にROLLBACKし、実体は書き込まない。
-- psql -v ON_ERROR_STOP=1 -f eval/version-storage/sql_plans.sql <test database>
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '60s';
SET LOCAL lock_timeout = '2s';
INSERT INTO blob (tenant_id, org, sha256, size_bytes, object_key)
SELECT 'version-perf', 'version-perf', lpad(to_hex(i), 64, '0'), 67108865, 'unused'
FROM generate_series(1, 100000) i;
INSERT INTO node (id, tenant_id, org, kind, name, blob_sha256, size_bytes, created_by, updated_by)
VALUES ('00000000-0000-4000-8000-000000001297', 'version-perf', 'version-perf', 'file',
        'synthetic', lpad(to_hex(100000), 64, '0'), 67108865, 'fixture', 'fixture');
INSERT INTO node_version (node_id, version, tenant_id, org, blob_sha256, size_bytes, content_type, author)
SELECT '00000000-0000-4000-8000-000000001297', i, 'version-perf', 'version-perf',
       lpad(to_hex(i), 64, '0'), 67108865, 'application/octet-stream', 'fixture'
FROM generate_series(1, 100000) i;
INSERT INTO job_queue (queue, tenant_id, payload)
SELECT 'storage_version_optimize', 'version-perf', jsonb_build_object('org', 'version-perf', 'node_id', i, 'after_version', 0)
FROM generate_series(1, 10000) i;
ANALYZE blob;
ANALYZE node;
ANALYZE node_version;
ANALYZE job_queue;

-- RLSを有効なままジョブの点検索を測る。JSON判定はtext索引で絞った後の残余条件。
SET LOCAL ROLE shiki_runtime;
SELECT set_config('app.tenant_id', 'version-perf', true);
EXPLAIN (ANALYZE, BUFFERS)
SELECT id FROM job_queue WHERE queue = 'storage_version_optimize' AND tenant_id = 'version-perf'
AND storage_coalesce_key = md5((jsonb_build_object('org', 'version-perf', 'node_id', 5000, 'after_version', 0) - 'after_version')::text)
AND (payload - 'after_version') = (jsonb_build_object('org', 'version-perf', 'node_id', 5000, 'after_version', 0) - 'after_version')
AND attempts = 0 LIMIT 1 FOR UPDATE SKIP LOCKED;

EXPLAIN (ANALYZE, BUFFERS)
WITH version_window AS MATERIALIZED (
    SELECT blob_sha256, version, is_proposal FROM node_version
    WHERE tenant_id = 'version-perf' AND org = 'version-perf'
      AND node_id = '00000000-0000-4000-8000-000000001297' AND version > 99900
    ORDER BY version LIMIT 16)
SELECT v.blob_sha256, v.version, (NOT v.is_proposal AND b.storage_manifest IS NULL AND b.size_bytes <= 67108864
  AND NOT EXISTS (SELECT 1 FROM node n WHERE n.tenant_id = 'version-perf' AND n.org = 'version-perf' AND n.blob_sha256 = b.sha256)) AS eligible
FROM version_window v JOIN blob b ON b.tenant_id = 'version-perf' AND b.org = 'version-perf' AND b.sha256 = v.blob_sha256
ORDER BY v.version;
RESET ROLE;

EXPLAIN (ANALYZE, BUFFERS)
SELECT tenant_id, org, sha256 FROM blob b WHERE storage_checked_at IS NULL AND size_bytes <= 67108864
AND NOT EXISTS (SELECT 1 FROM tenant t WHERE t.tenant_id = b.tenant_id AND t.status IN ('deleting', 'deleted'))
ORDER BY size_bytes, created_at, tenant_id, org, sha256 LIMIT 64;
ROLLBACK;
