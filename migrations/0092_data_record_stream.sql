-- Phase 15 Task 15.6: レコード変更 outbox ＋ SSE 差分配信 ＋ 索引の自動昇格（data-platform §9・§10）。
--
-- 1. **配信シーケンス**（§9.2）: outbox の配送台帳 `outbox_delivery` に購読ストリームごとの連番
--    `delivery_seq` を持たせる。**グローバルな bigserial にはしない**——共有採番だと他ストリームの
--    イベントで番号に穴が空き、「連続確認済み（low-water mark）」が定義できなくなる。採番は
--    `outbox_delivery_seq(consumer, next_seq)` から consumer ごとに払い出し、**その行ロックが並行
--    リレー間の排他も兼ねる**（同じ consumer への採番は直列化され、コミット順＝番号順になる）。
-- 2. **保持期間**（§9.2 の `stream.reset`／§3.3 のスナップショット読取の 410）: 採番した台帳行は
--    `retain_until` まで GC しない。保持期間を過ぎた位置からの再開は `stream.reset`（SSE）・410（読取）。
-- 3. **consumer 非公開の材料**: 可視性の before 判定に要る旧値（旧 owner・旧述語参照列）と内部キーは
--    `private_payload` に置く。汎用の consumer（workflow / miniapp-functions / app-gateway のライブテール /
--    RAG relay）はこの列を読まない＝外へ出る経路が構造的に無い。

alter table storage_event_outbox add column private_payload jsonb;

-- 購読ストリーム（consumer）ごとの次の配信位置。`tenant_id` は撤去（purge）の対象にするため
-- （構造化データのテーブル単位ストリームはテナントに属する。台帳 consumer は NULL）。
create table outbox_delivery_seq (
    consumer  text        primary key,
    next_seq  bigint      not null default 1 check (next_seq >= 1),
    tenant_id text,
    updated_at timestamptz not null default now()
);

alter table outbox_delivery add column delivery_seq bigint;
alter table outbox_delivery add column retain_until timestamptz;
-- 購読者内で穴のない連番（同じ consumer に同じ番号は 2 つ無い）。リプレイ・ライブテールは
-- `consumer = ? AND delivery_seq > ? ORDER BY delivery_seq` でこの索引を前から読む。
create unique index outbox_delivery_seq_uidx
    on outbox_delivery (consumer, delivery_seq) where delivery_seq is not null;

-- スナップショット読取が「まだ採番されていない変更」をテーブル単位で引く（§3.3）。
create index storage_event_outbox_data_change_idx
    on storage_event_outbox (tenant_id, (payload ->> 'table_id'), id)
    where payload ->> 'event_type' = 'data.record.changed';

-- 認可側の変更（ロール・共有・テーブル権限）の世代（§9.4・PIT-70 ③）。SSE の購読は TTL を待たず、
-- 世代が進んだら材料を引き直す。`scope` はテーブル id（テナント全体は nil uuid）。
create table data_stream_authz_epoch (
    tenant_id  text        not null,
    scope      uuid        not null,
    epoch      bigint      not null default 1,
    updated_at timestamptz not null default now(),
    primary key (tenant_id, scope)
);

-- awareness（他ユーザーのカーソル・選択範囲・§9.5）。**データを含まない**（レコード id と列の参照名だけ）。
-- 主体ごとに 1 行を上書きし、古い行は配信しない（短命）。
create table data_stream_awareness (
    tenant_id   text        not null,
    table_id    uuid        not null,
    principal   text        not null,
    record_id   uuid,
    field_key   text,
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, table_id, principal)
);
create index data_stream_awareness_recent_idx
    on data_stream_awareness (tenant_id, table_id, updated_at);

-- 索引の自動昇格の上限（§10・PIT-72）: テーブルあたりの索引列数（擬似列を除く）。NULL は既定 20。
alter table data_tenant_settings
    add column max_indexed_fields integer
        check (max_indexed_fields is null or max_indexed_fields between 1 and 1000);

-- 索引の降格（§10「逆方向も同じ機構で」）: 大きなテーブルの索引行の削除はスキーマ改訂の Tx で
-- 行わず、ジョブがチャンクで消す。`high_water` は降格時点の最大 record_int（それより後に作られた
-- レコードの行は、再昇格後の write-through の行なので消さない）。
create table data_index_purge (
    tenant_int  integer     not null,
    table_int   bigint      not null,
    field_id    smallint    not null,
    index_table smallint    not null,
    high_water  bigint      not null,
    created_at  timestamptz not null default now(),
    primary key (tenant_int, table_int, field_id, index_table)
);
