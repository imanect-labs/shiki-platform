-- blob（content-addressed ストア）を tenant_id スコープへ拡張する（SAAS.1・#84）。
--
-- 従来は (org, sha256) を主キー・ObjectStore キーを '{org}/{sha256}' としていたが、SaaS では
-- 同一 org slug を複数テナントが共有し得るため、org だけでは越境の dedup 共有・hash 存在オラクル・
-- refcount 破壊を防げない。tenant_id を最上位に織り込み、キーを '{tenant_id}/{org}/{sha256}' にする
-- （crates/storage/src/content_address.rs と対）。node / node_version の blob FK も tenant_id 込みへ。
--
-- 注: これは破壊的変更（GA 前・dev 専用）。既存 blob は tenant_id='default' へ backfill するが、
-- 非空 dev データでは node.tenant_id と一致しない可能性があるため、その場合は DB を作り直す前提
-- （本番デプロイ無し・オブジェクトは再アップロード前提）。CI/テストは空 DB で適用される。

-- 1. blob PK に依存する FK を一旦外す（node / node_version が (org, blob_sha256) で参照）。
alter table node drop constraint node_blob_fk;
alter table node_version drop constraint node_version_blob_fk;

-- 2. blob に tenant_id を足し、主キーを (tenant_id, org, sha256) へ。
alter table blob add column tenant_id text;
update blob set tenant_id = 'default' where tenant_id is null;
alter table blob alter column tenant_id set not null;
alter table blob drop constraint blob_pkey;
alter table blob add primary key (tenant_id, org, sha256);

-- 3. FK を tenant_id 込みで張り直す（folder は blob_sha256 が null＝MATCH SIMPLE で不問）。
alter table node add constraint node_blob_fk
    foreign key (tenant_id, org, blob_sha256) references blob (tenant_id, org, sha256);
alter table node_version add constraint node_version_blob_fk
    foreign key (tenant_id, org, blob_sha256) references blob (tenant_id, org, sha256);
