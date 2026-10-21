-- node_share_link_grant.expires_at は書き込みのみで読み取りクエリが存在しない dead column（#367/B-6）。
-- 失効は常に node_share_link 側の expires_at で駆動され（reconcile / revoke_expired / next の
-- 全クエリが link 表の expires_at を参照）、per-user grant 独自の期限セマンティクスは未実装。
-- YAGNI に従い列と部分インデックスを削除する（将来 per-user 期限が要るなら別 migration で再導入）。
drop index if exists node_share_link_grant_sweep_idx;
alter table node_share_link_grant drop column if exists expires_at;
