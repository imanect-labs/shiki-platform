-- Phase 15 Task 15.2: pivot 索引テーブル＋data_record のハッシュパーティション化＋索引状態の台帳。
--
-- 設計（docs/data-platform.md §3・§10・§13 M2・§16.7）:
--   * 索引の物理本数を「テナント数 × テーブル数 × 索引列数」から切り離す。v1 は宣言フィールドごとに
--     data_record へ partial 式インデックスを張っていた（crates/data/src/index/legacy.rs）。v2 は
--     「どの値がどのレコードにあるか」だけを型付きで持つ共有の索引テーブルへ行として書く。
--     **索引の総本数はパーティションあたりの定数**で、テーブルや列が増えても変わらない（§3.3）。
--   * 全テーブル `PARTITION BY HASH (table_int)`。クエリは常に `tenant_int = $1 AND table_int = $2` を
--     持つので 1 パーティションへ枝刈りされる（§3.8）。分割数はプロビジョニング時の設定値
--     `shiki.data_partitions`（未設定なら 64）。増やすには再ハッシュが要るので初期値は余裕をもって決める。
--   * 物理列順は**固定幅の列を幅の降順**に並べる（int2 の直後に int8 が来るとアラインメントの
--     パディングで record_int による節約が消える・docs/data/internal-keys-benchmark.md）。
--     主キー・索引の列順は §3.3 のまま（並べ替えるのはテーブル定義の物理列順だけ）。
--   * M2 は**二重書込**期間: 旧 partial index と新索引テーブルの両方を更新する（読取は旧経路のまま・
--     切替と旧方式の撤去は Task 15.3）。新テーブルを無視すれば旧経路がそのまま動く（切り戻し可能）。

-- ---------------------------------------------------------------------------
-- ⓪ 拡張の事前確認（pg_trgm = gin_trgm_ops / btree_gin = スコープ列の GIN 格納）
-- ---------------------------------------------------------------------------
-- 欠けていると後段の CREATE INDEX が分かりにくいエラーで落ちる。マネージド PostgreSQL では拡張の
-- 利用可否が制限されることがあるため（Cloud SQL / AlloyDB はいずれも利用可能）、**何が足りないかを
-- 名指しして明示的に失敗させる**。プロビジョニングの前提条件（docs/data-platform.md §3.3）。
create or replace function data_require_extensions(required text[]) returns void
language plpgsql as $$
declare
    missing text[];
begin
    select array_agg(r order by r) into missing
      from unnest(required) as r
     where not exists (select 1 from pg_available_extensions a where a.name = r);
    if missing is not null then
        raise exception 'shiki: PostgreSQL 拡張 % が利用できません（構造化データの索引テーブルに必須）', array_to_string(missing, ', ')
            using errcode = 'feature_not_supported',
                  hint = 'postgresql-contrib を導入するか、マネージド PostgreSQL の拡張 allowlist で有効化してから再実行してください（docs/data-platform.md §3.3）';
    end if;
end $$;

select data_require_extensions(array['pg_trgm', 'btree_gin']);
create extension if not exists pg_trgm;
create extension if not exists btree_gin;

-- ハッシュパーティションを n 個作る（<parent>_p<remainder>）。分割数は全索引テーブルで揃える。
create or replace function data_create_hash_partitions(parent text, n integer) returns void
language plpgsql as $$
begin
    if parent !~ '^data_[a-z_]+$' then
        raise exception 'data_create_hash_partitions: 不正な表名 %', parent;
    end if;
    if n < 1 or n > 1024 then
        raise exception 'data_create_hash_partitions: 分割数 % は 1〜1024 で指定してください', n;
    end if;
    for r in 0 .. n - 1 loop
        execute format(
            'create table %I partition of %I for values with (modulus %s, remainder %s)',
            parent || '_p' || r, parent, n, r);
    end loop;
end $$;

-- ---------------------------------------------------------------------------
-- ① data_record をハッシュパーティション表へ作り直す（PK = (tenant_int, table_int, id)）
-- ---------------------------------------------------------------------------
-- 旧表は宣言フィールドごとの partial 式インデックス（dr_*）をぶら下げているが、新表へは引き継げない
-- （式インデックスは leaf パーティションへ張り直す・legacy.rs）。台帳を空にしておけば、テーブルの
-- 初回アクセス時の初期化（index/state.rs）か常駐ワーカーが張り直す。張り直すまでの間も読取は正しい
-- （索引は速さにしか効かない）。unique は初期化が同じ Tx で旧 unique を張り直してから書込を通す。
alter table data_record rename to data_record_v1;
alter table data_record_v1 rename constraint data_record_pkey to data_record_v1_pkey;
alter table data_record_v1 rename constraint data_record_tenant_id_table_id_fkey
    to data_record_v1_tenant_id_table_id_fkey;
drop index data_record_int_idx;
drop index data_record_table_idx;
delete from data_index_registry;
-- 旧表を落とすと所有シーケンスも消えるため、先に所有を外す（採番は不変・再利用しない）。
alter sequence data_record_int_seq owned by none;

