//! `shiki-admin audit-viewer (grant|revoke) --tenant <t> --org <o> --user <sub> [--actor <id>]`（#634）。
//!
//! 監査閲覧者（`organization#audit_viewer`）を付ける・外す。audit_viewer は admin と独立なので、
//! 管理者でない監査担当者もここで付与する（メンバー管理の画面ができるまでの運用経路）。
//! 付与・剥奪は監査に残す（実行者は申告値として metadata に残す・retenant と同じ扱い）。

use anyhow::{bail, Context};
use api::config::AppConfig;
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    model, AuthContext, AuthzClient, Principal, Relation,
};
use sqlx::postgres::PgPoolOptions;

/// 付けるか外すか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Grant,
    Revoke,
}

/// パース済みの引数。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Args {
    pub(crate) op: Op,
    pub(crate) tenant: String,
    pub(crate) org: String,
    pub(crate) user: String,
    pub(crate) actor: Option<String>,
}

/// 引数をパースする（`args[0]` が grant/revoke）。
pub(crate) fn parse(args: &[String]) -> anyhow::Result<Args> {
    let op = match args.first().map(String::as_str) {
        Some("grant") => Op::Grant,
        Some("revoke") => Op::Revoke,
        _ => bail!("grant か revoke を指定してください"),
    };
    let (mut tenant, mut org, mut user, mut actor) = (None, None, None, None);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--tenant" => &mut tenant,
            "--org" => &mut org,
            "--user" => &mut user,
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
    Ok(Args {
        op,
        tenant,
        org: org.context("--org が必要です")?,
        user: user.context("--user が必要です")?,
        actor,
    })
}

/// 付与・剥奪を実行する。
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
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("HTTP クライアントの初期化に失敗")?;
    let fga = OpenFgaClient::connect(
        http,
        &OpenFgaConfig {
            base_url: config.authz.base_url.clone(),
            store_name: config.authz.store_name.clone(),
        },
        &model::default_model(),
    )
    .await
    .context("OpenFGA へ接続できません")?;

    // 監査の subject は CLI 固定値（申告値は認証されていないので metadata に出所つきで残す）。
    let ctx = AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "cli".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(a.tenant.clone()),
        },
        a.org.clone(),
        a.tenant.clone(),
    );
    let subject = ctx.ns().user(&a.user);
    let org_obj = ctx.ns().organization(&a.org);
    let action = match a.op {
        Op::Grant => {
            fga.write_tuple(&subject, Relation::AuditViewer, &org_obj)
                .await
                .context("audit_viewer の付与に失敗")?;
            "org_audit.viewer.grant"
        }
        Op::Revoke => {
            fga.delete_tuple(&subject, Relation::AuditViewer, &org_obj)
                .await
                .context("audit_viewer の剥奪に失敗")?;
            "org_audit.viewer.revoke"
        }
    };
    let (actor_claimed, actor_source) = a.actor.map_or_else(
        || {
            std::env::var("SUDO_USER")
                .map(|v| (v, "sudo_user"))
                .or_else(|_| std::env::var("USER").map(|v| (v, "user")))
                .unwrap_or_else(|_| ("unknown".to_string(), "none"))
        },
        |v| (v, "flag"),
    );
    storage::audit::AuditRecorder::new(db)
        .record(
            &ctx,
            storage::audit::AuditEntry {
                action,
                object_type: "organization",
                object_id: &a.org,
                decision: storage::audit::Decision::Allow,
                trace_id: None,
                metadata: serde_json::json!({
                    "user": a.user,
                    "actor_claimed": actor_claimed, "actor_source": actor_source,
                }),
            },
        )
        .await
        .context("監査ログへの記録に失敗")?;
    println!(
        "{action}: tenant={} org={} user={}",
        a.tenant, a.org, a.user
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
    fn parses_grant() {
        let a = parse(&s(&[
            "grant", "--tenant", "a-corp", "--org", "a-corp", "--user", "u1",
        ]));
        assert_eq!(
            a.ok(),
            Some(Args {
                op: Op::Grant,
                tenant: "a-corp".into(),
                org: "a-corp".into(),
                user: "u1".into(),
                actor: None,
            })
        );
    }

    #[test]
    fn rejects_missing_or_unknown() {
        assert!(parse(&s(&["list"])).is_err());
        assert!(parse(&s(&["revoke", "--tenant", "a-corp", "--org", "a"])).is_err());
        assert!(parse(&s(&["grant", "--tenant"])).is_err());
        assert!(parse(&s(&["grant", "--bogus", "x"])).is_err());
        assert!(parse(&s(&[
            "grant", "--tenant", "a/b", "--org", "a", "--user", "u"
        ]))
        .is_err());
    }
}
