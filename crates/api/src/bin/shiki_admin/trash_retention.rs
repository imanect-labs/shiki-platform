//! `shiki-admin trash-retention --tenant <t> --days <n|off> [--actor <id>]`（GAP-108c・D-202）。
//!
//! テナントのゴミ箱の保持期間（1〜3650 日・既定は無効）を設定する。日数を過ぎたゴミ箱の中身は、システムが自動で
//! 完全に削除する（元に戻せない）。`off` で自動の完全削除を止める。運用者 API
//! （`PUT /admin/tenants/{tenant_id}/trash-retention`）と同じ処理で、日数を入れたときと短くしたときは、その時点から
//! 数え直す（一斉に消えない猶予）。変更は監査に残す（実行者は申告値として metadata に出所つきで残す・
//! audit-retention と同じ扱い）。

use anyhow::{bail, Context};
use api::config::AppConfig;
use sqlx::postgres::PgPoolOptions;

/// パース済みの引数。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Args {
    pub(crate) tenant: String,
    /// 保持日数（`None` は自動の完全削除を止める）。
    pub(crate) days: Option<i32>,
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
    let days = days.context("--days が必要です（自動の完全削除を止めるときは off）")?;
    let days = if days == "off" {
        None
    } else {
        let d: i32 = days
            .parse()
            .context("--days は整数か off で指定してください")?;
        storage::trash_retention::validate_trash_retention_days(d)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Some(d)
    };
    Ok(Args {
        tenant,
        days,
        actor,
    })
}

/// 日数の表示（`None` は自動では消さない）。
fn show(days: Option<i32>) -> String {
    days.map_or_else(|| "off".into(), |d| format!("{d} 日"))
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
    let change = storage::trash_retention::TrashRetention::new(db)
        .set_retention_days(
            &a.tenant,
            a.days,
            "cli",
            serde_json::json!({ "actor_claimed": actor_claimed, "actor_source": actor_source }),
        )
        .await
        .context("保持期間の設定に失敗")?;
    let Some(change) = change else {
        bail!("active なテナントが見つかりません: {}", a.tenant);
    };
    println!(
        "tenant.trash_retention.set: tenant={} {} → {}",
        a.tenant,
        show(change.previous),
        show(change.days)
    );
    if change.grace_reset {
        if let (Some(days), Some(since)) = (change.days, change.since) {
            println!(
                "  いまゴミ箱にあるものは {} 以降に消え始めます（入れた・短くした時点から {days} 日を数え直す猶予）",
                (since + chrono::Duration::days(i64::from(days))).format("%Y-%m-%d %H:%M UTC")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn parses_days_and_off() {
        assert_eq!(
            parse(&s(&["--tenant", "a-corp", "--days", "30"])).ok(),
            Some(Args {
                tenant: "a-corp".into(),
                days: Some(30),
                actor: None,
            })
        );
        assert_eq!(
            parse(&s(&[
                "--tenant", "a-corp", "--days", "off", "--actor", "ops"
            ]))
            .ok(),
            Some(Args {
                tenant: "a-corp".into(),
                days: None,
                actor: Some("ops".into()),
            })
        );
    }

    #[test]
    fn rejects_out_of_range_and_bad_args() {
        assert!(
            parse(&s(&["--tenant", "a-corp", "--days", "0"])).is_err(),
            "下限 1"
        );
        assert!(
            parse(&s(&["--tenant", "a-corp", "--days", "3651"])).is_err(),
            "上限 3650"
        );
        assert!(parse(&s(&["--tenant", "a-corp", "--days", "x"])).is_err());
        assert!(parse(&s(&["--tenant", "a-corp", "--days", "null"])).is_err());
        // 省略で止めない（止めるときは off を明示する）。
        assert!(parse(&s(&["--tenant", "a-corp"])).is_err());
        assert!(parse(&s(&["--days", "30"])).is_err());
        assert!(parse(&s(&["--tenant", "a/b", "--days", "30"])).is_err());
        assert!(parse(&s(&["--bogus", "1"])).is_err());
        assert!(parse(&s(&["--tenant", "a-corp", "--days"])).is_err());
    }

    #[test]
    fn shows_off_and_days() {
        assert_eq!(show(None), "off");
        assert_eq!(show(Some(30)), "30 日");
    }
}
