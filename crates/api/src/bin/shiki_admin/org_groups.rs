//! `shiki-admin migrate-org-groups` — 旧 org group を `/tenants/<tenant>/...` へ移す（#811）。
//!
//! ```text
//! shiki-admin migrate-org-groups [--only <slug>]... [--exclude <slug>]... [--execute]
//! ```
//!
//! 本体は `api::keycloak_admin::org_group_migration`（一覧 → 選定 → 読み取り → 計画 → 適用）。
//! ここは引数・設定の配線、tenant 登録簿の参照、報告、セッション失効だけを持つ。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context};
use api::{
    config::{AppConfig, AuthConfig, Tenancy},
    keycloak_admin::{
        org_group_migration::{
            apply_tree, list_realm_groups, plan_tree, select_orgs, snapshot_tree, TenantRule,
            TreePlan,
        },
        KcGroup, KeycloakAdmin,
    },
    session::{RedisSessionStore, SessionStore},
};
use sqlx::postgres::PgPoolOptions;

/// CLI 引数。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Args {
    pub execute: bool,
    /// 対象 slug を明示する（tenant 登録簿に無い org を移すとき）。
    pub only: Vec<String>,
    /// 対象から除く slug。
    pub exclude: Vec<String>,
}

/// 引数を解釈する。
pub(crate) fn parse_args(args: &[String]) -> anyhow::Result<Args> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--execute" => out.execute = true,
            "--only" => out
                .only
                .push(it.next().context("--only に値が必要です")?.clone()),
            "--exclude" => out
                .exclude
                .push(it.next().context("--exclude に値が必要です")?.clone()),
            other => bail!("不明な引数: {other}"),
        }
    }
    Ok(out)
}

/// テナンシーモードから移行先テナントの決め方を選ぶ（ログイン時の tenant 解決と同じ取得元）。
pub(crate) fn tenant_rule(auth: &AuthConfig) -> anyhow::Result<TenantRule> {
    match auth.tenancy {
        Tenancy::Multi => Ok(TenantRule::Attribute),
        Tenancy::Single => {
            let fixed = auth
                .tenant_id
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .context("auth.tenancy=single で auth.tenant_id が空です")?;
            if let Err(violation) = authz::validate_tenant_id(fixed) {
                bail!("auth.tenant_id が tenant_id として不正です: {violation}");
            }
            Ok(TenantRule::Fixed(fixed.to_string()))
        }
    }
}

pub(crate) async fn run(args: &[String]) -> anyhow::Result<()> {
    let args = parse_args(args)?;
    let mode = if args.execute { "EXECUTE" } else { "DRY-RUN" };
    let config = AppConfig::load().context("設定のロードに失敗")?;
    let rule = tenant_rule(&config.auth)?;
    println!("== shiki-admin migrate-org-groups [{mode}] tenant 決定={rule:?} ==");

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Keycloak 用 HTTP クライアントの初期化に失敗")?;
    let kc = KeycloakAdmin::from_config(&http, &config.auth)
        .context("Keycloak admin の設定が不足（auth.provisioner_client_id / secret）")?;
    let db_options: sqlx::postgres::PgConnectOptions = config
        .database
        .url
        .parse()
        .context("DATABASE_URL の解析に失敗")?;
    let db = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(db_options.application_name("shiki-admin"))
        .await
        .context("Postgres へ接続できません")?;

    let realm = list_realm_groups(&kc)
        .await
        .context("group 一覧の読み取りに失敗")?;
    let registry_orgs: BTreeSet<String> =
        sqlx::query_scalar("SELECT DISTINCT org FROM tenant WHERE status <> 'deleted'")
            .fetch_all(&db)
            .await
            .context("tenant 登録簿の参照に失敗")?
            .into_iter()
            .collect();
    let legacy_names: Vec<String> = realm.legacy.iter().map(|g| g.name.clone()).collect();
    let selection = select_orgs(
        &legacy_names,
        &registry_orgs,
        &realm.migrated,
        &args.only,
        &args.exclude,
    );
    for (slug, reason) in &selection.skipped {
        println!("対象外: /{slug}（{reason}）");
    }
    for slug in &selection.missing {
        eprintln!("⚠️ --only /{slug} に該当する旧 group がありません");
    }
    if selection.selected.is_empty() {
        println!("移行する旧 org group はありません（移行済み・または対象外のみ）。");
        return Ok(());
    }

    let mut plans: Vec<TreePlan> = Vec::new();
    for slug in &selection.selected {
        let Some(top) = realm.legacy.iter().find(|g| &g.name == slug) else {
            continue;
        };
        let plan = plan_for(&kc, &db, top, &rule, &realm.migrated).await?;
        for line in plan.report() {
            println!("{line}");
        }
        plans.push(plan);
    }
    report_references(&kc, &selection.selected).await;
    if !args.execute {
        println!("DRY-RUN のため書き換えなし。--execute で実行します。");
        return Ok(());
    }
    execute(&kc, &config, &plans).await
}

/// 旧 group 1 つを読み、登録簿の同 slug テナントと移行済みテナントを添えて計画を立てる。
async fn plan_for(
    kc: &KeycloakAdmin<'_>,
    db: &sqlx::PgPool,
    top: &KcGroup,
    rule: &TenantRule,
    migrated: &BTreeMap<String, Vec<String>>,
) -> anyhow::Result<TreePlan> {
    let tree = snapshot_tree(kc, top)
        .await
        .with_context(|| format!("/{} の読み取りに失敗", top.name))?;
    let registered: Vec<String> = sqlx::query_scalar(
        "SELECT tenant_id FROM tenant WHERE org = $1 AND status <> 'deleted' ORDER BY tenant_id",
    )
    .bind(&tree.org)
    .fetch_all(db)
    .await
    .context("tenant 登録簿の参照に失敗")?;
    let already = migrated.get(&tree.org).cloned().unwrap_or_default();
    Ok(plan_tree(&tree, rule, &registered, &already))
}

