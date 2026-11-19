-- LLM プロバイダの稼働状況（管理コンソール「AI > プロバイダ」の状態表示の集計元）。
--
-- llm-gateway が**プロバイダへの試行ごと**（送り直しも 1 回と数える）の結果と、最初のデルタまでの
-- 時間（TTFT）を各インスタンスのメモリで分単位に数え、ここへ加算で upsert する。複数インスタンスでも
-- 足し合わせで揃うので、どのインスタンスから読んでも同じ値になる。
--
-- # tenant_id を持たない理由
--
-- プロバイダはデプロイ（運営側の `llm.providers`）に属し、全テナントで共有する。ここに残るのは
-- プロバイダ ID・分・件数だけで、テナント・利用者・本文・エラー文言は持たない（顧客データではない）。
-- 件数そのものは他テナントの利用規模を映すので、API は率と時間だけを返し、試行の少ない窓・時間では
-- 率も返さない（分母が小さい率は件数そのものになる）。最後の成功の時刻も 15 分単位に丸める。
-- テナント表ではないので RLS（0114）の対象外で、テナント消去の対象にもならない。
--
-- 保持は 7 日（shiki-server の書き込み側が 1 時間ごとに古い行を消す）。

create table llm_provider_health (
    provider      text        not null,   -- llm.providers[].id（既定プロバイダは `default`）
    minute        timestamptz not null,   -- 分の頭（date_trunc('minute', …)）
    -- 最後まで流れた（または流れ始めてから呼び出し側が打ち切った）試行。
    ok            bigint      not null default 0,
    -- プロバイダ側の障害（接続失敗・5xx・タイムアウト・流れ始めた後の切断・応答の解釈不能）。
    failed        bigint      not null default 0,
    -- レート制限・利用枠超過（429）。障害とは分けて数える。
    rate_limited  bigint      not null default 0,
    -- TTFT の度数分布（境界は llm-gateway の `health::TTFT_BOUNDS_MS`・最後の要素は上限超え）。
    ttft_hist     bigint[]    not null default '{}',
    primary key (provider, minute)
);

-- 保持期間の削除（minute < now() - 7 日）と、全プロバイダ横断の時間窓の読み取り用。
create index llm_provider_health_minute_idx on llm_provider_health (minute);
