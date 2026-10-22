-- RAG relay（rag/src/pipeline/relay.rs::relay_once）と同一の 1 txn: claim 64 → job_queue へコピー → processed_at。
-- 本番は enqueue を 1 件ずつ投げる（64 往復）が、ここでは 1 文にまとめている＝DB 側の上限を見る形。
-- payload.emitted_at に outbox の created_at を写し、enqueued_at との差を relay レイテンシとして読む。
BEGIN;
WITH c AS (
    SELECT o.id, o.org, o.tenant_id, o.node_id, o.version, o.op, o.actor, o.trace_id, o.created_at,
           coalesce(n.system, false) AS system
    FROM storage_event_outbox o LEFT JOIN node n ON n.id = o.node_id
    WHERE o.processed_at IS NULL
    ORDER BY o.id FOR UPDATE OF o SKIP LOCKED LIMIT 64
), j AS (
    INSERT INTO job_queue (queue, tenant_id, payload, max_attempts, trace_id)
    SELECT 'rag_ingest', tenant_id,
           jsonb_build_object('tenant_id', tenant_id, 'org', org, 'node_id', node_id, 'version', version,
                              'op', op, 'actor', actor, 'emitted_at', created_at),
           5, trace_id
    FROM c WHERE NOT system
)
UPDATE storage_event_outbox SET processed_at = now() WHERE id IN (SELECT id FROM c);
COMMIT;
