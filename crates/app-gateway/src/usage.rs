//! 能力利用量の計上（Task 9.8・`app_capability_usage`）。
//!
//! (テナント×アプリ×ユーザー×能力×日) で成功呼び出し回数を upsert する。B2 の自動実行（アプリの主体）の分は、
//! 利用者と分けて `principal = 'app'`・`user_sub` を空で記録する（BUG-204）。呼び出し元は
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
    /// 呼んだ利用者（`principal` が `app` の行は空・BUG-204）。
    pub user_sub: String,
    /// 呼んだ主体の種類（`user` は利用者・`app` は B2 の自動実行のアプリの主体・BUG-204・migration 0147）。
    pub principal: String,
    pub capability: String,
    pub day: NaiveDate,
    pub calls: i64,
}

/// 能力を呼んだ主体（利用量の行を分ける・BUG-204）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsagePrincipal<'a> {
    /// 利用者（B1 の PKCE・B2 が交換したトークン）。値は利用者の id（トークンの `sub`）。
    User(&'a str),
    /// B2 の自動実行のアプリの主体（サービスアカウントの id は記録しない）。
    App,
}

impl UsagePrincipal<'_> {
    /// 記録する `(principal, user_sub)`。
    fn columns(self) -> (&'static str, String) {
        match self {
            UsagePrincipal::User(sub) => ("user", sub.to_string()),
            UsagePrincipal::App => ("app", String::new()),
        }
    }
}

/// 成功呼び出し 1 回を計上する（day はサーバ時刻の CURRENT_DATE）。
pub(crate) async fn record_usage(
    db: &PgPool,
    tenant_id: &str,
    org: &str,
    app_id: Uuid,
    principal: UsagePrincipal<'_>,
    capability: CapabilityScope,
) -> Result<(), GatewayError> {
    let (kind, user_sub) = principal.columns();
    sqlx::query(
        "INSERT INTO app_capability_usage \
             (tenant_id, org, app_id, user_sub, principal, capability, day) \
         VALUES ($1, $2, $3, $4, $5, $6, CURRENT_DATE) \
         ON CONFLICT (tenant_id, app_id, user_sub, capability, day) \
         DO UPDATE SET calls = app_capability_usage.calls + 1, \
                       org = EXCLUDED.org, updated_at = now()",
    )
    .bind(tenant_id)
    .bind(org)
    .bind(app_id)
    .bind(user_sub)
    .bind(kind)
    .bind(capability.as_str())
    .execute(db)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// アプリの利用量を取得する（新しい日から・利用量 API と IT 検証用）。
///
/// テナントと組織の両方で絞る（組織はテナントの中の隔離の境界・PIT-45 の単一定義・BUG-197）。記録の組織は
/// インストールの組織（ゲートウェイが決める・D-130）。
pub async fn fetch_usage(
    db: &PgPool,
    tenant_id: &str,
    org: &str,
    app_id: Uuid,
    limit: i64,
) -> Result<Vec<CapabilityUsage>, GatewayError> {
    let rows = sqlx::query_as(
        "SELECT app_id, user_sub, principal, capability, day, calls \
         FROM app_capability_usage \
         WHERE tenant_id = $1 AND org = $2 AND app_id = $3 \
         ORDER BY day DESC, capability, user_sub LIMIT $4",
    )
    .bind(tenant_id)
    .bind(org)
    .bind(app_id)
    .bind(limit.clamp(1, 1000))
    .fetch_all(db)
    .await
    .map_err(map_db)?;
    Ok(rows)
}
