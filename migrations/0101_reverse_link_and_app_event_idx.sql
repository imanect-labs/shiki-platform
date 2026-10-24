-- 独立レビュー（データ書込・配信）の確定指摘 R2-A・R2-C・R2-E: 書込 Tx・リレー・ライブテール・再計算待ちが
-- 全件を走査（並べ替え）していた 3 つの読み方に索引を足す。
--
-- ① 片方向リンク（対称列なし）の参照元の逆引き。参照先のレコードが変わったとき、材料化した派生列の
--    再計算待ち（crates/data/src/derived/materialize.rs）と派生値の再配信（crates/data/src/stream/relay.rs）は
--    「参照元のパーティション（table_int）の中で、変わった参照先を指す辺」を
--    `tenant_int = ? AND table_int = ? AND field_id = ? AND dst_table_int = ? AND dst_record = ANY(?)` で引く。
--    主キー（…, src_record, ord）と data_link_single_idx では dst_record で絞れず、そのリンク列の全行を
--    書込 Tx の中で読んでいた。dst_table_int と src_record を INCLUDE して索引だけで答える。
--    リンク値での絞り込み（query/planner/lower.rs の `dst_record = ANY`）も同じ形で使える。
--    パーティション表の親に作る（各パーティションへ伝播する）。
create index data_link_dst_idx
    on data_link (tenant_int, table_int, field_id, dst_record)
    include (dst_table_int, src_record);

-- ② アプリ向けドメインイベントのライブテール（app-gateway の events.subscribe → storage::event::
--    peek_app_events_after）。取込が 1 行ごとに積むレコード変更（data.record.changed）はライブテールに
--    流さないので SQL 側で除く。除いた残りだけを (tenant_id, id) の順に持つ部分索引で、変更が何万件
--    積まれていても読み飛ばさずに次のアプリ向けイベントへ届く。述語はクエリと同じ形で書く。
create index storage_event_outbox_app_event_idx
    on storage_event_outbox (tenant_id, id)
    where payload ? 'event_type' and payload ->> 'event_type' <> 'data.record.changed';

-- ③ 材料化した派生列の再計算待ち（crates/data/src/derived/materialize.rs の DERIVED_PENDING_STEP_SQL）。
--    1 行の再計算は材料化した全列をまとめて計算し直すので、待ちは行の順（record_int, field_id）に読む。
--    主キー（…, field_id, record_int）とは並びが逆なので、行の順の索引を足す（毎回の全待ち行の並べ替えをしない）。
create index data_derived_pending_row_idx
    on data_derived_pending (tenant_int, table_int, record_int, field_id);
