//! ミニアプリ・インストール台帳（Task 9.6/9.13・二重ゲート第2段の正本）。
//!
//! ゲートウェイは Bearer トークンの azp（登録 client_id）からこの行を引き、`granted_scopes`
//! （同意付与＝requested の部分集合）と所有アプリ（`app_id`）を得る。毎リクエスト突合すること
//! で同意失効（revoked / scope 縮小）が即時反映される（token の scope クレームに依存しない）。
//!
//! 本 PR（9.6）は台帳＋ゲートウェイ参照とデブ用直接作成のみ。実インストール/プロビジョン
//! （単一 Tx＋補償・Keycloak client 登録連動）は Task 9.13b（PR9）が担う。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{map_db, GatewayError};

/// インストールのライフサイクル状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStatus {
    /// 有効（呼び出し可能）。
    Active,
    /// アンインストール済み（token 有効期限内でも 403・即時失効）。
    Revoked,
}

impl InstallStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            InstallStatus::Active => "active",
            InstallStatus::Revoked => "revoked",
        }
    }

    fn parse(s: &str) -> InstallStatus {
        // 未知値は fail-closed で revoked 扱い（有効化に倒さない）。
        match s {
            "active" => InstallStatus::Active,
            _ => InstallStatus::Revoked,
        }
    }
}

/// AI ガードレールの同意時ピン（Task 9.9・PR9 の同意フローがマニフェスト Budget/tools から焼き込む）。
///
/// 実行時は registry/artifact を読まず**この行だけ**が効く＝ユーザーが同意した内容が上限。
#[derive(Debug, Clone, Default, Serialize)]
pub struct AiPin {
    /// 使用可能モデル（空＝テナントカタログ全体を許可）。
    pub budget_models: Vec<String>,
    /// 日次コスト上限（マイクロ USD・None＝管理者キャップのみ）。
    pub budget_daily_usd_micros: Option<i64>,
    /// 1 回の呼び出しの最大トークン。
    pub budget_max_tokens: Option<i64>,
    /// agent.invoke で提示してよい宣言ツール（ToolName 閉集合へ実行時照合）。
    pub agent_tools: Vec<String>,
}

/// インストール台帳の 1 行。
#[derive(Debug, Clone, Serialize)]
pub struct AppInstallation {
    pub id: Uuid,
    pub app_id: Uuid,
    pub app_name: String,
    pub installed_version: String,
    /// 同意で付与されたスコープ（二重ゲートの scope 上限・requested の部分集合）。
    pub granted_scopes: Vec<String>,
    pub client_id_b1: Option<String>,
    pub client_id_b2: Option<String>,
    #[serde(skip)]
    pub status: InstallStatus,
    pub installed_by: String,
    pub created_at: DateTime<Utc>,
    /// AI ガードレールの同意時ピン（Task 9.9）。
    pub ai: AiPin,
}

/// インストール作成の入力（PR9 の同意フロー・本 PR はデブ用 fixture）。
#[derive(Debug, Clone)]
pub struct NewAppInstallation<'a> {
    pub app_id: Uuid,
    pub app_name: &'a str,
    pub installed_version: &'a str,
    pub granted_scopes: &'a [String],
    pub client_id_b1: Option<&'a str>,
    pub client_id_b2: Option<&'a str>,
    /// AI ガードレールの同意時ピン（Task 9.9・既定＝管理者キャップのみ）。
    pub ai: AiPin,
}

/// sqlx 実行時マップ用の生行（`FromRow`）。status は文字列で受けて enum へ写す。
#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    app_id: Uuid,
    app_name: String,
    installed_version: String,
    granted_scopes: Vec<String>,
    client_id_b1: Option<String>,
    client_id_b2: Option<String>,
    status: String,
    installed_by: String,
    created_at: DateTime<Utc>,
    budget_models: Vec<String>,
    budget_daily_usd_micros: Option<i64>,
    budget_max_tokens: Option<i64>,
    agent_tools: Vec<String>,
}

impl From<Row> for AppInstallation {
    fn from(r: Row) -> Self {
        AppInstallation {
            id: r.id,
            app_id: r.app_id,
            app_name: r.app_name,
            installed_version: r.installed_version,
            granted_scopes: r.granted_scopes,
            client_id_b1: r.client_id_b1,
            client_id_b2: r.client_id_b2,
            status: InstallStatus::parse(&r.status),
            installed_by: r.installed_by,
            created_at: r.created_at,
            ai: AiPin {
                budget_models: r.budget_models,
                budget_daily_usd_micros: r.budget_daily_usd_micros,
                budget_max_tokens: r.budget_max_tokens,
                agent_tools: r.agent_tools,
            },
        }
    }
}

