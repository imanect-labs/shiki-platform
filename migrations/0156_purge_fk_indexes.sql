-- 完全削除（GAP-108b）で node と blob の行を消すときの、外部キーの確かめに使う索引。
--
-- node の行を消すと、PG は node_tenant_id_parent_id_fkey のために「その node を親に持つ行」を探し、blob の行を消すと
-- node_version_blob_fk のために「その blob を指す版」を探す。今ある node_parent_idx は deleted_at is null の部分索引で、
-- 条件の無い RI の問い合わせには使えない。node_version には (tenant_id, org, blob_sha256) の索引が無い。このため、
-- ファイル 1 件の完全削除でも node と node_version を全件読み、大きなフォルダの完全削除や保持期間の掃除
-- （GAP-108c）は時間の上限に届きうる。
--
-- 0135・0155 と同じく CONCURRENTLY は使わない（作る間は node・node_version への書き込みが待たされる）。大きな表に
-- 当てるときはメンテナンスの時間に当てるか、事前に同じ名前の索引を CONCURRENTLY で作っておく（if not exists で飛ばす）。
create index if not exists node_tenant_parent_fk_idx on node (tenant_id, parent_id);
create index if not exists node_version_blob_fk_idx on node_version (tenant_id, org, blob_sha256);