create table data_record (
    id         uuid        not null default gen_random_uuid(),
    table_id   uuid        not null,
    table_int  bigint      not null,
    -- 内部キー（§3.2）。プール共通シーケンス。**外へ出さない**（PIT-75）。
    record_int bigint      not null default nextval('data_record_int_seq'),
    rev        bigint      not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    tenant_int integer     not null,
    tenant_id  text        not null,
    org        text        not null,
    owner      text        not null,
    data       jsonb       not null,
    primary key (tenant_int, table_int, id),
    foreign key (tenant_id, table_id) references data_table (tenant_id, id)
        on update cascade on delete cascade
        deferrable initially immediate
) partition by hash (table_int);

select data_create_hash_partitions('data_record',
    coalesce(nullif(current_setting('shiki.data_partitions', true), '')::integer, 64));

-- 索引テーブルから本体を引く経路（ページの本体取得）。
create unique index data_record_int_idx on data_record (tenant_int, table_int, record_int);
-- v1 の既定並び（updated_at 降順）。Task 15.3 で擬似列 updated_at（field_id 2）の索引へ置き換える。
create index data_record_table_idx on data_record (tenant_int, table_int, updated_at desc, id desc);

insert into data_record (id, table_id, table_int, record_int, rev, created_at, updated_at,
                         tenant_int, tenant_id, org, owner, data)
select id, table_id, table_int, record_int, rev, created_at, updated_at,
       tenant_int, tenant_id, org, owner, data
  from data_record_v1;

drop table data_record_v1;
alter sequence data_record_int_seq owned by data_record.record_int;

-- ---------------------------------------------------------------------------
-- ② pivot 索引テーブル（§3.3）。val は「索引キー」であって値の正本ではない（正本は data_record.data）
-- ---------------------------------------------------------------------------
-- text 系（text / select / multi_select / date / date_time / user_ref / role_ref / file_ref / status ＋
-- システム擬似列 field_id 0=owner・1=created_at・2=updated_at）。btree キー上限（ページの 1/3 ≒ 2,704B）を
-- 超えないよう MAX_INDEX_KEY_BYTES = 2,000B のプレフィクスだけを持ち、切り詰めた行は truncated = true
-- （等値・前方一致は候補を本体で再確認する）。COLLATE "C" はバイト順の決定性のため（§3.4）。
create table data_index_text (
    table_int  bigint   not null,
    record_int bigint   not null,
    tenant_int integer  not null,
    field_id   smallint not null check (field_id >= 0),
    ord        smallint not null default 0 check (ord >= 0),
    truncated  boolean  not null default false,
    val        text     collate "C" not null check (octet_length(val) <= 2000),
    primary key (tenant_int, table_int, field_id, record_int, ord)
) partition by hash (table_int);

-- number（numeric = 正確な小数・NumericSpec の正規化後の値）。
create table data_index_num (
    table_int  bigint   not null,
    record_int bigint   not null,
    tenant_int integer  not null,
    field_id   smallint not null check (field_id >= 0),
    ord        smallint not null default 0 check (ord >= 0),
    val        numeric  not null,
    primary key (tenant_int, table_int, field_id, record_int, ord)
) partition by hash (table_int);

-- リンク列ごとの宣言メタデータ。スキーマ改訂時にのみ書き換わる（レコード書込では触らない）。
-- リンク列ごとに 1 行の小さな表なのでパーティションしない（レコード数に比例しない）。
create table data_link_field (
    table_int     bigint   not null,
    tenant_int    integer  not null,
    field_id      smallint not null check (field_id >= 0),
    -- この列の行（src 側）が単一値しか持てないとき true（LinkDef.cardinality から決まる）。
    single_valued boolean  not null,
    primary key (tenant_int, table_int, field_id),
    unique (tenant_int, table_int, field_id, single_valued)
);

-- リンク（record_ref）。1 エッジにつき順方向行と逆方向行の 2 行（隣接リストの標準形）。
-- どちらの行も table_int = 自分が所属する側のテーブルなので、両方向とも 1 パーティションへ枝刈りされる。
create table data_link (
    table_int     bigint   not null,  -- この行が所属する側のテーブル（パーティションキー）
    src_record    bigint   not null,  -- この行が所属する側のレコード（record_int）
    dst_table_int bigint   not null,  -- 相手側のテーブル
    dst_record    bigint   not null,  -- 相手側のレコード（record_int）
    tenant_int    integer  not null,
    field_id      smallint not null check (field_id >= 0),  -- 順方向はリンク列・逆方向は対称列
    ord           smallint not null default 0 check (ord >= 0),
    is_reverse    boolean  not null default false,  -- 監査・再構築用（クエリでは使わない）
    -- カーディナリティ強制用。書込側に決めさせないため、下の複合 FK で宣言値に固定する（PIT-72）。
    single_valued boolean  not null,
    primary key (tenant_int, table_int, field_id, src_record, ord)
) partition by hash (table_int);

