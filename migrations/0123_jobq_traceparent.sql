-- ジョブの実行を enqueue 元のトレースにつなげる（crates/jobq の trace モジュール）。
--
-- 既存の trace_id 列は trace_id（32 桁 hex）だけで、親 span の span_id とフラグが無いため、
-- ワーカーが claim 後に「enqueue 元の span の子」や「enqueue 元へのリンク」を張れない。
-- W3C の traceparent（`00-<trace_id>-<span_id>-<flags>`）一式を別列に保存する。
--
-- trace_id 列は監査ログ・Langfuse との突合に使われているので意味を変えない（別列にする理由）。
-- 既存行・旧版が投入した行は null のまま（ワーカーは親無しの span を作るだけで落ちない）。
-- DLQ にも引き継ぎ、requeue_dead で元のトレースへ戻せるようにする。

alter table job_queue add column traceparent text;
alter table job_queue_dead add column traceparent text;
