//! `shiki-admin migrate-org-groups` — 旧 org group を `/tenants/<tenant>/...` へ移す（#811）。
//!
//! 本体は `api::keycloak_admin::org_group_migration`（読み取り → 計画 → 適用）。ここは引数・
//! 設定の配線、tenant 登録簿の参照、報告、セッション失効だけを持つ。

use anyhow::{bail, Context};
use api::{
    config::{AppConfig, AuthConfig, Tenancy},
    keycloak_admin::{
        org_group_migration::{apply_tree, plan_tree, snapshot, TenantRule, TreePlan},
        KeycloakAdmin,
    },
};
use sqlx::postgres::PgPoolOptions;

/// 引数を解釈する（`--execute` の有無）。
pub(crate) fn parse_args(args: &[String]) -> anyhow::Result<bool> {
    let mut execute = false;
    for a in args {
        match a.as_str() {
            "--execute" => execute = true,
            other => bail!("不明な引数: {other}"),
        }
    }
    Ok(execute)
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
    let execute = parse_args(args)?;
    let mode = if execute { "EXECUTE" } else { "DRY-RUN" };
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

    let trees = snapshot(&kc).await.context("旧 group の読み取りに失敗")?;
    if trees.is_empty() {
        println!("テナント無修飾の旧 group はありません（移行済み）。");
        return Ok(());
    }
    let mut plans: Vec<TreePlan> = Vec::new();
    for tree in &trees {
        // メンバーの居ない旧 group の複製先（作成時に用意された空の org group 用）。
        let registered: Vec<String> = sqlx::query_scalar(
            "SELECT tenant_id FROM tenant WHERE org = $1 AND status <> 'deleted' ORDER BY tenant_id",
        )
        .bind(&tree.org)
        .fetch_all(&db)
        .await
        .context("tenant 登録簿の参照に失敗")?;
        let plan = plan_tree(tree, &rule, &registered);
        for line in plan.report() {
            println!("{line}");
        }
        plans.push(plan);
    }
    let old_paths: Vec<String> = trees.iter().map(|t| format!("/{}", t.org)).collect();
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
    if !execute {
        println!("DRY-RUN のため書き換えなし。--execute で実行します。");
        return Ok(());
    }

    let mut moved = 0usize;
    let mut tenants: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for plan in &plans {
        let stats = apply_tree(&kc, plan)
            .await
            .with_context(|| format!("/{} の移行に失敗（再実行で収束）", plan.org))?;
        moved += stats.members_moved;
        tenants.extend(plan.targets.iter().cloned());
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
    println!("完了: メンバー移動 {moved} 件");

    // 既存セッションの principal は旧 groups claim のまま（refresh まで新パスが載らない）なので、
    // 移行先テナントのセッションを失効させ、再ログインで新しい claim を取らせる。
    use api::session::{RedisSessionStore, SessionStore};
    match RedisSessionStore::connect(&config.session.redis_url).await {
        Ok(sessions) => {
            for t in &tenants {
                match sessions.delete_tenant(t).await {
                    Ok(n) => println!("sessions: tenant={t} {n} 件失効"),
                    Err(e) => eprintln!("sessions 失効に失敗 tenant={t}（手動で対応要）: {e}"),
                }
            }
        }
        Err(e) => eprintln!("Redis 接続に失敗（sessions は手動で失効させること）: {e}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parse_args_defaults_to_dry_run() {
        assert!(!parse_args(&[]).unwrap());
        assert!(parse_args(&["--execute".into()]).unwrap());
        assert!(parse_args(&["--force".into()]).is_err());
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
