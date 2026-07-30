-- 共有リンクの per-user 個別取消を **durable** にする deny 台帳（#375）。
--
-- 経緯: #369 C-3 は per-user 取消を grant 行の DELETE で実装したが、リンクが active なままだと
-- 対象ユーザーが同じ URL＋パスワードで再 redeem してアクセスを復元でき、「取り消し」が持続
-- しなかった（PR #373 で撤去）。行を消さずソフト失効させ、redeem 側が「この (link,user) は
-- 取消済み」を見て拒否することで durable にする。
--
-- 中核の不変条件:
--   revoked_at IS NULL  ⇔  その (link,user) の付与が live
-- これを立てるのは以下の 2 経路（どちらも DELETE をやめてこの列を立てる）:
--   - owner の per-user 取消（revoke_share_link_grant）
--   - リンク自体の失効／期限失効（reconcile_user_grants_for_link）
-- 以後、台帳を読むすべてのクエリは revoked_at IS NULL で絞る。
--
-- リンクごと失効した場合も行を残すのは、**期限切れリンクは active に戻り得る**ため
-- （extend_share_link は expires_at を見ないので、未 sweep の期限切れリンクを未来へ延長すると
-- 復活する）。行を消すと deny 台帳がその経路で蒸発し durability が壊れる。
--
-- 誰が取り消したかは audit_log の node.share_link.grant.revoke（actor 付き）が正本なので、
-- revoked_by / revoked_reason 列は置かない（0056 で dead 列を落としたばかりの轍を踏まない）。
-- 既存行は NULL＝live で意味が正しく、バックフィル不要。
alter table node_share_link_grant
    add column revoked_at timestamptz;

-- per-user reconcile の参照カウント (node,user,role) は **live 行だけ**を数える。
-- revoked 行は deny 台帳としてのみ残り集計対象から常に外れるので、索引にも載せない。
drop index if exists node_share_link_grant_node_user_idx;
create index node_share_link_grant_node_user_idx
    on node_share_link_grant (node_id, user_id, role)
    where revoked_at is null;
