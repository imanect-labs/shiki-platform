-- node_closure を tenant スコープへ拡張する（SAAS.1 の一様化・#91 L-1）。
--
-- 他の全ドメインテーブルは tenant_id を持ち全クエリが tenant 述語で絞るのに、node_closure
-- だけが org のみで、循環判定・move の張り替え・復元時の祖先判定が「node.id（グローバル一意
-- UUID）」の一意性に**暗黙依存**していた。越境は起きない設計だが、防御を一枚に頼らず他層と
-- 揃えるため tenant_id を織り込み、closure クエリにも tenant 述語を付与できるようにする。
--
-- backfill: 既存エッジの tenant は descendant ノードの tenant と一致する（closure は
-- ensure_folder で tenant スコープ検証済みの親子からのみ張られるため、ancestor/descendant/
-- edge は必ず同一 tenant に閉じる）。

alter table node_closure add column tenant_id text;

update node_closure c
   set tenant_id = n.tenant_id
  from node n
 where n.id = c.descendant;

alter table node_closure alter column tenant_id set not null;

-- 配下一括取得・祖先取得を tenant 先頭の複合インデックスに載せ替える（越境行を歩かない）。
drop index if exists node_closure_desc_idx;
drop index if exists node_closure_anc_idx;
create index node_closure_desc_idx on node_closure (tenant_id, descendant);
create index node_closure_anc_idx on node_closure (tenant_id, ancestor);
