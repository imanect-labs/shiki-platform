-- no-transaction
-- issue #282: ノート側の会話一覧引き index を **CONCURRENTLY** で作る（thread への書込を止めない）。
--
-- sqlx 0.8 は先頭 `-- no-transaction` で migration を transaction 外実行する。CREATE INDEX
-- CONCURRENTLY は transaction ブロック内で実行できないため、この migration は index 1 文のみに保つ
-- （0047 の ALTER TABLE とは分割）。テナント内・当該ノート・未削除・更新日降順で会話一覧を引く。
create index concurrently if not exists thread_origin_note_idx
    on thread (tenant_id, org, origin_note_id, updated_at desc)
    where origin_note_id is not null and deleted_at is null;