-- 【重要】single_valued を宣言値に固定する。書込側が false を指定しても、宣言が true なら
-- 参照先の組が存在せず FK 違反で落ちる（＝部分一意を迂回できない）。
alter table data_link add constraint data_link_cardinality_fk
    foreign key (tenant_int, table_int, field_id, single_valued)
    references data_link_field (tenant_int, table_int, field_id, single_valued);

-- unique 制約。1 本の PK で全テーブル・全フィールドの一意性を賄う。長い値は
-- `<先頭 2,000B> || ':' || sha256(全体)` に正規化する（index/normalize.rs）。
create table data_unique (
    table_int  bigint   not null,
    record_int bigint   not null,
    tenant_int integer  not null,
    field_id   smallint not null check (field_id >= 0),
    val_norm   text     collate "C" not null,
    primary key (tenant_int, table_int, field_id, val_norm)
) partition by hash (table_int);

-- 部分一致（searchable 宣言の text 列のみ）。trigram GIN を「1 本だけ」張るための隔離先。
-- 全文を持つ（GIN は trigram 単位なので btree のキー長制約を受けない）。
create table data_index_search (
    table_int  bigint   not null,
    record_int bigint   not null,
    tenant_int integer  not null,
    field_id   smallint not null check (field_id >= 0),
    ord        smallint not null default 0 check (ord >= 0),
    val        text     not null,
    primary key (tenant_int, table_int, field_id, record_int, ord)
) partition by hash (table_int);

do $$
declare
    n integer := (select count(*) from pg_inherits where inhparent = 'data_record'::regclass);
begin
    perform data_create_hash_partitions('data_index_text', n);
    perform data_create_hash_partitions('data_index_num', n);
    perform data_create_hash_partitions('data_link', n);
    perform data_create_hash_partitions('data_unique', n);
    perform data_create_hash_partitions('data_index_search', n);
end $$;

-- 値索引 2 本（text / num）。駆動索引として `(val, record_int)` の keyset で歩く（§3.5）。
create index data_index_text_val_idx
    on data_index_text (tenant_int, table_int, field_id, val, record_int);
create index data_index_num_val_idx
    on data_index_num (tenant_int, table_int, field_id, val, record_int);
-- 宣言したカーディナリティを DB で強制する（部分一意 1 本）。2 行方式なので dst 側の一意も
-- 逆方向行に対する同じ制約として表現できる（§7.1）。
create unique index data_link_single_idx
    on data_link (tenant_int, table_int, field_id, src_record)
    where single_valued;
-- スコープ列を GIN 自体に含める（btree_gin）。含めないと 1 テナントの検索でもプール全体の
-- posting list を読み、ノイジーネイバーが p95 を壊す。
create index data_index_search_trgm_idx
    on data_index_search using gin (tenant_int, table_int, field_id, val gin_trgm_ops);

-- ---------------------------------------------------------------------------
-- ③ 索引状態の台帳（§10: absent / building / active）とバックフィルの進捗
-- ---------------------------------------------------------------------------
-- (フィールド, 索引テーブル) ごとの状態。building の間は書込が write-through し（PIT-72）、
-- クエリの駆動にもフィルタにも使わせない（Task 15.3 のクエリコンパイラが見る）。
-- absent → building の遷移は data_table 行の FOR UPDATE の下でだけ行う（書込は同じ行を
-- FOR KEY SHARE で押さえるので、遷移前に状態を読んだ書込は遷移より先に必ず終わる）。
create table data_index_state (
    table_int   bigint      not null,
    updated_at  timestamptz not null default now(),
    tenant_int  integer     not null,
    field_id    smallint    not null check (field_id >= 0),
    -- 1 = data_index_text / 2 = data_index_num / 3 = data_link / 4 = data_unique / 5 = data_index_search
    index_table smallint    not null check (index_table between 1 and 5),
    state       text        not null check (state in ('absent', 'building', 'active')),
    primary key (tenant_int, table_int, field_id, index_table)
);

-- テーブル単位のバックフィル進捗（jobq のチャンクジョブ・冪等・中断再開・進捗観測）。
-- cursor_record / high_water は record_int（内部キー）。**外へ出さない**（観測 API は件数だけ返す）。
create table data_index_backfill (
    table_int         bigint      not null,
    cursor_record     bigint      not null default 0,
    high_water        bigint      not null default 0,
    rows_done         bigint      not null default 0,
    rows_total        bigint      not null default 0,
    generation        bigint      not null default 1,
    started_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now(),
    finished_at       timestamptz,
    parity_checked_at timestamptz,
    tenant_int        integer     not null,
    parity_mismatches integer,
    status            text        not null default 'running'
        check (status in ('running', 'done', 'failed')),
    last_error        text,
    primary key (tenant_int, table_int)
);

-- 既存テーブルは台帳が未初期化（false）。初回アクセス時か常駐ワーカーが初期化し、
-- 既存レコードを building としてバックフィルする。新規作成は作成 Tx で台帳を作って true にする。
alter table data_table add column pivot_index_init boolean not null default false;
