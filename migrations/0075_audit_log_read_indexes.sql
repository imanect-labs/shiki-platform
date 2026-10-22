-- 監査ログの閲覧・書き出し（#634）のための索引。
--
-- 閲覧は常に「自テナント・自組織」に閉じ、新しい順（created_at DESC, id DESC）に読む。
-- 既存の audit_log_object_idx / audit_log_actor_idx は org で始まり tenant_id を含まない
-- （共用プールで同じ org 名を複数テナントが持つと、他テナントの行を歩く）ので、tenant_id で
-- 始まる形に置き換える。引用チャンクからの逆引き（rag.search の metadata.cited_chunk_ids）は
-- GIN 索引で引く。
--
-- 注: CONCURRENTLY にしない（0047・0074 と同じ理由: 並列に流れる `sqlx::migrate!` の advisory
-- lock とデッドロックする）。構築中は audit_log への書き込みが待たされるので、大きな既存環境
-- では作成時間に注意する。

create index if not exists audit_log_tenant_time_idx
    on audit_log (tenant_id, org, created_at desc, id desc);

create index if not exists audit_log_tenant_actor_idx
    on audit_log (tenant_id, org, actor, created_at desc, id desc);

create index if not exists audit_log_tenant_object_idx
    on audit_log (tenant_id, org, object_type, object_id, created_at desc, id desc);

create index if not exists audit_log_cited_chunks_idx
    on audit_log using gin ((metadata -> 'cited_chunk_ids'))
    where action = 'rag.search';

drop index if exists audit_log_object_idx;
drop index if exists audit_log_actor_idx;
