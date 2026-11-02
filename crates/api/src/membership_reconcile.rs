//! 組織の member と部署の role#member を、Keycloak の所属グループと定期に照合して外す（D-136）。
//!
//! 付けるのはこれまでどおりログインとセッションの更新（`routes/auth/org_member`・`callback::provision_roles`）だけで、
//! ここは外すだけ。shiki の web にログインしないまま Keycloak からアプリのトークン（B1）だけを取り続ける人も、
//! グループから外せばここで member が外れ、アプリのゲートウェイが 403 で断る。
//!
//! # 何を外すか（ログインの同期と同じ単一定義を使う）
//!
//! - **組織**: 所属グループから決まりうる組織（[`org_member::orgs_kept_by_reconcile`]）のほかの組織の member。
//!   組織が 1 つも決まらない人は既定の組織の member を残す（BUG-200）。admin・audit_viewer・他テナントのタプルには
//!   触れない（[`org_member::revoke_stale`]）。
//! - **部署**: ログインの同期が所属グループ由来と記録したロール（`directory_user_role.source = 'group'`）のうち、
//!   いま直接タプルがあり、所属グループ（[`authz::org_group::tenant_relative_groups`]）にも、ロールの claim の元の
//!   属性 `roles` にも無いもの（[`role_member::revoke_roles`]）。属性 `roles` 由来のロールと手で書いたタプルには触れない。
//!
//! # 外さないとき（fail-safe）
//!
//! - Keycloak を読めない（接続できない・権限が無い・5xx・パスの無いグループ）人は、何も外さない。
//! - Keycloak にいない人（404）も外さない。realm を取り違えた設定で全員を外さないため（消えた人はトークンを取れない）。
//! - OpenFGA の今のタプルを読めないときも外さない。
//! - 読む順は OpenFGA の今のタプル → Keycloak。ログインの同期が付けたばかりの member を、それより前に読んだ所属で
//!   外さない（タプルを読んだ後に Keycloak を読むので、そのタプルの元になった所属は必ず見える）。
//!
//! # 回し方
//!
//! - `membership_reconcile.interval_secs`（既定 60 秒・下限 10 秒）ごとに、続きの位置（migration 0130 の
//!   `membership_reconcile_cursor`）から `batch_size` 人（既定 500・1〜1,000）を照合する。最後まで読んだら頭に戻る。
//!   ディレクトリの N 人を 1 周するのに ceil(N / batch_size) 回かかる（既定では 1,000 人で 2 分、10,000 人で 20 分）。
//! - 1 人につき OpenFGA の読みが 1〜2 回と、Keycloak の読みが 1 回（部署の候補がある人は 2 回）。直接タプルの無い人は
//!   Keycloak を読まない。Keycloak のトークンは 1 回に 1 つ取る。
//! - サーバーが何台あっても、advisory lock を取れた 1 台だけがその回を進める（取れなければその回は何もしない）。
//! - Keycloak の失敗が [`MAX_FAILURE_STREAK`] 人続いたら（かページの終わりで失敗したら）、トークンを取り直して Keycloak に
//!   届くか確かめる。届かなければその回を止め、失敗した人から次の回に読み直す。届けば、その人たちは次の 1 周まで
//!   読み直さない（その人だけの失敗で照合が進まなくならない）。
//! - provisioner（`auth.provisioner_client_id`・`provisioner_client_secret`）が無ければ動かない（ログインとセッションの
//!   更新でだけ外れる）。

use std::panic::AssertUnwindSafe;
use std::{sync::Arc, time::Duration};

use authz::{AuthContext, AuthzClient, GroupLayout, Namespace, Principal, PrincipalKind};
use futures::FutureExt as _;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use storage::{DirectoryEntry, DirectoryStore, StorageError};

use crate::{
    config::AuthConfig,
    extract::{group_layout, DEFAULT_ORG},
    keycloak_admin::{AdminSession, KeycloakAdmin, KeycloakAdminError},
    routes::auth::{org_member, role_member},
};

