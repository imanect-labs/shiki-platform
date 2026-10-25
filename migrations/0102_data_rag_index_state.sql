-- 構造化データの RAG 増分索引（Task 15.8 のスパイク）で、**全件の反映が終わった世代**を記録する台帳。
--
-- 索引の世代の切替（初回・field_policy / 投影の変更）は、sink の全チャンクを破棄して全件を投影し直す。
-- sink は消費 Tx の外の索引なので、sink 側の世代だけで「作り直し済み」を判定すると、作り直しの途中で
-- 失敗・停止したときに、再配信で世代一致の増分経路へ入り、空・途中までの索引が完成扱いになる
-- （AI レビュー C2）。この表は作り直しが最後のページまで成功した後に、outbox の配送記録と同じ Tx で
-- 書く。途中で失敗すれば Tx ごと巻き戻るので、次の周期は世代不一致として作り直し直す。
-- 台帳 consumer ごとに持つ（consumer = sink の系統。別の系統の完了で自分の索引を完成扱いにしない）。
-- table_int は内部キー（外へ出さない）。テナント消去で消す（crates/data/src/store.rs の purge 対象）。
create table data_rag_index_state (
    tenant_int integer     not null,
    table_int  bigint      not null,
    consumer   text        not null,
    generation text        not null,
    updated_at timestamptz not null default now(),
    primary key (tenant_int, table_int, consumer)
);
