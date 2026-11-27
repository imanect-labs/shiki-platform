-- テーブルの一覧を、DB の候補から並びの順に引けるようにする（BUG-149）。
--
-- テーブルの一覧と名前の検索（`GET /data/tables`）は、OpenFGA の ListObjects の応答が上限（1,000 件）に届いたら、
-- テナント・組織の生きているテーブルを更新の新しい順（`updated_at DESC, id DESC`）に少しずつ引いて OpenFGA で
-- 絞る（crates/data/src/table_list.rs）。続きの位置は `(updated_at, id)` のキーセットなので、その並びの索引を足す。

create index if not exists data_table_list_idx
    on data_table (tenant_id, org, updated_at, id)
    where deleted_at is null;
