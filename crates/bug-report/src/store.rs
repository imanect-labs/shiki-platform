use crate::{BugReport, BugReportPolicy, Error};
use authz::AuthContext;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// SQL はこの crate に閉じ、service が認可と transaction を所有する。
pub struct BugReportStore {
    pub(crate) pool: PgPool,
}
impl BugReportStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
pub(crate) async fn lock(conn: &mut PgConnection, ctx: &AuthContext) -> Result<(), Error> {
    // org 全体を直列化し、複数 replica の policy・上限・command 競合を一緒に防ぐ。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("bug-report:{}:{}", ctx.tenant_id(), ctx.org()))
        .execute(conn)
        .await?;
    Ok(())
}
pub(crate) async fn policy(
    conn: &mut PgConnection,
    ctx: &AuthContext,
) -> Result<BugReportPolicy, Error> {
    let value: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT settings FROM bug_report_policy WHERE tenant_id=$1 AND org=$2 FOR UPDATE",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .fetch_optional(conn)
    .await?;
    value.map_or_else(
        || Ok(BugReportPolicy::default()),
        |v| serde_json::from_value(v).map_err(|_| Error::Invalid),
    )
}
pub(crate) async fn report(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<BugReport, Error> {
    let value: serde_json::Value = sqlx::query_scalar(
        "SELECT snapshot FROM bug_report WHERE tenant_id=$1 AND org=$2 AND id=$3 AND body_purged_at IS NULL FOR UPDATE",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(Error::NotFound)?;
    serde_json::from_value(value).map_err(|_| Error::Invalid)
}
pub(crate) fn json<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, Error> {
    serde_json::to_value(value).map_err(|_| Error::Invalid)
}
