-- 一括取込に「取込を始めた run」を保存する（run の書込の印・BUG-44）。
--
-- ワークフローの run の中の書込は、outbox の consumer 非公開部に由来の run の印を載せ、ワークフローの relay が
-- 因果の鎖と by_workflow をそこから引く（書込の trace_id から run を推しはからない）。data.bulk_upsert の続きは
-- ジョブのワーカーが取込の主体から文脈を組み直して進めるので、run を保存しないと、ワーカーが進めた分の変更に
-- 印が載らない。NULL は run の外の取込（利用者の UI / API・従来どおり）。
alter table data_bulk_import add column origin_run_id uuid;
