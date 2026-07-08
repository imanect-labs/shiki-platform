-- Phase 5 W4: 生成 run に trace_id を保持する（Task 5.9・可観測化）。
--
-- これまで agent_mode 生成は trace_id を伝播しておらず（RunContext.trace_id=None）、Langfuse トレースと
-- OTel/監査が相関しなかった。post_message で受けた trace_id を run に永続化し、ワーカーが RunContext へ
-- 伝播することで「1 自律セッション = 1 トレース」（多段スパン）を trace_id で突合できるようにする。
alter table generation_run add column if not exists trace_id text;
