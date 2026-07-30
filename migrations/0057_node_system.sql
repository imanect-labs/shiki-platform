-- issue #392: 自律エージェントのワークスペースを「システム領域」にする。
--
-- 自律エージェントの作業メモ（brief / outline / notes 等）は**使い捨て**であり、ユーザーの
-- ドライブに並べる/社内検索に載せる対象ではない。一方で durable な置き場所は必要（ステップを
-- 跨いだ再開・剪定されたコンテキストの外部メモリ）なので、ストレージ層（認可・監査・版管理の
-- 単一チョークポイント）は再用し、**表示と索引だけを外す**属性を node に持たせる。
--
-- 効き方（正本は crates/storage/src/service/read.rs / trash.rs / crates/rag/src/pipeline/relay.rs）:
--   - ドライブ一覧・名前検索・ゴミ箱一覧に出さない（list_children(include_system=false) 等）
--   - 書込イベントを rag_ingest へ relay しない（＝ベクタ/全文索引に載らない）
--   - エージェント自身（chat の WorkspaceStore 経由）からは通常どおり list/read/write できる
-- 認可は一切変えない（system でも ReBAC は同じ経路で効く。隠すことを権限の代わりにしない）。

alter table node add column system boolean not null default false;

-- 既存の自律ワークスペース（thread ごとの agent-workspace-<uuid>）とその配下を system 化する。
-- node_closure で子孫を引く（ワークスペースはフラットだが、将来のサブフォルダにも効かせる）。
update node set system = true
where kind = 'folder' and deleted_at is null and name like 'agent-workspace-%';

update node set system = true
where id in (
    select c.descendant from node_closure c
    join node f on f.id = c.ancestor
    where f.kind = 'folder' and f.name like 'agent-workspace-%'
);

-- 一覧クエリは「隠さない側」（system = false）が既定の絞り込みなので、そちらに部分索引を張る。
-- 既存の node_parent_idx（parent 単位）と併用され、system 領域が増えても一覧が劣化しない。
create index node_visible_parent_idx
    on node (org, tenant_id, parent_id)
    where deleted_at is null and system = false;
