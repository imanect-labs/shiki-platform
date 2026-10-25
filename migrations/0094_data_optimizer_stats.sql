-- Phase 15 Task 15.12: 自前クエリオプティマイザ O2（統計・コスト・フィードバック・固定戦略）
-- （docs/data/query-optimizer.md §4・§5・§7.3・§8）。
--
-- 統計 4 表はいずれも `tenant_int` / `table_int` を先頭に持ち、tenant スコープ（shard_safety_lint の S3）と
-- purge 経路（DataStore::purge_tenant・storage の tenant_int 導出）の対象に入る。
--
-- **統計は可視性を無視して全行から集める**（PIT-75）。読取経路は `crates/data/src/query/planner/` に閉じ、
-- 利用者・API・エラー文言へ値も推定も出さない。書くのは outbox の消費側（行数・値頻度）と統計収集ジョブだけ。

-- テーブル単位: 行数（outbox 消費側で増減）・前回収集からの変更行数・行幅・値頻度表の初期化状態。
create table data_table_stats (
    tenant_int      integer     not null,
    table_int       bigint      not null,
    -- 行数（outbox 消費側の create +1 / delete -1。統計収集で数え直す）。
    n_rows          bigint      not null default 0,
    -- 前回の統計収集からの変更行数（起動条件: > 50 + 0.1 × n_rows）。
    changes_since   bigint      not null default 0,
    -- 値頻度表と行数が数え直し済みか（偽の間は planner が使わない）。
    counts_ready    boolean     not null default false,
    -- 値頻度表を持つ列（種類数が上限 1,024 以下の分類列と owner 擬似列 0）。消費側は列がここに無ければ足さない。
    counts_fields   smallint[]  not null default '{}',
    -- 数え直しの時点で既に索引へ反映済みだった未消費のイベント（消費側が二重に足さない・消費したら外す）。
    skip_events     bigint[]    not null default '{}',
    -- 行幅（本体の平均バイト数）と TOAST 率（data-platform §16.5 の covering read の判定材料）。
    avg_width       real,
    toast_frac      real,
    analyzed_at     timestamptz,
    updated_at      timestamptz not null default now(),
    primary key (tenant_int, table_int)
);

-- フィールド単位（索引表ごと）: 値を持つ行数・索引行数・異なり数・最頻値 100・等深ヒストグラム 100 区間・
-- 切り詰め率・多値の平均要素数・値の並びと record_int の並びの相関（-1〜1）。
create table data_field_stats (
    tenant_int      integer     not null,
    table_int       bigint      not null,
    field_id        smallint    not null check (field_id >= 0),
    index_table     smallint    not null,
    n_values        bigint      not null,
    n_entries       bigint      not null,
    n_distinct      bigint      not null,
    -- [[値, 頻度(0〜1)], …]（頻度の降順・最大 100）。
    mcv             jsonb       not null default '[]',
    -- 等深ヒストグラムの境界（101 個まで・text 表現）。
    histogram       jsonb       not null default '[]',
    truncated_frac  real        not null default 0,
    avg_elems       real        not null default 1,
    correlation     real        not null default 0,
    sampled         boolean     not null default false,
    analyzed_at     timestamptz not null default now(),
    primary key (tenant_int, table_int, field_id, index_table)
);

-- 値頻度表（種類数が上限 1,024 以下の分類列と owner 擬似列）。outbox 消費側が
-- `ON CONFLICT DO UPDATE SET cnt = cnt + excluded.cnt` でバッチ反映する（書込 Tx では更新しない）。
create table data_value_count (
    tenant_int  integer  not null,
    table_int   bigint   not null,
    field_id    smallint not null check (field_id >= 0),
    val         text     collate "C" not null,
    cnt         bigint   not null,
    primary key (tenant_int, table_int, field_id, val)
);

-- リンク統計（リンク列 × 方向）: リンクを持つ行の割合・平均・最大ファンアウト。
create table data_link_stats (
    tenant_int    integer     not null,
    table_int     bigint      not null,
    field_id      smallint    not null check (field_id >= 0),
    is_reverse    boolean     not null,
    n_src         bigint      not null,
    n_edges       bigint      not null,
    max_fanout    integer     not null,
    link_frac     real        not null,
    analyzed_at   timestamptz not null default now(),
    primary key (tenant_int, table_int, field_id, is_reverse)
);

-- 機微なテーブル向けの固定戦略（§7.3・PIT-75 の残余リスク）。真ならコストベース選択・件数探索・
-- 統計の参照を止め、統計に依らない固定の計画（安全計画）で読む。
alter table data_table add column planner_fixed boolean not null default false;