/// 旧 group パスを参照する realm 設定を報告する（移行しない・手で付け替える）。
async fn report_references(kc: &KeycloakAdmin<'_>, slugs: &[String]) {
    let old_paths: Vec<String> = slugs.iter().map(|s| format!("/{s}")).collect();
    let refs = kc.group_path_references(&old_paths).await;
    if refs.found.is_empty() {
        println!("旧 group パスを参照する realm 設定: なし");
    } else {
        println!(
            "⚠️ 旧 group パスを参照する realm 設定 {} 件（移行しない・`/tenants/<tenant>/...` へ手で付け替える）:",
            refs.found.len()
        );
        for r in &refs.found {
            println!("  {r}");
        }
    }
    for u in &refs.unchecked {
        eprintln!("⚠️ 調べられなかった realm 設定（手で確認要）: {u}");
    }
}

/// 計画を順に適用し、メンバーを移したテナントのセッションをその都度失効させる。
///
/// 既存セッションの principal は旧 groups claim のまま（refresh まで新パスが載らない）なので、
/// 再ログインで新しい claim を取らせる。途中の旧 group で失敗しても、それまでに移した分の
/// 失効は済ませてからエラーを返す（再実行では移動済みのテナントが計画に出てこないため）。
async fn execute(
    kc: &KeycloakAdmin<'_>,
    config: &AppConfig,
    plans: &[TreePlan],
) -> anyhow::Result<()> {
    let sessions = match RedisSessionStore::connect(&config.session.redis_url).await {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!(
                "⚠️ Redis 接続に失敗（移行先テナントのセッションを手動で失効させること）: {e}"
            );
            None
        }
    };
    let mut revoked: BTreeSet<String> = BTreeSet::new();
    let mut moved = 0usize;
    let mut failed_removals = 0usize;
    for plan in plans {
        let result = apply_tree(kc, plan).await;
        // 移動を試みたテナント（成功・失敗を問わない）。
        let touched: BTreeSet<&String> = plan.moves.iter().map(|m| &m.tenant).collect();
        for t in touched {
            if !revoked.insert(t.clone()) {
                continue;
            }
            match &sessions {
                Some(store) => match store.delete_tenant(t).await {
                    Ok(n) => println!("sessions: tenant={t} {n} 件失効"),
                    Err(e) => eprintln!("sessions 失効に失敗 tenant={t}（手動で対応要）: {e}"),
                },
                None => eprintln!("sessions 未失効 tenant={t}（手動で対応要）"),
            }
        }
        let stats =
            result.with_context(|| format!("/{} の移行に失敗（再実行で収束）", plan.org))?;
        moved += stats.members_moved;
        failed_removals += stats.failed_removals.len();
        for f in &stats.failed_removals {
            eprintln!("⚠️ 旧 group から外せなかったメンバー（旧 group は保持）: {f}");
        }
        println!(
            "/{}: group {} 個を用意・メンバー {} 件を移動・旧 group {}",
            plan.org,
            stats.groups_ensured,
            stats.members_moved,
            if stats.old_deleted {
                "削除"
            } else {
                "保持"
            }
        );
    }
    // 旧 group（と付いた role）が残ったままなので、成功扱いにしない（自動化が再実行を省かないように）。
    if failed_removals != 0 {
        bail!(
            "旧 group から外せないメンバーが {failed_removals} 件あります（メンバー移動 {moved} 件）。\
             LDAP 側のマッパー設定を直して再実行してください"
        );
    }
    println!("完了: メンバー移動 {moved} 件");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parse_args_defaults_to_dry_run() {
        assert_eq!(parse_args(&[]).unwrap(), Args::default());
        let args: Vec<String> = [
            "--only",
            "acme",
            "--exclude",
            "ops",
            "--only",
            "b",
            "--execute",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            parse_args(&args).unwrap(),
            Args {
                execute: true,
                only: vec!["acme".into(), "b".into()],
                exclude: vec!["ops".into()],
            }
        );
        assert!(parse_args(&["--force".into()]).is_err());
        assert!(parse_args(&["--only".into()]).is_err());
    }

    fn auth(tenancy: Tenancy, tenant_id: Option<&str>) -> AuthConfig {
        AuthConfig {
            issuer: "http://localhost/realms/shiki".into(),
            internal_base_url: None,
            jwks_uri: None,
            audience: "shiki-api".into(),
            jwks_ttl_secs: 300,
            client_id: "shiki-web".into(),
            client_secret: None,
            redirect_uri: "http://localhost:3000/auth/callback".into(),
            post_logout_redirect_uri: "http://localhost:3000/".into(),
            scopes: "openid profile".into(),
            tenancy,
            tenant_id: tenant_id.map(str::to_string),
            provisioner_client_id: None,
            provisioner_client_secret: None,
            admin_base_url: None,
        }
    }

    #[test]
    fn tenant_rule_follows_tenancy() {
        // multi は属性（claim と同じ取得元）・single は固定値。固定値が空/不正なら止める。
        assert_eq!(
            tenant_rule(&auth(Tenancy::Multi, Some("ignored"))).unwrap(),
            TenantRule::Attribute
        );
        assert_eq!(
            tenant_rule(&auth(Tenancy::Single, Some(" onprem "))).unwrap(),
            TenantRule::Fixed("onprem".into())
        );
        assert!(tenant_rule(&auth(Tenancy::Single, None)).is_err());
        assert!(tenant_rule(&auth(Tenancy::Single, Some("a|b"))).is_err());
    }
}
