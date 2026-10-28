//! `shiki-admin audit-retention --tenant <t> --days <n> [--actor <id>]`（#831）。
//!
//! テナントの監査ログの保持期間（90〜3650 日・既定 365）を設定する。運用者 API
//! （`PUT /admin/tenants/{tenant_id}/audit-retention`）と同じ処理で、変更は監査に残す
//! （実行者は申告値として metadata に出所つきで残す・audit-viewer と同じ扱い）。

use anyhow::{bail, Context};
use api::config::AppConfig;
use sqlx::postgres::PgPoolOptions;

/// パース済みの引数。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Args {
    pub(crate) tenant: String,
    pub(crate) days: i32,
    pub(crate) actor: Option<String>,
}

/// 引数をパースする。
pub(crate) fn parse(args: &[String]) -> anyhow::Result<Args> {
    let (mut tenant, mut days, mut actor) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--tenant" => &mut tenant,
            "--days" => &mut days,
            "--actor" => &mut actor,
            other => bail!("不明な引数: {other}"),
        };
        *slot = Some(
            it.next()
                .with_context(|| format!("{a} に値が必要です"))?
                .clone(),
        );
    }
    let tenant = tenant.context("--tenant が必要です")?;
    if let Err(v) = authz::validate_tenant_id(&tenant) {
        bail!("--tenant が tenant_id として不正です: {v}");
    }
    let days: i32 = days
        .context("--days が必要です")?
        .parse()
        .context("--days は整数で指定してください")?;
    storage::audit_retention::validate_retention_days(days).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Args {
        tenant,
        days,
        actor,
    })
}

/// 保持期間を設定する。
pub(crate) async fn run(args: &[String]) -> anyhow::Result<()> {
    let a = parse(args)?;
    let config = AppConfig::load().context("設定のロードに失敗")?;
    let db_options: sqlx::postgres::PgConnectOptions = config
        .database
        .url
        .parse()
        .context("DATABASE_URL の解析に失敗")?;
    let db = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(db_options.application_name("shiki-admin"))
        .await
        .context("Postgres へ接続できません")?;
    let (actor_claimed, actor_source) = super::audit_viewer::claimed_actor(a.actor);
    let previous = storage::audit_retention::AuditRetention::new(db)
        .set_retention_days(
            &a.tenant,
            a.days,
            "cli",
            serde_json::json!({ "actor_claimed": actor_claimed, "actor_source": actor_source }),
        )
        .await
        .context("保持期間の設定に失敗")?;
    let Some(previous) = previous else {
        bail!("active なテナントが見つかりません: {}", a.tenant);
    };
    println!(
        "tenant.audit_retention.set: tenant={} {} 日 → {} 日",
        a.tenant, previous, a.days
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn parses_and_validates() {
        assert_eq!(
            parse(&s(&["--tenant", "a-corp", "--days", "730"])).ok(),
            Some(Args {
                tenant: "a-corp".into(),
                days: 730,
                actor: None,
            })
        );
        assert!(
            parse(&s(&["--tenant", "a-corp", "--days", "30"])).is_err(),
            "下限 90"
        );
        assert!(parse(&s(&["--tenant", "a-corp", "--days", "x"])).is_err());
        assert!(parse(&s(&["--tenant", "a-corp"])).is_err());
        assert!(parse(&s(&["--days", "365"])).is_err());
        assert!(parse(&s(&["--tenant", "a/b", "--days", "365"])).is_err());
        assert!(parse(&s(&["--bogus", "1"])).is_err());
    }
}
