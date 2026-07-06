-- Phase 3: llm-gateway トークン会計（Task 3.2 / 3.8）。
--
-- llm-gateway（in-process チョークポイント）が LLM 呼び出しごとに実消費を記録する。
-- SAAS.3 課金の集計元であり金額クリティカル（PIT-28）ゆえ、tenant_id + org を必須カラムとし
-- 冪等キーで二重計上を不能にする（同一 attempt の再送で重複行を作らない）。
-- 表示は run 単位に集約するが、記録は attempt/呼び出し単位（design §4.5）。
--
-- コストは float を使わず整数マイクロ USD（cost_usd_micros）で持つ（丸め/累積誤差の回避）。
-- 単価はテナントのモデルカタログ由来（プロンプト/補完の別単価）。

create table llm_usage (
    id                bigserial   primary key,
    tenant_id         text        not null,
    org               text        not null,
    -- 冪等キー（例: `<run_id>:<attempt>:<call_ordinal>`）。同一呼び出しの再記録を潰す。
    idempotency_key   text        not null,
    provider          text        not null,   -- openai / anthropic / stub / ...
    model             text        not null,
    prompt_tokens     bigint      not null default 0,
    completion_tokens bigint      not null default 0,
    -- 実コスト（マイクロ USD）。単価×トークンをアプリで算出して刻む。
    cost_usd_micros   bigint      not null default 0,
    -- OTel / Langfuse / 監査ログと突合するトレース ID。
    trace_id          text,
    created_at        timestamptz not null default now(),
    -- テナント内で冪等キー一意（二重計上防止・バイパス不能）。
    unique (tenant_id, idempotency_key)
);

-- テナント別の使用量集計（課金・クォータ）。
create index llm_usage_tenant_idx on llm_usage (tenant_id, org, created_at desc);
