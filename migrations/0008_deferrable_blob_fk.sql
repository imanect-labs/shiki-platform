-- blob への FK を DEFERRABLE にする（#89 retenant / cell→pool 移行の前提）。
--
-- テナントリネームは node / node_version / blob の tenant_id を同一 txn 内で順に UPDATE するが、
-- 即時（NOT DEFERRABLE）FK だと途中状態（node が旧 tenant の blob 行を参照）で違反になる。
-- DEFERRABLE INITIALLY IMMEDIATE なら通常運用は従来どおり即時検査のまま、移行 txn だけ
-- `SET CONSTRAINTS ... DEFERRED` で commit 時検査へ切り替えられる。
alter table node
    drop constraint node_blob_fk,
    add constraint node_blob_fk
        foreign key (tenant_id, org, blob_sha256) references blob (tenant_id, org, sha256)
        deferrable initially immediate;
alter table node_version
    drop constraint node_version_blob_fk,
    add constraint node_version_blob_fk
        foreign key (tenant_id, org, blob_sha256) references blob (tenant_id, org, sha256)
        deferrable initially immediate;
