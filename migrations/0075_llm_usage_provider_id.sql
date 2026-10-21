-- プレビュー環境の利用制限（月次コスト上限・二段・#757）が数える軸を会計へ足す。
--
-- 既存の `provider` 列はアダプタ**種別**（openai / openai_responses / anthropic / stub）で、
-- 同じ種別の追加プロバイダを区別できない（プレビューでは OpenRouter・OpenCode Go・
-- Google AI Studio・Anthropic のうち 3 つが種別 openai 系）。金額クリティカル（PIT-28）な
-- 既存列の意味は変えず、デプロイ設定の `llm.providers[].id` を刻む列を足す。
--
-- 既存行は NULL（どのプロバイダ設定で呼んだか遡って決められない）。プロバイダ別の総枠は
-- NULL 行を数えないので、制限はこの migration 以後の消費から効く。
alter table llm_usage add column provider_id text;

-- ユーザー別の月次集計（制限の第 1 段）。tenant_id に閉じ、user_sub で絞る。
create index idx_llm_usage_user_month
    on llm_usage (tenant_id, user_sub, created_at desc)
    where user_sub is not null;

-- プロバイダ別の月次集計（制限の第 2 段＝環境総枠）。**テナント横断で数える**
-- （API キーの請求はテナントで分かれないため・llm-gateway の limits.rs 参照）。
create index idx_llm_usage_provider_month
    on llm_usage (provider_id, created_at desc)
    where provider_id is not null;
