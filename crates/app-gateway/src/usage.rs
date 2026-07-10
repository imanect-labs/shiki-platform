//! 能力利用量の計上（Task 9.8・`app_capability_usage`）。
//!
//! (テナント×アプリ×ユーザー×能力×日) で成功呼び出し回数を upsert する。呼び出し元は
//! 二重ゲート middleware（成功応答時のみ・ハンドラに散らさない＝単一チョークポイント）。
//! 監査の正は `audit_log`（全許可/拒否）で、こちらはコスト按分・利用量 API（PR12）向けの集計。

use authz::CapabilityScope;
use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{map_db, GatewayError};

/// 利用量 1 行（利用量 API・テスト検証用）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CapabilityUsage {
    pub app_id: Uuid,
    pub user_sub: String,
    pub capability: String,
    pub day: NaiveDate,
    pub calls: i64,
}

/// 成功呼び出し 1 回を計上する（day はサーバ時刻の CURRENT_DATE）。
pub(crate) async fn record_usage(
    db: &PgPool,
    tenant_id: &str,
    org: &str,
    app_id: Uuid,
    user_sub: &str,
    capability: CapabilityScope,
) -> Result<(), GatewayError> {
    sqlx::query(
        "INSERT INTO app_capability_usage \
             (tenant_id, org, app_id, user_sub, capability, day) \
         VALUES ($1, $2, $3, $4, $5, CURRENT_DATE) \
         ON CONFLICT (tenant_id, app_id, user_sub, capability, day) \
         DO UPDATE SET calls = app_capability_usage.calls + 1, updated_at = now()",
    )
    .bind(tenant_id)
    .bind(org)
    .bind(app_id)
    .bind(user_sub)
    .bind(capability.as_str())
    .execute(db)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// アプリの利用量を取得する（新しい日から・利用量 API と IT 検証用）。
pub async fn fetch_usage(
    db: &PgPool,
    tenant_id: &str,
    app_id: Uuid,
    limit: i64,
) -> Result<Vec<CapabilityUsage>, GatewayError> {
    let rows = sqlx::query_as(
        "SELECT app_id, user_sub, capability, day, calls \
         FROM app_capability_usage \
         WHERE tenant_id = $1 AND app_id = $2 \
         ORDER BY day DESC, capability, user_sub LIMIT $3",
    )
    .bind(tenant_id)
    .bind(app_id)
    .bind(limit.clamp(1, 1000))
    .fetch_all(db)
    .await
    .map_err(map_db)?;
    Ok(rows)
}
