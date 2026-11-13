-- ドライブの名前の検索を、DB の候補から名前の順に引けるようにする（BUG-187）。
--
-- 名前の検索（`StorageService::search_nodes_by_name`）は、OpenFGA の ListObjects（file・folder の viewer）が上限
-- （1,000 件）に届いたら、候補を語で選ばずに、組織の生きているノードを並びの順に 200 件ずつ引いて OpenFGA で絞る
-- （crates/storage/src/service/node_scan.rs の named_scan_sql・1 回の頼みで問うのは 1,000 問まで）。
-- 語で候補を選ばないのは、見えないノードの名前に語が含まれるかを応答の時間から推し量らせないため。これまでの索引は
-- 名前の順を含まないので、引くたびに組織の生きているノードをすべて読んで並べ直すことになる。
--
-- 並びは「フォルダー優先 → 名前の自然順（node_name_natural・0110）→ id」（#1040）。フォルダーを昇順でも降順でも
-- 先頭に置きつつキーセットを 1 本の行の比較に保つため、種別の順位の式が向きで変わる（昇順は `kind <> 'folder'` の
-- 昇順、降順は `kind = 'folder'` の降順・crates/storage/src/service.rs の child_order_sql）。1 本の索引では両方の向きを
-- 満たせないので、向きごとに 1 本ずつ足す（降順は 2 本目を逆にたどる）。更新の順は 0139 の node_shared_scan_idx が使える。
--
-- 名前の列は ICU の照合（node_name_natural）で並べる。ICU の版が上がって並びが変わると、この 2 本の索引の並びが
-- 比較とずれ、検索の続きで欠けることがある（認可は OpenFGA の判定で別に効くので、見えないものは出ない）。
-- PostgreSQL が照合の版の違いを警告したら、この 2 本を REINDEX してから `ALTER COLLATION node_name_natural
-- REFRESH VERSION` を流す（docs/guides/db-maintenance.md）。大きな node の表では、索引の作成に時間がかかる。

create index if not exists node_name_scan_idx
    on node (tenant_id, org, (kind <> 'folder'), (name collate node_name_natural), id)
    where deleted_at is null;

create index if not exists node_name_scan_desc_idx
    on node (tenant_id, org, (kind = 'folder'), (name collate node_name_natural), id)
    where deleted_at is null;
