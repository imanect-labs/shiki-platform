//! ロール・部署の member の直接タプル（`role:<tenant>|<id>#member@user:<tenant>|<sub>`）を読む・外す手順。
//!
//! ログインとセッションの更新のロールの同期（[`super::callback::provision_roles`]）と、Keycloak の所属グループとの
//! 定期の照合（[`crate::membership_reconcile`]・D-136）が同じ手順を使う（写しを作らない）。何を外すかは呼ぶ側が
//! 決める。

use authz::{AuthzClient, Namespace, ObjectType, Relation, Subject};

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
