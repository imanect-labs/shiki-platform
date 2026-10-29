//! 利用制限（月次コスト上限・プレビュー環境向け・#757）。
//!
//! 集計元は会計（`llm_usage`）そのもの＝課金と同じ数字で判定する（別に数え直さない）。
//! 月の区切りは利用量ダッシュボード（[`usage_report`](crate::usage_report)）と揃えて
//! **日本時間の暦月**にする。
//!
//! 二段で守る（設定は [`UsageLimitsConfig`]）:
//! 1. **ユーザー単位**（`tenant_id` ＋ `user_sub`・全プロバイダ合算）。1 人の使い過ぎで
//!    全員が止まるのを防ぐ。
//! 2. **プロバイダ別の環境総枠**（`provider_id`・**テナント横断**）。API キーの請求額
//!    そのものを守る。キーの請求はテナントで分かれないので、ここだけは `tenant_id` に
//!    閉じない — 返すのは「枠を使い切ったか」の真偽だけで、他テナントの利用内容は
//!    呼び出し元へ出さない（アンビエント権限にはならない）。
//!
//! 判定は**生成を始める前**に行う。上限は「ここを超えたら次から止める」線であって厳密な
//! 天井ではない（ミニアプリの日次予算 `app_spend_today` と同じ性質）。超える量は 2 通り:
//!
//! - **1 回の生成の中で跨ぐ分**。判定の後に生成が走るので、その 1 回分は止まらない。
//! - **同時に走る生成の分**。判定は読み取りだけで、会計への加算はストリームを読み切った後の
//!   [`record_generation`](crate::gateway::LlmGateway::record_generation) が行う。同じユーザーが
//!   複数のスレッドで同時に始めると、どの加算よりも前に全部の判定が通る。超過は
//!   「同時実行数 × 1 回の生成のコスト」まで膨らみ得る。
//!
//! 厳密に守るなら、生成前に見積額を予約行として書き、確定時に実額で置き換える（判定は
//! ユーザー・枠ごとの advisory lock を取った 1 トランザクションの中で行う）必要がある。
//! **プレビューの安全弁にはそこまで要らない**（枠は月額で、1 人が同時に開けるスレッド数は
//! たかが知れている）ため、ここでは読み取り判定に留める。
//!
//! 集計に失敗したときは**生成しない**（fail-closed）。開けてしまうと請求を守れない。

use authz::AuthContext;
use sqlx::PgPool;

use crate::config::{ProviderLimit, UsageLimitsConfig};

/// 日本時間の「今月 1 日 00:00」（timestamptz）。`created_at` の範囲に使う。
const MONTH_START: &str = "(date_trunc('month', now() AT TIME ZONE 'Asia/Tokyo')) \
     AT TIME ZONE 'Asia/Tokyo'";
/// 日本時間の「来月 1 日 00:00」（timestamptz）。上限がリセットされる時刻。
const NEXT_MONTH_START: &str = "(date_trunc('month', now() AT TIME ZONE 'Asia/Tokyo') \
     + interval '1 month') AT TIME ZONE 'Asia/Tokyo'";

/// 呼び出しユーザーの当月（日本時間）の利用額（本人向けの表示・#965）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthUsage {
    /// 当月の合計コスト（マイクロ USD）。上限の判定と同じ集計（tenant ＋ 本人・全プロバイダ）。
    pub used_usd_micros: i64,
    /// ユーザー単位の上限（マイクロ USD）。無効・未設定なら `None`。
    pub limit_usd_micros: Option<i64>,
    /// 集計期間の始まり（日本時間の今月 1 日 00:00）。
    pub period_start: chrono::DateTime<chrono::Utc>,
    /// 集計期間の終わり（日本時間の来月 1 日 00:00・この時刻を含まない）。
    pub period_end: chrono::DateTime<chrono::Utc>,
}

/// 制限判定の失敗。
#[derive(Debug)]
pub enum LimitError {
    /// 上限に達している。文言はそのままユーザーへ出せるものにする。
    Exceeded(String),
    /// 集計に失敗した（fail-closed で生成を止める）。
    Query(sqlx::Error),
}

/// 利用制限の判定器（会計テーブルの月次集計）。
#[derive(Clone)]
pub struct UsageLimiter {
    db: PgPool,
}

impl UsageLimiter {
    pub fn new(db: PgPool) -> Self {
        UsageLimiter { db }
    }

    /// 上限に達していないか確かめる。達していれば [`LimitError::Exceeded`]。
    ///
    /// `provider_id` はデプロイ設定のプロバイダ ID、`provider_label` はその表示名
    /// （文言に出すのは表示名だけ。ID は出さない）。
    pub async fn check(
        &self,
        ctx: &AuthContext,
        limits: &UsageLimitsConfig,
        provider_id: &str,
        provider_label: &str,
    ) -> Result<(), LimitError> {
        if limits.is_noop() {
            return Ok(());
        }
        let per_user = limits.per_user();
        let per_provider = limits.provider_limit(provider_id);
        // 互いに独立な集計なので並べて流す（生成前の待ちを 1 往復に収める）。
        let (user_spent, provider_spent) = tokio::try_join!(
            self.user_spend_this_month(ctx, per_user),
            self.provider_spend_this_month(per_provider),
        )
        .map_err(LimitError::Query)?;

        if let (Some(limit), Some(spent)) = (per_user, user_spent) {
            if spent >= limit {
                return Err(LimitError::Exceeded(user_limit_message(limit)));
            }
        }
        if let (Some(limit), Some(spent)) = (per_provider, provider_spent) {
            if spent >= limit.usd_micros {
                return Err(LimitError::Exceeded(provider_limit_message(
                    provider_label,
                    limit.usd_micros,
                )));
            }
        }
        Ok(())
    }