/// advisory lock の鍵（この照合専用の固定値）。
const LOCK_KEY: i64 = 136_000_136;
/// 間隔の下限。
const MIN_INTERVAL: Duration = Duration::from_secs(10);
/// 1 回に照合する人数の上限。
const MAX_BATCH: usize = 1_000;
/// Keycloak に届くかを確かめ直す、続いた失敗の人数。
pub const MAX_FAILURE_STREAK: usize = 3;
/// ロールの claim の元になる利用者の属性（`deploy/keycloak/README.md` の User Profile・shiki-web のマッパー）。
const ROLES_ATTRIBUTE: &str = "roles";

/// 定期の照合の設定（`[membership_reconcile]`・`SHIKI__MEMBERSHIP_RECONCILE__*`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembershipReconcileConfig {
    /// 照合を動かすか（既定 true。provisioner が無ければ true でも動かない）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 間隔（秒）。既定 60（下限 10）。
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// 1 回に照合する人数。既定 500（1〜1,000 に収める）。
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

impl Default for MembershipReconcileConfig {
    fn default() -> Self {
        MembershipReconcileConfig {
            enabled: true,
            interval_secs: default_interval_secs(),
            batch_size: default_batch_size(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_interval_secs() -> u64 {
    60
}

fn default_batch_size() -> usize {
    500
}

/// 照合が使うもの。
#[derive(Clone)]
pub struct ReconcileDeps {
    /// advisory lock と続きの位置。
    pub db: PgPool,
    pub authz: Arc<dyn AuthzClient>,
    pub directory: Arc<DirectoryStore>,
    /// 部署を外したら構造化データの購読へ知らせる（ログインの同期と同じ）。
    pub data: Arc<data::DataStore>,
    /// Keycloak の管理 API（上限時間つきのクライアント）。
    pub http: reqwest::Client,
    pub auth: AuthConfig,
    /// 1 回に照合する人数（1〜1,000）。
    pub batch_size: usize,
}

/// 1 回の照合の結果（ログ・テスト用）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// ディレクトリから読んだ人。
    pub read: usize,
    /// Keycloak と照合した人（直接タプルの無い人は数えない）。
    pub checked: usize,
    /// Keycloak にいなかった人（外さない）。
    pub not_found: usize,
    /// Keycloak を読めなかった人（外さない）。
    pub failed: usize,
    /// member を外した組織の数。
    pub revoked_orgs: usize,
    /// member を外した部署の数。
    pub revoked_roles: usize,
    /// 最後まで読んで頭に戻った。
    pub wrapped: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error("照合の位置を読み書きできません: {0}")]
    Db(#[from] sqlx::Error),
    #[error("ディレクトリを読めません: {0}")]
    Storage(#[from] StorageError),
    #[error("Keycloak を読めません: {0}")]
    Keycloak(#[from] KeycloakAdminError),
}

/// 定期の照合を起動する（止めてある・provisioner が無いなら何もしない）。
pub fn spawn(mut deps: ReconcileDeps, config: &MembershipReconcileConfig) {
    if !config.enabled {
        tracing::info!(
            "membership_reconcile.enabled=false: 組織の member の定期の照合は動きません"
        );
        return;
    }
    if let Err(e) = KeycloakAdmin::from_config(&deps.http, &deps.auth) {
        tracing::info!(reason = %e, "Keycloak の管理 API が無いため、組織の member の定期の照合は動きません（ログインとセッションの更新でだけ外れます）");
        return;
    }
    deps.batch_size = config.batch_size.clamp(1, MAX_BATCH);
    let interval = Duration::from_secs(config.interval_secs).max(MIN_INTERVAL);
    tokio::spawn(async move {
        // 起動直後の負荷を避けて少し待つ。
        tokio::time::sleep(Duration::from_secs(30)).await;
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match run_once(&deps).await {
                Ok(Some(r)) if r.revoked_orgs + r.revoked_roles + r.failed > 0 => tracing::info!(
                    read = r.read,
                    checked = r.checked,
                    not_found = r.not_found,
                    failed = r.failed,
                    revoked_orgs = r.revoked_orgs,
                    revoked_roles = r.revoked_roles,
                    "組織の member と部署を Keycloak の所属グループと照合しました"
                ),
                Ok(Some(r)) => tracing::debug!(
                    read = r.read,
                    checked = r.checked,
                    "照合: 外すものはありません"
                ),
                Ok(None) => tracing::debug!("照合: 他のサーバーが照合中"),
                Err(e) => {
                    tracing::warn!(error = %e, "組織の member の照合に失敗（次の回に読み直す）");
                }
            }
        }
    });
}

/// 1 回分を照合する（ほかのサーバーが照合中なら `Ok(None)`）。
pub async fn run_once(deps: &ReconcileDeps) -> Result<Option<ReconcileReport>, ReconcileError> {
    // ロックとアンロックを同じ接続で行う（セッションロックは接続に紐づく）。
    let mut conn = deps.db.acquire().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        return Ok(None);
    }
    // パニックしても必ず解放する（解放せずにプールへ戻すと、その接続の寿命まで誰も照合できない）。
    let result = AssertUnwindSafe(run_locked(deps)).catch_unwind().await;
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await;
    match result {
        Ok(r) => r.map(Some),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn run_locked(deps: &ReconcileDeps) -> Result<ReconcileReport, ReconcileError> {
    let batch = deps.batch_size.clamp(1, MAX_BATCH);
    let after = load_cursor(&deps.db).await?;
    let page = deps.directory.users_after(after.as_ref(), batch).await?;
    let mut report = ReconcileReport {
        read: page.len(),
        ..ReconcileReport::default()
    };
    let mut done = 0;
    if !page.is_empty() {
        let admin = KeycloakAdmin::from_config(&deps.http, &deps.auth)?;
        // トークンを取れない（Keycloak に届かない）ときは、位置を進めずに止める。
        let mut kc = admin.session().await?;
        done = reconcile_page(deps, &mut kc, &page, &mut report).await?;
    }
    match cursor_move(page.len(), batch, done) {
        CursorMove::Rewind => {
            sqlx::query("DELETE FROM membership_reconcile_cursor")
                .execute(&deps.db)
                .await?;
            report.wrapped = true;
        }
        CursorMove::After(i) => {
            if let Some(last) = page.get(i) {
                save_cursor(&deps.db, last).await?;
            }
        }
        CursorMove::Stay => {}
    }
    Ok(report)
}

/// 次の回の続きの位置の動かし方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorMove {
    /// 最後まで読んだ。次の回は頭から。
    Rewind,
    /// ページの `i` 番目の人の次から。
    After(usize),
    /// 動かさない（1 人も終えていない）。
    Stay,
}

/// `batch` 人を頼んで `page_len` 人が返り、頭から `done` 人を終えたときの続きの位置（純粋関数）。
fn cursor_move(page_len: usize, batch: usize, done: usize) -> CursorMove {
    if done == page_len && page_len < batch {
        CursorMove::Rewind
    } else if done == 0 {
        CursorMove::Stay
    } else {
        CursorMove::After(done.min(page_len) - 1)
    }
}

/// 1 ページを照合し、頭から何人を終えたかを返す（Keycloak に届かなくなったら、そこで止める）。
pub(crate) async fn reconcile_page(
    deps: &ReconcileDeps,
    kc: &mut AdminSession<'_>,
    page: &[DirectoryEntry],
    report: &mut ReconcileReport,
) -> Result<usize, ReconcileError> {
    let recorded = deps.directory.group_roles_of(page).await?;
    let layout = group_layout(&deps.auth);
    let mut done = 0;
    let mut streak = 0;
    for (i, entry) in page.iter().enumerate() {
        let departments = recorded.get(entry).map_or(&[][..], Vec::as_slice);
        match reconcile_user(deps, kc, entry, departments, layout).await {
            UserOutcome::Untouched => {}
            UserOutcome::Checked { orgs, roles } => {
                report.checked += 1;
                report.revoked_orgs += orgs;
                report.revoked_roles += roles;
            }
            UserOutcome::NotFound => report.not_found += 1,
            UserOutcome::Failed(e) => {
                report.failed += 1;
                streak += 1;
                tracing::warn!(tenant_id = %entry.tenant_id, user = %entry.user_id, error = %e,
                    "照合: Keycloak から所属を読めません（この人は外さない）");
                if streak < MAX_FAILURE_STREAK && i + 1 < page.len() {
                    continue;
                }
                // 続けて失敗した（かページの終わり）。Keycloak に届くかを、トークンを取り直して確かめる。届けばこの人たちの
                // 失敗はこの人たちだけのものなので先へ進む（同じ人で毎回止まらない）。
                if let Err(e) = kc.refresh_token().await {
                    tracing::warn!(error = %e, "照合: Keycloak に届きません（この回を止め、次の回に読み直す）");
                    return Ok(done);
                }
            }
        }
        streak = 0;
        done = i + 1;
    }
    Ok(done)
}

/// 1 人の照合の結果。
enum UserOutcome {
    /// 照合するタプルが無い（Keycloak を読んでいない）。
    Untouched,
    /// 照合した（外した組織・部署の数）。
    Checked { orgs: usize, roles: usize },
    /// Keycloak にいない（外さない）。
    NotFound,
    /// Keycloak を読めない（外さない）。
    Failed(KeycloakAdminError),
}

/// 1 人を照合する（本文はモジュールの説明）。
async fn reconcile_user(
    deps: &ReconcileDeps,
    kc: &mut AdminSession<'_>,
    entry: &DirectoryEntry,
    recorded_departments: &[String],
    layout: GroupLayout,
) -> UserOutcome {
    let tenant_id = entry.tenant_id.as_str();
    if authz::validate_tenant_id(tenant_id).is_err()
        || authz::validate_local_id(&entry.user_id).is_err()
    {
        return UserOutcome::Untouched;
    }
    let authz = &*deps.authz;
    let ns = Namespace::for_tenant(tenant_id);
    let subject = ns.user(&entry.user_id);

    // 1. OpenFGA の今のタプル（Keycloak より先に読む）。読めなければその種類は外さない。
    let orgs = org_member::current_orgs(authz, &subject, &ns)
        .await
        .unwrap_or_default();
    let departments: Vec<String> = if recorded_departments.is_empty() {
        Vec::new()
    } else {
        role_member::current_roles(authz, &subject, &ns)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|role| recorded_departments.contains(role))
            .collect()
    };
    if orgs.is_empty() && departments.is_empty() {
        return UserOutcome::Untouched;
    }

    // 2. Keycloak の所属（部署の候補が所属グループに無ければ、属性 roles も読む）。外す前にすべて読む。
    let groups = match kc.user_group_paths(&entry.user_id).await {
        Ok(Some(groups)) => groups,
        Ok(None) => return UserOutcome::NotFound,
        Err(e) => return UserOutcome::Failed(e),
    };
    let in_groups: Vec<String> =
        authz::org_group::tenant_relative_groups(&groups, tenant_id, layout)
            .iter()
            .map(|d| d.trim().to_string())
            .collect();
    let mut stale_departments: Vec<String> = departments
        .into_iter()
        .filter(|d| !in_groups.contains(d))
        .collect();
    if !stale_departments.is_empty() {
        let roles = match kc.user_attribute(&entry.user_id, ROLES_ATTRIBUTE).await {
            Ok(Some(roles)) => roles,
            Ok(None) => return UserOutcome::NotFound,
            Err(e) => return UserOutcome::Failed(e),
        };
        stale_departments.retain(|d| !roles.iter().any(|r| r.trim() == d.as_str()));
    }

    // 3. 外す（ログインの同期と同じ手順）。
    let keep = org_member::orgs_kept_by_reconcile(&groups, tenant_id, layout);
    let revoked_orgs = org_member::revoke_stale(authz, &subject, &ns, &orgs, &keep).await;
    let revoked_roles = role_member::revoke_roles(authz, &subject, &ns, &stale_departments).await;
    if !revoked_orgs.is_empty() || !revoked_roles.is_empty() {
        tracing::info!(%tenant_id, user = %entry.user_id, orgs = ?revoked_orgs, roles = ?revoked_roles,
            "照合: 所属グループから外れた人の member を外した");
    }
    if !revoked_roles.is_empty() {
        after_roles_revoked(deps, entry, &groups, layout, &revoked_roles).await;
    }
    UserOutcome::Checked {
        orgs: revoked_orgs.len(),
        roles: revoked_roles.len(),
    }
}

/// 部署を外した後: 写し（AI 活用分析の部署集計）から消し、構造化データの購読へ知らせる（ログインの同期と同じ）。
async fn after_roles_revoked(
    deps: &ReconcileDeps,
    entry: &DirectoryEntry,
    groups: &[String],
    layout: GroupLayout,
    revoked: &[String],
) {
    if let Err(e) = deps
        .directory
        .remove_user_roles(&entry.tenant_id, &entry.user_id, revoked)
        .await
    {
        tracing::warn!(error = %e, "照合: directory_user_role から外せません（次のログインで置き換わる）");
    }
    let org = org_member::desired_org(groups, &entry.tenant_id, layout)
        .unwrap_or_else(|| DEFAULT_ORG.to_string());
    let ctx = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: entry.user_id.clone(),
            email: None,
            groups: groups.to_vec(),
            roles: vec![],
            tenant_id: Some(entry.tenant_id.clone()),
        },
        org,
        entry.tenant_id.clone(),
    );
    if let Err(e) = deps.data.notify_authz_changed(&ctx, None).await {
        tracing::warn!(error = %e, "照合: SSE へのロール変更の通知に失敗（購読は TTL で引き直す）");
    }
}

/// 続きの位置（最後に照合を終えた人）。無ければ頭から。
async fn load_cursor(db: &PgPool) -> Result<Option<DirectoryEntry>, sqlx::Error> {
    sqlx::query_as("SELECT tenant_id, user_id FROM membership_reconcile_cursor WHERE id = 1")
        .fetch_optional(db)
        .await
}

async fn save_cursor(db: &PgPool, last: &DirectoryEntry) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO membership_reconcile_cursor (id, tenant_id, user_id) VALUES (1, $1, $2) \
         ON CONFLICT (id) DO UPDATE SET tenant_id = excluded.tenant_id, \
           user_id = excluded.user_id, updated_at = now()",
    )
    .bind(&last.tenant_id)
    .bind(&last.user_id)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{cursor_move, CursorMove};

    #[test]
    fn cursor_moves_past_finished_users_and_rewinds_at_the_end() {
        // 頼んだ数より少なく返り、すべて終えた: 最後まで読んだので頭から。空のページも同じ。
        assert_eq!(cursor_move(3, 500, 3), CursorMove::Rewind);
        assert_eq!(cursor_move(0, 500, 0), CursorMove::Rewind);
        // 頼んだ数だけ返った: 続きがあるかもしれないので、最後の人の次から（次の回が空なら頭へ）。
        assert_eq!(cursor_move(500, 500, 500), CursorMove::After(499));
        // 途中で止めた（Keycloak に届かない）: 終えた人の次から。1 人も終えていなければ動かさない。
        assert_eq!(cursor_move(3, 500, 1), CursorMove::After(0));
        assert_eq!(cursor_move(3, 500, 0), CursorMove::Stay);
    }
}
