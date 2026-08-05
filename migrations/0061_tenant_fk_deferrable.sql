-- tenant_id を参照列に含む FK をすべて DEFERRABLE にする（#420 retenant / cell→pool 移行の前提）。
--
-- テナントリネーム（shiki-admin retenant --from A --to B）は tenant_id を持つ全テーブルを同一 txn 内で
-- 順に UPDATE する。即時（NOT DEFERRABLE）FK だと、参照列に tenant_id を含む関係が途中状態で必ず違反する:
--   - 親を先に更新 → 子がまだ旧 tenant を指す（ON UPDATE NO ACTION なら親側の RI 検査で違反）
--   - 子を先に更新 → 新 tenant の親行がまだ存在しない（子側の FK 検査で違反）
-- どちらの順序でも解けないため、commit 時検査への遅延が唯一の解。migration 0008 が node/node_version →
-- blob に対して同じ手当てを済ませており、本 migration は残り 9 本へ同じ扱いを広げる。
--
-- DEFERRABLE INITIALLY IMMEDIATE なので通常運用の検査タイミングは従来どおり（各文の末尾）で不変。
-- 移行 txn だけが `SET CONSTRAINTS ALL DEFERRED` で commit 時検査へ切り替える。
--
-- ON UPDATE / ON DELETE の挙動は元定義をそのまま維持する（DEFERRABLE 以外は変更しない）。

alter table artifact_version
    drop constraint artifact_version_tenant_id_artifact_id_fkey,
    add constraint artifact_version_tenant_id_artifact_id_fkey
        foreign key (tenant_id, artifact_id) references artifact (tenant_id, id)
        on update cascade on delete cascade
        deferrable initially immediate;

alter table data_index_registry
    drop constraint data_index_registry_tenant_id_table_id_fkey,
    add constraint data_index_registry_tenant_id_table_id_fkey
        foreign key (tenant_id, table_id) references data_table (tenant_id, id)
        on update cascade on delete cascade
        deferrable initially immediate;

alter table data_record
    drop constraint data_record_tenant_id_table_id_fkey,
    add constraint data_record_tenant_id_table_id_fkey
        foreign key (tenant_id, table_id) references data_table (tenant_id, id)
        on update cascade on delete cascade
        deferrable initially immediate;

alter table data_record_revision
    drop constraint data_record_revision_tenant_id_table_id_fkey,
    add constraint data_record_revision_tenant_id_table_id_fkey
        foreign key (tenant_id, table_id) references data_table (tenant_id, id)
        on update cascade on delete cascade
        deferrable initially immediate;

alter table run_event
    drop constraint run_event_tenant_id_run_id_fkey,
    add constraint run_event_tenant_id_run_id_fkey
        foreign key (tenant_id, run_id) references workflow_run (tenant_id, run_id)
        on delete cascade
        deferrable initially immediate;

alter table step_execution
    drop constraint step_execution_tenant_id_run_id_fkey,
    add constraint step_execution_tenant_id_run_id_fkey
        foreign key (tenant_id, run_id) references workflow_run (tenant_id, run_id)
        on delete cascade
        deferrable initially immediate;

alter table skill_installation
    drop constraint skill_installation_tenant_id_registry_entry_id_fkey,
    add constraint skill_installation_tenant_id_registry_entry_id_fkey
        foreign key (tenant_id, registry_entry_id) references registry_entry (tenant_id, id)
        deferrable initially immediate;

alter table workflow_delegation
    drop constraint workflow_delegation_tenant_id_workflow_id_fkey,
    add constraint workflow_delegation_tenant_id_workflow_id_fkey
        foreign key (tenant_id, workflow_id) references workflow_registration (tenant_id, workflow_id)
        on delete cascade
        deferrable initially immediate;

alter table workflow_trigger
    drop constraint workflow_trigger_tenant_id_workflow_id_fkey,
    add constraint workflow_trigger_tenant_id_workflow_id_fkey
        foreign key (tenant_id, workflow_id) references workflow_registration (tenant_id, workflow_id)
        on delete cascade
        deferrable initially immediate;
