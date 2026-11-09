//! ロール・部署の member の直接タプル（`role:<tenant>|<id>#member@user:<tenant>|<sub>`）を読む・外す手順と、
//! member であるべきロールの決め方（[`desired_roles`]）。
//!
//! ログインとセッションの更新のロールの同期（[`super::callback::provision_roles`]）と、Keycloak の所属グループとの
//! 定期の照合（[`crate::membership_reconcile`]・D-136・D-140）が同じ手順と同じ決め方を使う（写しを作らない）。
//! どれを外すかの候補は呼ぶ側が決める（ログインの同期は今の直接タプルのすべて、照合はログインの同期が記録したもの）。

use authz::{AuthContext, AuthzClient, GroupLayout, Namespace, ObjectType, Relation, Subject};

use crate::extract::org_of_groups;

/// member であるべきロール（[`desired_roles`] の結果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesiredRoles {
    /// 所属グループから決まる組織（決まらなければ既定の組織・`AuthContext` と同じ規則）。
    pub org: String,
    /// ロールの id と、所属グループ由来（部署）か。同じ id が属性 `roles` と所属グループの両方にあれば属性の方。
    pub roles: Vec<(String, bool)>,
}

impl DesiredRoles {
    /// このロールの member であるべきか。
    pub(crate) fn contains(&self, role: &str) -> bool {
        self.roles.iter().any(|(id, _)| id == role)
    }
}

/// 属性 `roles`（ロールの claim の元）と所属グループ（`groups`）から、member であるべきロールを決める
/// （ログインの同期と定期の照合の単一定義）。
///
/// - 部署は自テナント配下の所属グループだけ（`/tenants/<tenant>/<org>/<dept>` → `<org>/<dept>`・#811）。他テナント
///   配下・（multi では）テナント無修飾のグループはロールにしない。**組織そのもののグループ（`<org>`）は除く**
///   （組織は organization のタプルの責務）。属性 `roles` に組織と同じ名前のロールがあっても、それは正当なロール
///   なので除かない（除くと同期が誤って外す）。
/// - 識別子を壊す値（空・FGA の構造文字・制御文字）は [`authz::validate_local_id`]（文字の決まりの単一定義・
///   #91 M-3）で除く。
pub(crate) fn desired_roles(
    roles: &[String],
    groups: &[String],
    tenant_id: &str,
    layout: GroupLayout,
) -> DesiredRoles {
    let org = org_of_groups(groups, tenant_id, layout);
    let departments = authz::org_group::tenant_relative_groups(groups, tenant_id, layout);
    let roles = role_sources(roles, &departments, &org);
    DesiredRoles { org, roles }
}

/// [`desired_roles`] の本体（部署はテナントからの相対パスにしたもの）。
pub(crate) fn role_sources(
    roles: &[String],
    departments: &[String],
    org: &str,
) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let candidates = roles
        .iter()
        .map(|r| (r.trim().to_string(), false))
        .chain(departments.iter().map(|d| (d.clone(), true)));
    for (id, from_group) in candidates {
        let id = id.trim();
        if (from_group && id == org) || authz::validate_local_id(id).is_err() {
            continue;
        }
        if !out.iter().any(|(existing, _)| existing == id) {
            out.push((id.to_string(), from_group));
        }
    }
    out
}

/// 利用者が直接タプルを持つこのテナントのロール（継承は展開しない・他テナントのタプルは除く）。
///
/// 読めないときは `None`（呼ぶ側は外さない。未知の状態に破壊的な操作をしない）。
pub(crate) async fn current_roles(
    authz: &dyn AuthzClient,
    subject: &Subject,
    ns: &Namespace<'_>,
) -> Option<Vec<String>> {
    match authz.read_subject_objects(subject, ObjectType::Role).await {
        Ok(objects) => Some(
            objects
                .iter()
                .filter_map(|raw| raw.strip_prefix("role:"))
                .filter_map(|id_part| ns.strip_object_id(id_part))
                .map(str::to_string)
                .collect(),
        ),
        Err(e) => {
            tracing::warn!(error = %e, "ロールの直接タプルを読めません（外すのは次の同期に回す）");
            None
        }
    }
}

/// `stale` のロールの member を外し、実際に外したロールを返す（無いものは no-op・失敗は次の同期に任せる）。
pub(crate) async fn revoke_roles(
    authz: &dyn AuthzClient,
    subject: &Subject,
    ns: &Namespace<'_>,
    stale: &[String],
) -> Vec<String> {
    let mut revoked = Vec::new();
    for role in stale {
        match authz
            .delete_tuple(subject, Relation::Member, &ns.role(role))
            .await
        {
            Ok(true) => {
                tracing::info!(%role, "ロールの member を外した（所属から消えた）");
                revoked.push(role.clone());
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(%role, error = %e, "ロールの member を外せません（次の同期で外し直す）");
            }
        }
    }
    revoked
}

