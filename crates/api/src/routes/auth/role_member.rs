//! ロール・部署の member の直接タプル（`role:<tenant>|<id>#member@user:<tenant>|<sub>`）を読む・外す手順と、
//! member であるべきロールの決め方（[`desired_roles`]）。
//!
//! ログインとセッションの更新のロールの同期（[`super::callback::provision_roles`]）と、Keycloak の所属グループとの
//! 定期の照合（[`crate::membership_reconcile`]・D-136・D-140）が同じ手順と同じ決め方を使う（写しを作らない）。
//! どれを外すかの候補は呼ぶ側が決める（ログインの同期は今の直接タプルのすべて、照合はログインの同期が記録したもの）。

use authz::{AuthzClient, GroupLayout, Namespace, ObjectType, Relation, Subject};

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
