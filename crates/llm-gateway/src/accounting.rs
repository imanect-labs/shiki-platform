//! トークン会計（`llm_usage` テーブル）。SAAS.3 課金の集計元・金額クリティカル（PIT-28）。
//!
//! tenant_id + org を必須カラムとし、冪等キーで二重計上を不能にする（同一 attempt の再送で
//! 重複行を作らない・`unique (tenant_id, idempotency_key)` ＋ `ON CONFLICT DO NOTHING`）。
//! コストは float を使わず整数マイクロ USD。
//! Task 9.9: (ユーザー×アプリ) の集計軸として `user_sub`（常に AuthContext から）と
//! `app_id`（ミニアプリ経由のみ）を記録する。

use authz::AuthContext;
use sqlx::PgPool;
use uuid::Uuid;

use crate::model::Usage;

/// 1 回の LLM 呼び出しの会計レコード。
#[derive(Debug, Clone)]
pub struct UsageRecord {
    /// 冪等キー（例 `<run_id>:<attempt>:<call_ordinal>`）。テナント内一意。
    pub idempotency_key: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    /// 実コスト（マイクロ USD・整数）。
    pub cost_usd_micros: i64,
    pub trace_id: Option<String>,
    /// 呼び出し元ミニアプリ（ゲートウェイ AI 能力・Task 9.9）。chat 等は `None`。
    pub app_id: Option<Uuid>,
}

/// 会計レコーダ（`llm_usage` への冪等追記）。
#[derive(Clone)]
pub struct UsageRecorder {
    db: PgPool,
}

impl UsageRecorder {
    pub fn new(db: PgPool) -> Self {
        UsageRecorder { db }
    }

    /// 1 件記録する（冪等）。同一 `(tenant_id, idempotency_key)` の再送は no-op。
    pub async fn record(&self, ctx: &AuthContext, rec: &UsageRecord) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO llm_usage \
             (tenant_id, org, idempotency_key, provider, model, prompt_tokens, completion_tokens, \
              cost_usd_micros, trace_id, app_id, user_sub) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (tenant_id, idempotency_key) DO NOTHING",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&rec.idempotency_key)
        .bind(&rec.provider)
        .bind(&rec.model)
        .bind(rec.usage.prompt_tokens as i64)
        .bind(rec.usage.completion_tokens as i64)
        .bind(rec.cost_usd_micros)
        .bind(rec.trace_id.as_deref())
        .bind(rec.app_id)
        .bind(&ctx.principal.id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// アプリの当日（サーバ日付・UTC）の合計コスト（マイクロ USD）を返す。
    ///
    /// ゲートウェイ AI 能力の日次予算チェック用（Task 9.9）。テナント全体で app 単位に集計する
    /// （ユーザー横断＝アプリの予算はテナント内で共有）。
    pub async fn app_spend_today(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<i64, sqlx::Error> {
        let (sum,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(cost_usd_micros), 0)::bigint FROM llm_usage \
             WHERE tenant_id = $1 AND app_id = $2 AND created_at >= date_trunc('day', now())",
        )
        .bind(&ctx.tenant_id)
        .bind(app_id)
        .fetch_one(&self.db)
        .await?;
        Ok(sum)
    }
}
