-- 引用の位置情報（issue #508）。
--
-- 引用は「版ごとの正規化ブロック列（doc_block）の中の範囲」で指す。原本（docx の XML・md の
-- バイト列）の位置には戻さない。出典パネルはこのブロック列を描くので、形式に関係なく
-- 引用箇所をハイライトできる。
--
--   * doc_block は**版ごとに残す**。rag_chunk は従来どおり最新版だけ（検索対象を絞る）で、
--     古い版を引用した会話は doc_block と Citation 自身が持つ一節（quote）で解決する。
--     ノードの削除・テナント消去で一緒に消す。
--   * オフセットは UTF-16 コード単位（読むのはブラウザだけ。JS の文字列と同じ数え方）。
--   * 大きな文書でも「引用箇所の前後 N ブロック」だけを主キーの範囲で引く（全件取得しない）。

create table doc_block (
    tenant_id   text   not null,
    org         text   not null,
    node_id     uuid   not null,
    version     bigint not null,
    -- 版の中のブロック番号（パーサの出力順・0 起点）。アンカーの基準。
    ordinal     int    not null,
    type        text   not null check (type in ('heading', 'paragraph', 'table', 'caption', 'list_item')),
    level       int,
    text        text   not null,
    -- list_item のみ: 見た目の番号・記号。
    list_marker text,
    page        int,
    -- PDF など座標を持つ形式のみ: [{page, bbox: [l, t, r, b], origin}]。
    prov        jsonb  not null default '[]',
    primary key (tenant_id, node_id, version, ordinal)
);

-- rag_chunk → doc_block の範囲（parent と旧インデックスは null）。
alter table rag_chunk
    add column block_start  int,
    add column off_start    int,
    add column block_end    int,
    add column off_end      int,
    -- 元エディタで一節を探すための前後の文脈（TextQuote の prefix / suffix）。
    add column quote_prefix text not null default '',
    add column quote_suffix text not null default '',
    -- PDF の原本上の枠（範囲に含まれるブロックの prov）。
    add column boxes        jsonb not null default '[]';
