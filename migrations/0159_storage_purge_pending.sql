-- ドライブの完全削除（GAP-108）の片付け待ち。
--
-- 完全削除は、ノード・版・blob の行を 1 つのトランザクションで消す。DB の外にあるもの（OpenFGA のタプルと、
-- オブジェクトストアの本体）はトランザクションに入れられないので、消す仕事をここに 1 行ずつ積み、commit の後に
-- 掃除（storage::service::purge_pending・中断アップロードの回収タイマに相乗り）が片付ける。成功した行は消し、
-- 失敗した行は attempts と last_error を残して次の回に回す。
--
--   kind = 'node_tuples': そのノード（node_id・node_kind）に直接張られたタプルをすべて外す。
--   kind = 'object'     : blob の本体（object_key）を消す。掃除は blob のロック（bump_blob と同じ advisory xact
--                         lock）の中で、(tenant_id, org, sha256) の blob の行が無いことを確かめてから消す。同じ内容を
--                         同時に保存して行が作り直されていれば消さない（共有の本体を消して他の参照を壊さない・PIT-14）。
--                         object_key は blob_object_key(tenant_id, org, sha256) と一致するものだけを消す。
--
-- 行を入れるのは完全削除のトランザクションの中だけ（DB の行と片付け待ちの行が一緒に確定する）。

create table storage_purge_pending (
    tenant_id  text        not null,
    id         uuid        not null default gen_random_uuid(),
    org        text        not null,
    kind       text        not null check (kind in ('node_tuples', 'object')),
    node_id    uuid,
    node_kind  text        check (node_kind in ('file', 'folder')),
    sha256     text,
    object_key text,
    attempts   int         not null default 0 check (attempts >= 0),
    last_error text,
    created_at timestamptz not null default now(),
    primary key (tenant_id, id),
    -- 種類ごとに要る列だけを持つ（掃除が取り違えないように、形を DB で保証する）。
    check (
        (kind = 'node_tuples' and node_id is not null and node_kind is not null
            and sha256 is null and object_key is null)
        or (kind = 'object' and sha256 is not null and object_key is not null
            and node_id is null and node_kind is null)
    )
);

-- 掃除は古い行から取る（テナントを横断する・FOR UPDATE SKIP LOCKED）。
create index storage_purge_pending_created_idx on storage_purge_pending (created_at);

-- RLS（0114 と同じポリシー・移行モード）。新しいテナント表は、作る migration で付ける（docs/guides/db-maintenance.md §5）。
alter table storage_purge_pending enable row level security;
alter table storage_purge_pending force row level security;
create policy shiki_tenant_isolation on storage_purge_pending
    using ((select shiki_rls_tenant()) is null or tenant_id = (select shiki_rls_tenant()))
    with check ((select shiki_rls_tenant()) is null or tenant_id = (select shiki_rls_tenant()));
