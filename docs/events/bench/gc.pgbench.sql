-- outbox GC 1 バッチ（storage::event::gc_delivered・1 バッチ = 1 txn・fast-forward と advisory lock で排他）。
BEGIN;
SELECT pg_advisory_xact_lock(6001127091272304451);
DELETE FROM storage_event_outbox
WHERE id IN (
    SELECT o.id FROM storage_event_outbox o
    WHERE o.processed_at IS NOT NULL
      AND (SELECT count(*) FROM outbox_delivery d
           WHERE d.event_id = o.id AND d.consumer = ANY(ARRAY['miniapp-functions', 'workflow'])) >= 2
    ORDER BY o.id LIMIT 1000
    FOR UPDATE SKIP LOCKED
);
COMMIT;
