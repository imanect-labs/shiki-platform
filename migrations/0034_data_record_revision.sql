-- Phase 9 Task 9.5: レコード・リビジョン履歴（追記型 changelog）。
--
-- 「誰が・いつ・どのフィールドをどう変えたか」をフィールド単位差分で残す。
-- 行は INSERT のみ（不変）。record 側の rev と 1:1 に対応し、楽観ロック（rev 不一致 409）と
-- 同一トランザクションで書かれる。record が消えても履歴は table の生存期間だけ残す
-- （FK は data_table へ張り、record へは張らない）。

create table data_record_revision (
    tenant_id  text        not null,
    record_id  uuid        not null,
    table_id   uuid        not null,
    -- record.rev と同値（この改訂を適用した後の値・1 始まり）。
    rev        bigint      not null,
    -- 変更した subject（principal.id）。
    changed_by text        not null,
    -- create / update / delete / transition（transition は Task 9.10）。
    change_kind text       not null check (change_kind in (
        'create', 'update', 'delete', 'transition'
    )),
    -- フィールド単位差分 [{"field": .., "old": .., "new": ..}]（create は old=null、
    -- delete は new=null の全フィールド）。
    patch      jsonb       not null,
    trace_id   text,
    created_at timestamptz not null default now(),
    primary key (tenant_id, record_id, rev),
    foreign key (tenant_id, table_id)
        references data_table (tenant_id, id) on delete cascade on update cascade
);

-- レコード履歴の時系列取得（rev 降順）に PK がそのまま効く。テーブル横断の監査集計用に
-- table_id 側の索引も張る。
create index data_record_revision_table_idx
    on data_record_revision (tenant_id, table_id, created_at desc);
