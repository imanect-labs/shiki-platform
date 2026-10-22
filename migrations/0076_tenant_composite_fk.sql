-- 旧来の storage / chat 系テーブルの tenant 整合を DB で強制する（#815・多層防御）。
--
-- 新しいテーブル群（artifact / workflow / data）は `(tenant_id, id)` の複合 FK で親子の tenant を
-- DB が保証しているが、Phase 1〜3 由来のテーブルは `id` 単独の FK で、親子の tenant_id が食い違う
-- 行を DB が通していた。今のコードは前段でテナントを確認しているので越境は起きないが、アプリに
-- バグが 1 つ入れば越境し得る。本 migration はそれを DB で塞ぐ:
--
--   1. 親に `UNIQUE (tenant_id, id)` を張り、子の単一列 FK を `(tenant_id, 親id)` の複合 FK に置き換える
--   2. tenant_id 列の無い子（generation_event / collab_update / app_function_run）に tenant_id を足し、
--      既存行を親からバックフィルして NOT NULL ＋複合 FK にする
--   3. 一意性: office_lock の主キーに tenant_id を含める／app_installation の client_id に一意索引
--   4. 先頭が org の索引を tenant_id 先頭へ置き換える（同じ org slug を共有する他テナントの行を走査しない）
--
-- RLS はここでは導入しない（別途判断・#815）。
--
-- 複合 FK はすべて `DEFERRABLE INITIALLY IMMEDIATE`（migration 0061 と同じ流儀）。通常運用の検査
-- タイミングは従来どおり各文の末尾で、テナント移行（retenant）の txn だけが `SET CONSTRAINTS ALL
-- DEFERRED` で commit 時検査へ倒す。ON UPDATE は NO ACTION（遅延下では親子どちらを先に更新しても
-- commit 時に揃っていれば通る）、ON DELETE は元定義を維持する。
--
-- 既存データに親子の tenant 食い違いがあれば、FK 追加がその行を名指しして失敗する（黙って直さない。
-- 食い違い自体が調査対象のバグなので、運用者が確認してから流し直す）。
--
-- ⚠️ 大量データでの所要時間・ロック（単一 txn なので取ったロックは commit まで保持される）:
--   - `alter table ... add constraint ... unique` / `drop constraint` は ACCESS EXCLUSIVE を取る
--     （その表への**読み取りも**止まる）。単一 txn なので、node / thread / message / generation_run /
--     collab_doc 等は 2. のバックフィル完了〜commit まで読めない＝ドライブ・チャットは全停止する。
--   - `add constraint ... foreign key` は子を全件走査して検証する（親子とも SHARE ROW EXCLUSIVE 以上）。
--   - 2. のバックフィル UPDATE は対象表の全行を書き換える（行数ぶんの新タプル＝一時的に約 2 倍に膨らむ）。
--     `set not null` も全件走査（ACCESS EXCLUSIVE）。最も重いのは generation_event（トークン単位の
--     イベントログ）で、メンテナンス窓での適用を前提にする。
--   - 4. の索引置換も新規構築（構築中はその表への書込が止まる）。
--   - 列追加（default 無し）と主キー差し替え以外にテーブル本体の書換えは無い。

-- ---------------------------------------------------------------------------
-- 1a. 親側の (tenant_id, id) 一意制約（複合 FK の参照先）。
--     id 単独の主キーは残す（既存の参照・索引・`ON CONFLICT (id)` を壊さない）。
-- ---------------------------------------------------------------------------
alter table node                  add constraint node_tenant_id_id_key unique (tenant_id, id);
alter table thread                add constraint thread_tenant_id_id_key unique (tenant_id, id);
alter table message               add constraint message_tenant_id_id_key unique (tenant_id, id);
alter table generation_run        add constraint generation_run_tenant_id_run_id_key unique (tenant_id, run_id);
alter table collab_doc            add constraint collab_doc_tenant_id_node_id_key unique (tenant_id, node_id);
alter table app_function_schedule add constraint app_function_schedule_tenant_id_id_key unique (tenant_id, id);
alter table storage_event_outbox  add constraint storage_event_outbox_tenant_id_id_key unique (tenant_id, id);

-- ---------------------------------------------------------------------------
-- 1b. storage: node ツリー・版・アップロード宣言。
-- ---------------------------------------------------------------------------
alter table node
    drop constraint node_parent_id_fkey,
    add constraint node_tenant_id_parent_id_fkey
        foreign key (tenant_id, parent_id) references node (tenant_id, id)
        deferrable initially immediate;

alter table node_closure
    drop constraint node_closure_ancestor_fkey,
    drop constraint node_closure_descendant_fkey,
    add constraint node_closure_tenant_id_ancestor_fkey
        foreign key (tenant_id, ancestor) references node (tenant_id, id)
        deferrable initially immediate,
    add constraint node_closure_tenant_id_descendant_fkey
        foreign key (tenant_id, descendant) references node (tenant_id, id)
        deferrable initially immediate;