/// 全列（RETURNING / SELECT 共通・Row の FromRow と対応）。
const COLS: &str = "id, app_id, app_name, installed_version, granted_scopes, \
                    client_id_b1, client_id_b2, status, installed_by, created_at, \
                    budget_models, budget_daily_usd_micros, budget_max_tokens, agent_tools";

/// インストール台帳ストア（Postgres・tenant スコープ）。
#[derive(Clone)]
pub struct AppInstallationStore {
    db: PgPool,
}

impl AppInstallationStore {
    pub fn new(db: PgPool) -> Self {
        AppInstallationStore { db }
    }

    /// インストールを作成/更新する（tenant 内 1 アプリ 1 行・再インストールは upsert）。
    ///
    /// 本 PR ではデブ用の直接作成。PR9 の同意フローが単一 Tx＋補償で置き換える。
    pub async fn upsert(
        &self,
        ctx: &AuthContext,
        new: NewAppInstallation<'_>,
    ) -> Result<AppInstallation, GatewayError> {
        let sql = format!(
            "INSERT INTO app_installation \
                 (tenant_id, org, app_id, app_name, installed_version, granted_scopes, \
                  client_id_b1, client_id_b2, status, installed_by, \
                  budget_models, budget_daily_usd_micros, budget_max_tokens, agent_tools) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active', $9, $10, $11, $12, $13) \
             ON CONFLICT (tenant_id, app_id) DO UPDATE SET \
                 app_name = EXCLUDED.app_name, \
                 installed_version = EXCLUDED.installed_version, \
                 granted_scopes = EXCLUDED.granted_scopes, \
                 client_id_b1 = EXCLUDED.client_id_b1, \
                 client_id_b2 = EXCLUDED.client_id_b2, \
                 budget_models = EXCLUDED.budget_models, \
                 budget_daily_usd_micros = EXCLUDED.budget_daily_usd_micros, \
                 budget_max_tokens = EXCLUDED.budget_max_tokens, \
                 agent_tools = EXCLUDED.agent_tools, \
                 status = 'active', \
                 updated_at = now() \
             RETURNING {COLS}"
        );
        let row: Row = sqlx::query_as(&sql)
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(new.app_id)
            .bind(new.app_name)
            .bind(new.installed_version)
            .bind(new.granted_scopes)
            .bind(new.client_id_b1)
            .bind(new.client_id_b2)
            .bind(&ctx.principal.id)
            .bind(&new.ai.budget_models)
            .bind(new.ai.budget_daily_usd_micros)
            .bind(new.ai.budget_max_tokens)
            .bind(&new.ai.agent_tools)
            .fetch_one(&self.db)
            .await
            .map_err(map_db)?;
        Ok(row.into())
    }

    /// azp（登録 client_id）から**有効な**インストールを引く（ゲートウェイの二重ゲート第2段）。
    ///
    /// B1/B2 いずれの client_id でも解決する。revoked / 不在は `None`（fail-closed）。
    pub async fn resolve_active_by_client(
        &self,
        tenant_id: &str,
        client_id: &str,
    ) -> Result<Option<AppInstallation>, GatewayError> {
        let sql = format!(
            "SELECT {COLS} FROM app_installation \
             WHERE tenant_id = $1 AND status = 'active' \
               AND (client_id_b1 = $2 OR client_id_b2 = $2) \
             LIMIT 1"
        );
        let row: Option<Row> = sqlx::query_as(&sql)
            .bind(tenant_id)
            .bind(client_id)
            .fetch_optional(&self.db)
            .await
            .map_err(map_db)?;
        Ok(row.map(Into::into))
    }

    /// インストールを失効させる（アンインストール・即時反映）。
    pub async fn revoke(&self, ctx: &AuthContext, app_id: Uuid) -> Result<(), GatewayError> {
        let updated = sqlx::query(&format!(
            "UPDATE app_installation SET status = '{}', updated_at = now() \
             WHERE tenant_id = $1 AND app_id = $2",
            InstallStatus::Revoked.as_str()
        ))
        .bind(&ctx.tenant_id)
        .bind(app_id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(GatewayError::NotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip_and_fail_closed() {
        assert_eq!(InstallStatus::Active.as_str(), "active");
        assert_eq!(InstallStatus::Revoked.as_str(), "revoked");
        assert_eq!(InstallStatus::parse("active"), InstallStatus::Active);
        assert_eq!(InstallStatus::parse("revoked"), InstallStatus::Revoked);
        // 未知値は fail-closed（revoked 扱い）。
        assert_eq!(InstallStatus::parse("bogus"), InstallStatus::Revoked);
        assert_eq!(InstallStatus::parse(""), InstallStatus::Revoked);
    }
}
