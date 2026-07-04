-- 監査ハッシュチェーンの直前行探索を tenant_id + org スコープにした（SAAS.1・#84）ため、
-- その形状に合う部分インデックスへ差し替える。共用プールで同一 org slug を複数テナントが
-- 共有する場合、旧 (org, id) インデックスだと他テナントの chained 行を歩いてから自テナントの
-- prev_hash に到達するため、per-tenant/org の advisory ロック保持時間が伸びる。
create index audit_log_chain_tenant_idx
    on audit_log (tenant_id, org, id) where chained;

-- 旧 org-only インデックスは chain 探索が tenant スコープになり不要。
drop index if exists audit_log_chain_idx;