/// ロールの直接タプルと台帳（`directory_role`）を、あるべき集合（`desired`・claims 由来）にそろえる（ログインと
/// セッションの更新の同期の本体・[`super::callback::provision_roles`]）。タプルを変えたかを返す。
///
/// 現在の直接タプル（自テナント名前空間のみ・継承展開なし）が読めない時は**剥奪をスキップ**し付与のみ行う（誤剥奪を
/// 避ける fail-safe）。
///
/// 剥奪は 現在 − あるべき。**claims を正本とする**（callback.rs 冒頭の設計）ため、claims が空＝ロール無しと解釈し
/// stale タプルを剥奪する。IdP でユーザーの最後の role/group が外れたケース（desired 空）で過剰権限を残さないため
/// （剥奪漏れ＝fail-open を避ける・#91 P1 レビュー対応）。過剰剥奪は fail-safe（アクセス減・次回の正しいログインで
/// 再付与）で自己回復する。一過性の IdP 不調そのものは、現在集合の READ が失敗した時のみ剥奪をスキップする上の
/// ガード（unknown state に対して破壊的操作をしない）で扱い、READ 成功時の空 claims は正当な「ロール無し」として扱う。
pub(crate) async fn sync_roles(
    authz: &dyn AuthzClient,
    directory: &storage::DirectoryStore,
    ctx: &AuthContext,
    desired: &[String],
) -> bool {
    let (subject, ns) = (ctx.subject(), ctx.ns());
    let current: Vec<String> = current_roles(authz, &subject, &ns)
        .await
        .unwrap_or_default();
    let stale: Vec<String> = current
        .iter()
        .filter(|c| !desired.contains(*c))
        .cloned()
        .collect();
    let revoked = !revoke_roles(authz, &subject, &ns, &stale).await.is_empty();
    let granted = grant_roles(authz, directory, ctx, desired, &current).await;
    revoked || granted
}

/// あるべきロールの member を付け、台帳（`directory_role`）をそろえる。タプルを足したかを返す（付与は あるべき − 現在。
/// 冪等 write なので差分に限らず流しても安全だが、無駄打ちを避ける）。
///
/// - 今タプルがある（`current` にある）ロールも、台帳の行が欠けていれば戻す（BUG-270）。台帳は共有の相手検索・ロール
///   の列の参照の照合・テナント撤去の列挙（`purge_tenant`）が読むので、タプルだけが残ると、ロールを書けず撤去からも
///   漏れる。行があれば何も変えない（表示名・作成日時を保つ）。
/// - 今タプルが無いロールは、台帳を先に書いてからタプルを足す（#91 M-4）。タプルを先に書くと、台帳の書き込みに
///   失敗したとき「タプルはあるが台帳に無いロール」ができ、撤去の列挙から漏れる。台帳を書けなければタプルを
///   足さない（次の同期で試し直す）。
async fn grant_roles(
    authz: &dyn AuthzClient,
    directory: &storage::DirectoryStore,
    ctx: &AuthContext,
    desired: &[String],
    current: &[String],
) -> bool {
    let (tenant_id, org) = (ctx.tenant_id(), ctx.org());
    let held: Vec<String> = desired
        .iter()
        .filter(|d| current.contains(*d))
        .cloned()
        .collect();
    match directory.ensure_roles(tenant_id, org, &held).await {
        Ok(0) => {}
        Ok(restored) => {
            tracing::info!(
                restored,
                "directory_role に欠けていたロールの行を戻した（タプルはあった）"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "directory_role の欠けを戻せません（次回同期で再試行）");
        }
    }
    let (subject, ns) = (ctx.subject(), ctx.ns());
    let mut granted = false;
    for role_id in desired.iter().filter(|d| !current.contains(*d)) {
        if let Err(e) = directory
            .upsert_role(role_id, tenant_id, org, role_id)
            .await
        {
            tracing::warn!(role = %role_id, error = %e, "directory_role の upsert に失敗（タプル付与を保留・次回同期で再試行）");
            continue;
        }
        match authz
            .write_tuple(&subject, Relation::Member, &ns.role(role_id))
            .await
        {
            Ok(_) => granted = true,
            Err(e) => {
                tracing::warn!(role = %role_id, error = %e, "role 付与に失敗（best-effort・login は継続）");
            }
        }
    }
    granted
}
