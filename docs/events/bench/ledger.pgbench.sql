-- 台帳コンシューマ（storage::event::claim_undelivered → mark_delivered・同一 txn）。
-- @CONSUMER@ は outbox-pool-bench.sh が埋める。書込→配送のレイテンシを bench_latency に記録する。
BEGIN;
WITH c AS (
    SELECT o.id, o.tenant_id, o.created_at, coalesce(n.system, false) AS system
    FROM storage_event_outbox o LEFT JOIN node n ON n.id = o.node_id
    WHERE NOT EXISTS (SELECT 1 FROM outbox_delivery d WHERE d.consumer = '@CONSUMER@' AND d.event_id = o.id)
    ORDER BY o.id FOR UPDATE OF o SKIP LOCKED LIMIT 64
), l AS (
    INSERT INTO bench_latency (consumer, lat_ms)
    SELECT '@CONSUMER@', extract(epoch FROM clock_timestamp() - created_at) * 1000 FROM c
)
INSERT INTO outbox_delivery (consumer, event_id, tenant_id)
SELECT '@CONSUMER@', id, tenant_id FROM c
ON CONFLICT (consumer, event_id) DO NOTHING;
COMMIT;
