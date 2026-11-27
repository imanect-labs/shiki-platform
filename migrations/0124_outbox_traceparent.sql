-- ファイル更新 → storage_event_outbox → relay → rag_ingest の経路で、書込んだリクエストの trace
-- context（W3C traceparent）をインジェストのジョブまで運ぶ（crates/jobq の trace、0123 の続き）。
--
-- relay は別プロセスのループで動くので、enqueue の時点の span は書込んだリクエストではない。
-- trace_id 列だけでは親 span が分からないため、traceparent 一式を outbox に残す。
-- 既存行・旧版が書いた行は null（relay はリンク無しで積むだけで落ちない）。

alter table storage_event_outbox add column traceparent text;
