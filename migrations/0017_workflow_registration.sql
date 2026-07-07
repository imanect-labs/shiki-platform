-- Phase 10 Task 10.4a: 実行主体・委譲モデル（engine.md §6・§10・FR-12 最重要）。
--
-- workflow_registration = 有効化状態の正。workflow_trigger = IR から実体化したトリガ。
-- workflow_delegation = 委譲台帳（有効化者が自分の権限範囲から付与した (scope, object) を記録）。
--
-- 委譲モデル（human 承認・engine.md §6.6）: 委譲 = 対象オブジェクトへの通常 relation タプル
-- （subject = workflow プリンシパル）＋ workflow_delegation 行でのリンク管理。新機構は使わない。
-- 委譲は (scope, 対象オブジェクト) ごとに 1 行（object_ref を PK に含む）。
--
-- 規約: 全行 tenant_id not null・PK は tenant 先頭の複合キー（#91）。

create table workflow_registration (
    tenant_id       text        not null,
    workflow_id     uuid        not null,
    org             text        not null,
    -- enabled / disabled / suspended_reconsent（失権検知で再同意要求・engine.md §6.3）。
    status          text        not null default 'disabled'
                    check (status in ('enabled', 'disabled', 'suspended_reconsent')),
    -- 有効化した IR バージョン。
    enabled_version bigint,
    -- 有効化時に同意したスコープ集合（declared_scopes ⊆ これ を実行時検証・engine.md §6.2）。
    consented_scopes text[]     not null default '{}',
    -- 有効化者（委譲の起点・失権検知の対象）。
    enabled_by      text,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now(),
    primary key (tenant_id, workflow_id)
);

create table workflow_trigger (
    tenant_id       text        not null,
    trigger_id      text        not null,
    workflow_id     uuid        not null,
    -- 実体化元の IR バージョン。
    version         bigint      not null,
    kind            text        not null check (kind in ('schedule', 'event', 'interactive')),
    -- schedule=cron source は無し / event=イベント source（storage.write 等）。
    source          text,
    -- トリガの spec（cron/tz/scope/filter 等）。
    spec            jsonb       not null default '{}'::jsonb,
    -- スケジュール評価済み watermark（cron の misfire/catchup・engine.md §5.2）。
    last_planned_at timestamptz,
    enabled         boolean     not null default true,
    created_at      timestamptz not null default now(),
    primary key (tenant_id, trigger_id),
    foreign key (tenant_id, workflow_id)
        references workflow_registration (tenant_id, workflow_id) on delete cascade
);

-- イベント/スケジュールのマッチャ走査（有効なトリガのみ・engine.md §5.4）。
create index workflow_trigger_match_idx
    on workflow_trigger (tenant_id, kind, source)
    where enabled;

create table workflow_delegation (
    tenant_id   text        not null,
    workflow_id uuid        not null,
    -- 委譲者（この人の失権でワークフローを fail-closed 停止・engine.md §6.3）。
    delegator   text        not null,
    -- 委譲した declared_scope の 1 要素。
    scope       text        not null,
    -- 委譲対象の FGA オブジェクト（例: `folder:<tenant>|<id>`）。
    object_ref  text        not null,
    -- 書き込んだ FGA タプルの relation（撤去に使う・例: viewer）。
    relation    text        not null,
    granted_at  timestamptz not null default now(),
    revoked_at  timestamptz,
    -- (scope, object) ごとに 1 行。
    primary key (tenant_id, workflow_id, delegator, scope, object_ref),
    foreign key (tenant_id, workflow_id)
        references workflow_registration (tenant_id, workflow_id) on delete cascade
);

-- 委譲者ごとの棚卸し走査（失権検知で全委譲を再評価・engine.md §6.3）。
create index workflow_delegation_delegator_idx
    on workflow_delegation (tenant_id, delegator)
    where revoked_at is null;
