-- 一括取込に「辿ってよいテーブルの天井」（data::TableReach）を保存する（AI レビュー C3 の続き）。
--
-- ワークフローの data.bulk_upsert は委譲の天井つきのストアで取込を始めるが、続きはジョブのワーカーが
-- 天井の無いストアで進める。天井を保存しないと、ワーカーが進めた分の検証（リンクの参照先の存在と
-- 書き手からの可視性）が委譲元の権限を丸ごと使い、未委譲テーブルのレコードの有無が行エラーから分かる。
-- 形: {"tables": [uuid, ...], "co": {"kind": "workflow", "id": "..."} | null}。NULL は天井なし
-- （ユーザー本人の UI / API からの取込・従来どおり）。
alter table data_bulk_import add column reach jsonb;