alter table node_version
    drop constraint node_version_node_id_fkey,
    add constraint node_version_tenant_id_node_id_fkey
        foreign key (tenant_id, node_id) references node (tenant_id, id)
        deferrable initially immediate;

alter table pending_upload
    drop constraint pending_upload_parent_id_fkey,
    drop constraint pending_upload_target_node_id_fkey,
    add constraint pending_upload_tenant_id_parent_id_fkey
        foreign key (tenant_id, parent_id) references node (tenant_id, id)
        deferrable initially immediate,
    add constraint pending_upload_tenant_id_target_node_id_fkey
        foreign key (tenant_id, target_node_id) references node (tenant_id, id)
        deferrable initially immediate;

alter table outbox_delivery
    drop constraint outbox_delivery_event_id_fkey,
    add constraint outbox_delivery_tenant_id_event_id_fkey
        foreign key (tenant_id, event_id) references storage_event_outbox (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

alter table collab_doc
    drop constraint collab_doc_node_id_fkey,
    add constraint collab_doc_tenant_id_node_id_fkey
        foreign key (tenant_id, node_id) references node (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

-- ---------------------------------------------------------------------------
-- 1c. chat: thread / message 配下。
-- ---------------------------------------------------------------------------
alter table message
    drop constraint message_thread_id_fkey,
    drop constraint message_parent_id_fkey,
    add constraint message_tenant_id_thread_id_fkey
        foreign key (tenant_id, thread_id) references thread (tenant_id, id)
        on delete cascade
        deferrable initially immediate,
    -- 親メッセージ削除時は parent_id だけを NULL にする（tenant_id まで NULL にすると NOT NULL 違反。PG15+）。
    add constraint message_tenant_id_parent_id_fkey
        foreign key (tenant_id, parent_id) references message (tenant_id, id)
        on delete set null (parent_id)
        deferrable initially immediate;

alter table generation_run
    drop constraint generation_run_message_id_fkey,
    drop constraint generation_run_thread_id_fkey,
    add constraint generation_run_tenant_id_message_id_fkey
        foreign key (tenant_id, message_id) references message (tenant_id, id)
        on delete cascade
        deferrable initially immediate,
    add constraint generation_run_tenant_id_thread_id_fkey
        foreign key (tenant_id, thread_id) references thread (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

alter table run_approval
    drop constraint run_approval_run_id_fkey,
    add constraint run_approval_tenant_id_run_id_fkey
        foreign key (tenant_id, run_id) references generation_run (tenant_id, run_id)
        on delete cascade
        deferrable initially immediate;

alter table thread_skill_pin
    drop constraint thread_skill_pin_thread_id_fkey,
    add constraint thread_skill_pin_tenant_id_thread_id_fkey
        foreign key (tenant_id, thread_id) references thread (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

alter table ui_action_invocation
    drop constraint ui_action_invocation_thread_id_fkey,
    drop constraint ui_action_invocation_message_id_fkey,
    add constraint ui_action_invocation_tenant_id_thread_id_fkey
        foreign key (tenant_id, thread_id) references thread (tenant_id, id)
        on delete cascade
        deferrable initially immediate,
    add constraint ui_action_invocation_tenant_id_message_id_fkey
        foreign key (tenant_id, message_id) references message (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

alter table ui_action_binding
    drop constraint ui_action_binding_thread_id_fkey,
    drop constraint ui_action_binding_message_id_fkey,
    add constraint ui_action_binding_tenant_id_thread_id_fkey
        foreign key (tenant_id, thread_id) references thread (tenant_id, id)
        on delete cascade
        deferrable initially immediate,
    add constraint ui_action_binding_tenant_id_message_id_fkey
        foreign key (tenant_id, message_id) references message (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

alter table ui_action_run
    drop constraint ui_action_run_thread_id_fkey,
    drop constraint ui_action_run_message_id_fkey,
    add constraint ui_action_run_tenant_id_thread_id_fkey
        foreign key (tenant_id, thread_id) references thread (tenant_id, id)
        on delete cascade
        deferrable initially immediate,
    add constraint ui_action_run_tenant_id_message_id_fkey
        foreign key (tenant_id, message_id) references message (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

alter table message_feedback
    drop constraint message_feedback_message_id_fkey,
    drop constraint message_feedback_thread_id_fkey,
    add constraint message_feedback_tenant_id_message_id_fkey
        foreign key (tenant_id, message_id) references message (tenant_id, id)
        on delete cascade
        deferrable initially immediate,
    add constraint message_feedback_tenant_id_thread_id_fkey
        foreign key (tenant_id, thread_id) references thread (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

alter table thread_insight
    drop constraint thread_insight_thread_id_fkey,
    add constraint thread_insight_tenant_id_thread_id_fkey
        foreign key (tenant_id, thread_id) references thread (tenant_id, id)
        on delete cascade
        deferrable initially immediate;

-- ---------------------------------------------------------------------------
-- 2. tenant_id 列の無い子テーブル: 列追加 → 親からバックフィル → NOT NULL → 複合 FK。
--    既存 FK（ON DELETE CASCADE）があるので親の無い孤児行は存在せず、全行が埋まる。
--    列は default 無しで足す（テーブル書換え無し）。INSERT は必ず tenant_id を明示する。
-- ---------------------------------------------------------------------------
alter table generation_event add column tenant_id text;
update generation_event e set tenant_id = r.tenant_id
    from generation_run r where r.run_id = e.run_id;
alter table generation_event
    alter column tenant_id set not null,
    drop constraint generation_event_run_id_fkey,
    add constraint generation_event_tenant_id_run_id_fkey
        foreign key (tenant_id, run_id) references generation_run (tenant_id, run_id)
        on delete cascade
        deferrable initially immediate;
-- purge / retenant / 件数集計は `where tenant_id = $1` で直接引く（tenant_scope）。主キーは
-- (run_id, seq) なので、tenant 先頭の索引が無いと共用プールの最大表を全件走査する。
create index generation_event_tenant_idx on generation_event (tenant_id, run_id);

alter table collab_update add column tenant_id text;
update collab_update u set tenant_id = d.tenant_id
    from collab_doc d where d.node_id = u.node_id;
alter table collab_update
    alter column tenant_id set not null,
    drop constraint collab_update_node_id_fkey,
    add constraint collab_update_tenant_id_node_id_fkey
        foreign key (tenant_id, node_id) references collab_doc (tenant_id, node_id)
        on delete cascade
        deferrable initially immediate;
create index collab_update_tenant_idx on collab_update (tenant_id, node_id);

alter table app_function_run add column tenant_id text;
update app_function_run r set tenant_id = s.tenant_id
    from app_function_schedule s where s.id = r.schedule_id;
alter table app_function_run
    alter column tenant_id set not null,
    drop constraint app_function_run_schedule_id_fkey,
    add constraint app_function_run_tenant_id_schedule_id_fkey
        foreign key (tenant_id, schedule_id) references app_function_schedule (tenant_id, id)
        on delete cascade
        deferrable initially immediate;
create index app_function_run_tenant_idx on app_function_run (tenant_id, schedule_id);

-- ---------------------------------------------------------------------------
-- 3. 一意性。
-- ---------------------------------------------------------------------------
-- office_lock: 主キーを tenant 込みにする。file_id 単独だと、期限切れロックの upsert
-- （ON CONFLICT (file_id)）が他テナントの行を自テナントの行へ書き換えられた（tenant_id の上書き）。
-- 既存行は file_id 一意なので (tenant_id, file_id) も一意（差し替えで失敗しない）。
alter table office_lock
    drop constraint office_lock_pkey,
    add primary key (tenant_id, file_id);

-- app_installation: azp（client_id）→ installation の解決を一意にする。一意でないと
-- `resolve_active_by_client` の `LIMIT 1` がどれか 1 つの installation に解決してしまう。
-- client_id は app-platform が `app-{app_id}-b1` / `app-{app_id}-b2` と app_id から決定的に採番し、
-- (tenant_id, app_id) は一意なので、アプリ経路で作られた行は重複しない（手で入れた fixture を除く）。
-- 解決クエリは tenant_id 込みで引くため、索引も tenant 先頭（同じ client を別テナントが持つのは正常）。
drop index app_installation_client_b1_idx;
drop index app_installation_client_b2_idx;
create unique index app_installation_client_b1_uidx
    on app_installation (tenant_id, client_id_b1)
    where client_id_b1 is not null;
create unique index app_installation_client_b2_uidx
    on app_installation (tenant_id, client_id_b2)
    where client_id_b2 is not null;

-- ---------------------------------------------------------------------------
-- 4. 先頭が org の索引を tenant_id 先頭へ置き換える。
--    org slug は tenant をまたいで共有され得るため、org 先頭だと他テナントの行まで走査する。
--    （org, tenant_id, ...) と等値で両方を引く索引（node_sibling_name_uidx 等）は対象外: 2 列目も
--    等値で絞れるので他テナントの行を走査しない。
-- ---------------------------------------------------------------------------
drop index blob_gc_idx;
create index blob_gc_idx on blob (tenant_id, org) where refcount = 0;

drop index node_parent_idx;
create index node_parent_idx on node (tenant_id, org, parent_id) where deleted_at is null;

-- blob への複合 FK (tenant_id, org, blob_sha256) と同じ並びにし、blob 側の RI 検査にも使えるようにする。
drop index node_blob_idx;
create index node_blob_idx on node (tenant_id, org, blob_sha256);

-- audit_log の object/actor 索引は 0075（#834）で tenant 先頭（audit_log_tenant_{object,actor}_idx）に
-- 置き換え済みなので、ここでは触らない。
