-- 書込側: storage::event::emit_on と同一の INSERT（ドメイン書込本体〔node/node_version/監査〕は含めない）。
-- テナントは bench_node（T テナント均等）から一様に選ぶ。
\set k random(0, :nodes - 1)
INSERT INTO storage_event_outbox (org, tenant_id, node_id, version, op, actor, trace_id, payload)
SELECT org, tenant_id, id, 1, 'update', 'user-bench', md5(random()::text),
       jsonb_build_object('kind', 'file', 'blob_sha256', md5(random()::text) || md5(random()::text), 'size', 12345)
FROM bench_node WHERE k = :k;
