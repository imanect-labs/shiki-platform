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

-- 既存の自律ワークスペースとその配下を system 化する。
--
-- 判定は「**その thread が自動生成した**フォルダ」だけに絞る。条件は 2 つの AND:
--   ① `thread.workspace_folder_id` がそのフォルダを指している
--   ② 名前が `agent-workspace-<その thread の id>` と完全一致する（生成規則そのもの）
--
-- `name like 'agent-workspace-%'` だけで拾うと、利用者が自分で作った同名フォルダを勝手に
-- 隠してしまう（この命名は予約されていない・レビュー指摘 Codex P1）。逆に
-- `workspace_folder_id` だけで拾うと、「既存フォルダをそのままワークスペースにする」で
-- 利用者が**選んだ可視フォルダ**まで隠してしまう。名前に thread id が入っている＝
-- ensure_workspace が作ったもの、という事実で両方を排除する。
with auto_workspace as (
    select n.id
    from thread t
    join node n on n.id = t.workspace_folder_id
    where n.kind = 'folder'
      and n.name = 'agent-workspace-' || t.id::text
)
update node set system = true
where id in (select id from auto_workspace)
   or id in (
        select c.descendant from node_closure c
        where c.ancestor in (select id from auto_workspace)
   );

-- 一覧クエリは「隠さない側」（system = false）が既定の絞り込みなので、そちらに部分索引を張る。
-- 既存の node_parent_idx（parent 単位）と併用され、system 領域が増えても一覧が劣化しない。
create index node_visible_parent_idx
    on node (org, tenant_id, parent_id)
    where deleted_at is null and system = false;
