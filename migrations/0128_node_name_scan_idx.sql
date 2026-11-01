-- ドライブの名前の検索を、DB の候補から名前の順に引けるようにする（BUG-187）。
--
-- 名前の検索（`StorageService::search_nodes_by_name`）は、OpenFGA の ListObjects（file・folder の viewer）が上限
-- （1,000 件）に届いたら、候補を語で選ばずに、組織の生きているノードを並びの順（既定は `name, id`）に 200 件ずつ
-- 引いて OpenFGA で絞る（crates/storage/src/service/node_scan.rs の named_scan_sql・1 回の頼みで問うのは 1,000 問まで）。
-- 語で候補を選ばないのは、見えないノードの名前に語が含まれるかを応答の時間から推し量らせないため。これまでの索引は
-- 名前の順を含まないので、引くたびに組織の生きているノードをすべて読んで並べ直すことになる。続きの位置は
-- `(name, id)` のキーセットなので、その並びの索引を足す（更新の順は 0125 の node_shared_scan_idx が使える）。
-- 大きな node の表では、索引の作成に時間がかかる。

create index node_name_scan_idx
    on node (tenant_id, org, name, id)
    where deleted_at is null;
