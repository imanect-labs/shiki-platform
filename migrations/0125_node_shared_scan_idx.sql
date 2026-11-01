-- ドライブの「共有されたもの」を、DB の候補から並びの順に引けるようにする（BUG-162 の続き）。
--
-- 共有された一覧（`StorageService::list_shared_with_me`）は、OpenFGA の ListObjects の応答が上限（1,000 件）に届いたら、
-- 組織の生きているノードのうち自分が作っていないものを、更新の新しい順（`updated_at DESC, id DESC`）に 200 件ずつ
-- 引いて OpenFGA で絞る（crates/storage/src/service/shared_list.rs の scan_shared_with_me・1 回の頼みで最大 5 回）。
-- これまでの索引（node_tenant_org_idx・node_parent_idx など）は更新の時刻を含まないので、引くたびに組織の生きている
-- ノードをすべて読んで並べ直していた。続きの位置は `(updated_at, id)` のキーセットなので、その並びの索引を足す
-- （artifact_list_idx・data_table_list_idx と同じ形）。大きな node の表では、索引の作成に時間がかかる。

create index node_shared_scan_idx
    on node (tenant_id, org, updated_at, id)
    where deleted_at is null;
