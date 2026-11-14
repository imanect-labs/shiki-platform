-- アーティファクトの共有された一覧を、DB の候補から並びの順に引けるようにする（BUG-149 の続き）。
--
-- 共有された一覧（`ArtifactStore::list_shared_with_me`）は、OpenFGA の ListObjects の応答が上限（1,000 件）に届いたら、
-- 組織の生きているアーティファクトのうち自分が持ち主でないものを、更新の新しい順（`updated_at DESC, id DESC`）に
-- 少しずつ引いて OpenFGA で絞る（crates/artifact/src/store.rs の scan_shared_with_me）。これまでの索引
-- （artifact_owner_idx）は持ち主が先頭なので、引くたびに組織の全行を読んで並べ直していた。続きの位置は
-- `(updated_at, id)` のキーセットなので、その並びの索引を足す（data_table_list_idx と同じ形）。

create index if not exists artifact_list_idx
    on artifact (tenant_id, org, updated_at, id)
    where deleted_at is null;
