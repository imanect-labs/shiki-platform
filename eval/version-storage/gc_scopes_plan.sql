\set ON_ERROR_STOP on
-- 専用テストDBで実行。合成台帳はROLLBACKし、ストアは操作しない。
BEGIN;
SET LOCAL statement_timeout = '60s';
SET LOCAL lock_timeout = '2s';
INSERT INTO blob_storage_scope (tenant_id,org,next_check_at)
SELECT 'gc-plan-' || i, 'org', '1900-01-01'::timestamptz FROM generate_series(1,10000) i;
INSERT INTO blob_storage_hold (tenant_id,org,expires_at)
SELECT 'gc-plan-' || i, 'org', now() + interval '1 day' FROM generate_series(1,10000) i;
ANALYZE blob_storage_scope;
ANALYZE blob_storage_hold;
SET LOCAL ROLE shiki_maintenance;
SET LOCAL application_name = 'shiki-cross-tenant:blob_storage_gc_scan';
EXPLAIN (ANALYZE, BUFFERS)
WITH due AS MATERIALIZED (SELECT tenant_id, org FROM blob_storage_scope               WHERE next_check_at < now() ORDER BY next_check_at LIMIT 16 FOR UPDATE SKIP LOCKED),               postponed AS (UPDATE blob_storage_scope s SET next_check_at = GREATEST(                 clock_timestamp() + interval '1 minute',                 COALESCE((SELECT max(expires_at) FROM blob_storage_hold h WHERE h.tenant_id = s.tenant_id AND h.org = s.org), now()),                 CASE WHEN EXISTS (SELECT 1 FROM tenant t WHERE t.tenant_id = s.tenant_id AND t.status IN ('deleting', 'deleted'))                      THEN clock_timestamp() + interval '1 hour' ELSE now() END)               FROM due WHERE s.tenant_id = due.tenant_id AND s.org = due.org RETURNING s.tenant_id, s.org)               SELECT tenant_id, org FROM postponed p               WHERE NOT EXISTS (SELECT 1 FROM blob_storage_hold h WHERE h.tenant_id = p.tenant_id AND h.org = p.org AND h.expires_at > now())               AND NOT EXISTS (SELECT 1 FROM tenant t WHERE t.tenant_id = p.tenant_id AND t.status IN ('deleting', 'deleted'));
ROLLBACK;
