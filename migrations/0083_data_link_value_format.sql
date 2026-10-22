-- Phase 15 Task 15.4 追補: v1 の record_ref の保存値（単一 UUID 文字列）を配列へ移行する
-- （data-platform §7.1「値の表現は常に配列」・§13 M1）。
--
-- 書込は常に配列を書き、読取は移行の前後どちらの形も同じ配列として返す（crate::field_key::upgrade_link_value）。
-- 既存レコードの書き換えはオンラインのチャンク処理（crate::link_values）で行う。テナント・テーブル単位で
-- record_int 順に進め、冪等（文字列の値だけを書き換える）で、中断しても進捗から再開する。

-- ① テーブルごとのリンク値の形式。1 = v1 の文字列が残り得る / 2 = 全て配列（移行済み）。
--    既存テーブル（と、ローリング更新中に旧バイナリが作るテーブル）は 1 から始め、常駐ワーカーが移行する。
--    新しいバイナリは作成時に 2 を書く（最初から配列しか書かない）。
alter table data_table add column link_value_format smallint not null default 1
    check (link_value_format in (1, 2));
create index data_table_link_value_pending_idx on data_table (tenant_id, id)
    where link_value_format = 1;

-- ② 移行の進捗（テーブル単位・data_index_backfill と同じ形）。
--    cursor_record まで処理済み。末尾まで進んだら全行を検査し、文字列が残っていれば（旧バイナリの書込・
--    行ロック中で飛ばした行）先頭からもう一周する（passes）。残りが無ければ形式を 2 にして done。
create table data_link_value_migration (
    table_int         bigint      not null,
    cursor_record     bigint      not null default 0,
    high_water        bigint      not null default 0,
    rows_converted    bigint      not null default 0,
    started_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now(),
    finished_at       timestamptz,
    parity_checked_at timestamptz,
    tenant_int        integer     not null,
    passes            integer     not null default 0,
    parity_mismatches integer,
    status            text        not null default 'running'
        check (status in ('running', 'done')),
    last_error        text,
    primary key (tenant_int, table_int)
);