    /// 呼び出しユーザーの当月（日本時間）の利用額と上限（本人向けの表示）。
    ///
    /// 上限の判定（[`Self::check`]）と同じ集計なので、表示と止まる線がずれない。上限が無くても
    /// 利用額は返す（表示側が出すかを決める）。`(tenant_id, user_sub, created_at)` の索引に乗る。
    pub async fn month_usage(
        &self,
        ctx: &AuthContext,
        limits: &UsageLimitsConfig,
    ) -> Result<MonthUsage, sqlx::Error> {
        let (used, start, end): (
            i64,
            chrono::DateTime<chrono::Utc>,
            chrono::DateTime<chrono::Utc>,
        ) = sqlx::query_as(&format!(
            "SELECT COALESCE(SUM(cost_usd_micros), 0)::bigint, {MONTH_START}, {NEXT_MONTH_START} \
                 FROM llm_usage \
                 WHERE tenant_id = $1 AND user_sub = $2 AND created_at >= {MONTH_START}"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .fetch_one(&self.db)
        .await?;
        Ok(MonthUsage {
            used_usd_micros: used,
            limit_usd_micros: limits.per_user(),
            period_start: start,
            period_end: end,
        })
    }

    /// 呼び出しユーザーの当月（日本時間）の合計コスト。`limit` が無ければ問い合わせない。
    async fn user_spend_this_month(
        &self,
        ctx: &AuthContext,
        limit: Option<i64>,
    ) -> Result<Option<i64>, sqlx::Error> {
        if limit.is_none() {
            return Ok(None);
        }
        let (sum,): (i64,) = sqlx::query_as(&format!(
            "SELECT COALESCE(SUM(cost_usd_micros), 0)::bigint FROM llm_usage \
             WHERE tenant_id = $1 AND user_sub = $2 AND created_at >= {MONTH_START}"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .fetch_one(&self.db)
        .await?;
        Ok(Some(sum))
    }

    /// 枠を共有するプロバイダ群の当月（日本時間）の合計コスト（**テナント横断**）。
    ///
    /// 枠は API キー単位なので、1 つのキーを共有する複数のプロバイダ ID を合算する。
    async fn provider_spend_this_month(
        &self,
        limit: Option<&ProviderLimit>,
    ) -> Result<Option<i64>, sqlx::Error> {
        let Some(limit) = limit else {
            return Ok(None);
        };
        let (sum,): (i64,) = sqlx::query_as(&format!(
            "SELECT COALESCE(SUM(cost_usd_micros), 0)::bigint FROM llm_usage \
             WHERE provider_id = ANY($1) AND created_at >= {MONTH_START}"
        ))
        .bind(&limit.providers)
        .fetch_one(&self.db)
        .await?;
        Ok(Some(sum))
    }
}

/// マイクロ USD を `$12.34` 表記にする（セント丸め・金額を 0 と誤認させない）。
#[must_use]
pub fn format_usd(micros: i64) -> String {
    let cents = (micros.max(0) + 5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// ユーザー単位の上限に達したときの文言。
#[must_use]
pub fn user_limit_message(limit: i64) -> String {
    format!(
        "プレビュー環境の今月の利用上限（{}）に達しました。翌月 1 日にリセットされます。",
        format_usd(limit)
    )
}

/// プロバイダ別の環境総枠を使い切ったときの文言。
#[must_use]
pub fn provider_limit_message(provider_label: &str, limit: i64) -> String {
    format!(
        "プレビュー環境の {provider_label} の今月の利用枠（{}）を使い切りました。\
         別の提供元のモデルをお試しください。",
        format_usd(limit)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_is_noop() {
        let mut limits = UsageLimitsConfig {
            enabled: false,
            per_user_usd_micros: Some(1_000_000),
            providers: vec![ProviderLimit {
                providers: vec!["anthropic".into()],
                usd_micros: 50_000_000,
            }],
        };
        // enabled=false は数値が入っていても一切効かない（問い合わせもしない）。
        assert!(limits.is_noop());
        assert_eq!(limits.per_user(), None);
        assert!(limits.provider_limit("anthropic").is_none());

        limits.enabled = true;
        assert!(!limits.is_noop());
        assert_eq!(limits.per_user(), Some(1_000_000));
        assert_eq!(
            limits.provider_limit("anthropic").map(|p| p.usd_micros),
            Some(50_000_000)
        );
        // 列挙していないプロバイダは無制限。
        assert!(limits.provider_limit("opencode").is_none());
    }

    #[test]
    fn enabled_without_numbers_is_noop() {
        let limits = UsageLimitsConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(limits.is_noop());
    }

    #[test]
    fn usd_is_formatted_to_cents() {
        assert_eq!(format_usd(0), "$0.00");
        assert_eq!(format_usd(10_000), "$0.01");
        assert_eq!(format_usd(1_500_000), "$1.50");
        assert_eq!(format_usd(50_000_000), "$50.00");
        // 端数はセントへ丸める（$1.235 → $1.24）。
        assert_eq!(format_usd(1_235_000), "$1.24");
    }

    #[test]
    fn messages_name_the_limit_and_not_the_provider_id() {
        let msg = user_limit_message(1_500_000);
        assert!(msg.contains("$1.50"), "{msg}");

        let msg = provider_limit_message("Anthropic", 50_000_000);
        assert!(msg.contains("Anthropic"), "{msg}");
        assert!(msg.contains("$50.00"), "{msg}");
    }
}
