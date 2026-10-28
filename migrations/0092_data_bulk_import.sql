-- Phase 15 Task 15.11: 一括書込パス（data-platform §16.7）。
--
-- 10 万行のインポートを ① ステージング表へ COPY → ② 通常書込と同じ検証器で正規検証 →
-- ③ チャンク Tx で適用 の 3 段で行う。**authoritative な表（data_record / 索引）へは COPY しない**。
-- 適用は 1 件の create と同じ入口（crates/data/src/record_effects.rs）で本体・索引行・revision・
-- 材料化を書く。各段は jobq のチャンクジョブで、取込 ID ＋行番号で冪等・中断再開できる。
--
-- 3 表とも tenant_int で持つ（data の索引表と同じ・purge は crates/data の撤去経路が消す）。
-- record_int / table_int は内部キーで、観測 API は件数と行番号・行キーだけを返す（PIT-75）。

-- 取込 1 件の進捗。
create table data_bulk_import (
    tenant_int        integer     not null,
    id                uuid        not null,
    table_int         bigint      not null,
    table_id          uuid        not null,
    -- 取込の主体（ジョブで AuthContext を組み直す・昇格しない。チャンクごとに editor を問い直す）。
    principal_kind    text        not null,
    principal_id      text        not null,
    org               text        not null,
    trace_id          text,
    -- validating → applying → indexing → done（恒久エラーは failed）。
    status            text        not null default 'validating'
        check (status in ('validating', 'applying', 'indexing', 'done', 'failed')),
    -- 検証に使ったスキーマの版。適用時に版が変わっていたら未適用の行を検証し直す。
    schema_version    bigint      not null,
    -- 検証で不合格の行が 1 つでもあれば何も適用しない（全件か無か）。
    abort_on_invalid  boolean     not null default false,
    -- 索引の遅延構築（data-platform §10 の building を流用）を要求するか（auto / immediate / deferred）。
    index_mode        text        not null default 'auto'
        check (index_mode in ('auto', 'immediate', 'deferred')),
    -- この取込が値索引を building に保持している（保持中はバックフィルが active へ切り替えない）。
    index_hold        boolean     not null default false,
    rows_total        bigint      not null,
    -- 検証・適用の進捗（行番号。この番号以下は処理済み）。
    validate_cursor   bigint      not null default 0,
    apply_cursor      bigint      not null default 0,
    rows_valid        bigint      not null default 0,
    rows_invalid      bigint      not null default 0,
    rows_applied      bigint      not null default 0,
    -- 検証後・適用時に並行書込と競合して弾かれた行（unique・リンクのカーディナリティ）。
    rows_rejected     bigint      not null default 0,
    -- 適用した最小の record_int（遅延構築のバックフィルの開始位置・内部キー）。
    first_record_int  bigint,
    parity_checked_at timestamptz,
    parity_mismatches integer,
    last_error        text,
    created_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now(),
    finished_at       timestamptz,
    primary key (tenant_int, id)
);

-- バックフィルの切替判定（このテーブルを保持している取込があるか）。
create index data_bulk_import_hold_idx on data_bulk_import (tenant_int, table_int)
    where index_hold;
-- 未完了の取込の拾い直し（ジョブを失ったとき）。
create index data_bulk_import_open_idx on data_bulk_import (status, updated_at)
    where status in ('validating', 'applying', 'indexing');

-- ステージング（① COPY の行き先）。payload は COPY 時に JSONB として構文検査だけ受ける。
-- state: 0 = 未検証 / 1 = 合格 / 2 = 不合格 / 3 = 適用済み / 4 = 適用時に競合で拒否。
create table data_bulk_staging (
    tenant_int integer  not null,
    import_id  uuid     not null,
    -- 1 始まりの行番号（取込内で一意・チャンク境界の冪等性の単位）。
    row_no     bigint   not null,
    -- 呼び出し側の行キー（任意・取込内で一意）。エラー報告で行を特定する。
    row_key    text,
    -- 作成するレコードの外部 ID（COPY 時に採番）。本体の主キーと一致するので二重適用は起きない。
    record_id  uuid     not null,
    payload    jsonb    not null,
    state      smallint not null default 0 check (state between 0 and 4),
    -- 検証済みの正規化値（外部形 key → 値）。適用はこの値を書く。
    normalized jsonb,
    error      text,
    primary key (tenant_int, import_id, row_no)
);

create unique index data_bulk_staging_key_idx on data_bulk_staging (tenant_int, import_id, row_key)
    where row_key is not null;

-- 取込内の一意性の予約（unique 列の値・一対一 / 一対多リンクの参照先）。取込の中で同じ値を
-- 2 行が持つと後の行を不合格にする（チャンクを跨いでも検出する）。
create table data_bulk_key (
    tenant_int integer  not null,
    import_id  uuid     not null,
    field_id   smallint not null,
    key        text     not null,
    row_no     bigint   not null,
    primary key (tenant_int, import_id, field_id, key)
);
