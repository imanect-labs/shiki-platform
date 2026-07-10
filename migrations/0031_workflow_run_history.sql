-- Phase 10 #198: 実行履歴 read API（run 一覧の keyset ページング）＋エディタレイアウト永続化。
--
-- workflow_editor_layout = dnd エディタのノード座標（IR 外・非バージョン・化粧品扱い）。
-- IR に座標を入れない（deny-unknown・ir_version 据え置き・AI 生成 IR は dagre 自動配置）。

create index if not exists workflow_run_list_idx
    on workflow_run (tenant_id, workflow_id, created_at desc, run_id desc);

create table workflow_editor_layout (
    tenant_id   text        not null,
    workflow_id uuid        not null,
    -- { "positions": { "<node_id>": { "x": .., "y": .. } }, "triggers": { ... } }
    layout      jsonb       not null default '{}'::jsonb,
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, workflow_id)
);
