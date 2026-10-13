//! 利用量の集計（管理画面の利用量ダッシュボード・Task 12.2 の一部）。
//!
//! 集計元は会計（`llm_usage`）そのもの＝課金と同じ数字を出す（別に数え直さない）。
//! 範囲は呼び出し元の **tenant_id ＋ org** に必ず閉じる（`llm_usage_tenant_idx` に乗る）。
//! 日付の区切りは日本時間（Asia/Tokyo）。

use authz::AuthContext;
use chrono::NaiveDate;
use sqlx::PgPool;

/// 集計の絞り込み。`from` 〜 `to`（両端含む・日本時間の日付）。
#[derive(Debug, Clone)]
pub struct UsageFilter {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub model: Option<String>,
    pub source: Option<String>,
}

/// 期間全体の合計。
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct UsageTotals {
    pub cost_usd_micros: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub calls: i64,
    pub users: i64,
}

/// 日別の 1 行。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UsageDay {
    pub day: NaiveDate,
    pub cost_usd_micros: i64,
    pub tokens: i64,
    pub calls: i64,
}

/// 内訳の 1 行（モデル・ユーザー・利用元のいずれか）。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UsageBreakdown {
    pub key: String,
    pub cost_usd_micros: i64,
    pub tokens: i64,
    pub calls: i64,
}

/// 利用量ダッシュボード 1 画面分。
#[derive(Debug, Clone, Default)]
pub struct UsageReport {
    pub totals: UsageTotals,
    pub daily: Vec<UsageDay>,
    pub by_model: Vec<UsageBreakdown>,
    pub by_user: Vec<UsageBreakdown>,
    pub by_source: Vec<UsageBreakdown>,
}

/// 内訳の軸（SQL に埋める列名は閉じた集合からだけ選ぶ）。
#[derive(Debug, Clone, Copy)]
enum Dimension {
    Model,
    User,
    Source,
}

impl Dimension {
    const fn column(self) -> &'static str {
        match self {
            Dimension::Model => "model",
            Dimension::User => "COALESCE(user_sub, '')",
            Dimension::Source => "source",
        }
    }
}

/// 共通の WHERE（テナント・org・期間・任意の絞り込み）。プレースホルダは $1..$6 固定。
const SCOPE: &str = "tenant_id = $1 AND org = $2 \
     AND created_at >= ($3::date)::timestamp AT TIME ZONE 'Asia/Tokyo' \
     AND created_at < (($4::date) + 1)::timestamp AT TIME ZONE 'Asia/Tokyo' \
     AND ($5::text IS NULL OR model = $5) \
     AND ($6::text IS NULL OR source = $6)";

/// 内訳の上位件数（ユーザー別は人数が多いので上位だけ返す）。
const BREAKDOWN_LIMIT: i64 = 20;

/// 利用量の集計器。
#[derive(Clone)]
pub struct UsageReporter {
    db: PgPool,
}

impl UsageReporter {
    pub fn new(db: PgPool) -> Self {
        UsageReporter { db }
    }

    /// ダッシュボード 1 画面分をまとめて集計する。
    pub async fn report(
        &self,
        ctx: &AuthContext,
        f: &UsageFilter,
    ) -> Result<UsageReport, sqlx::Error> {
        let totals: UsageTotals = sqlx::query_as(&format!(
            "SELECT COALESCE(SUM(cost_usd_micros), 0)::bigint AS cost_usd_micros, \
                    COALESCE(SUM(prompt_tokens), 0)::bigint AS prompt_tokens, \
                    COALESCE(SUM(completion_tokens), 0)::bigint AS completion_tokens, \
                    COUNT(*)::bigint AS calls, \
                    COUNT(DISTINCT user_sub)::bigint AS users \
             FROM llm_usage WHERE {SCOPE}"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(f.from)
        .bind(f.to)
        .bind(f.model.as_deref())
        .bind(f.source.as_deref())
        .fetch_one(&self.db)
        .await?;

        let daily: Vec<UsageDay> = sqlx::query_as(&format!(
            "SELECT (created_at AT TIME ZONE 'Asia/Tokyo')::date AS day, \
                    COALESCE(SUM(cost_usd_micros), 0)::bigint AS cost_usd_micros, \
                    COALESCE(SUM(prompt_tokens + completion_tokens), 0)::bigint AS tokens, \
                    COUNT(*)::bigint AS calls \
             FROM llm_usage WHERE {SCOPE} GROUP BY 1 ORDER BY 1"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(f.from)
        .bind(f.to)
        .bind(f.model.as_deref())
        .bind(f.source.as_deref())
        .fetch_all(&self.db)
        .await?;

        Ok(UsageReport {
            totals,
            daily,
            by_model: self.breakdown(ctx, f, Dimension::Model).await?,
            by_user: self.breakdown(ctx, f, Dimension::User).await?,
            by_source: self.breakdown(ctx, f, Dimension::Source).await?,
        })
    }

    async fn breakdown(
        &self,
        ctx: &AuthContext,
        f: &UsageFilter,
        dim: Dimension,
    ) -> Result<Vec<UsageBreakdown>, sqlx::Error> {
        let col = dim.column();
        sqlx::query_as(&format!(
            "SELECT {col} AS key, \
                    COALESCE(SUM(cost_usd_micros), 0)::bigint AS cost_usd_micros, \
                    COALESCE(SUM(prompt_tokens + completion_tokens), 0)::bigint AS tokens, \
                    COUNT(*)::bigint AS calls \
             FROM llm_usage WHERE {SCOPE} \
             GROUP BY 1 ORDER BY cost_usd_micros DESC, tokens DESC, key LIMIT $7"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(f.from)
        .bind(f.to)
        .bind(f.model.as_deref())
        .bind(f.source.as_deref())
        .bind(BREAKDOWN_LIMIT)
        .fetch_all(&self.db)
        .await
    }
}
