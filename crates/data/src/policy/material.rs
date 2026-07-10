//! 述語材料の解決（PIT-18・Task 9.3）。
//!
//! `$user.roles` や個別共有 ID 集合は OpenFGA 側にあり、行データは Postgres にある。
//! 1 クエリごとに全展開すると `IN(数千)` が肥大するため（PIT-1 と同型の
//! カーディナリティ問題）、次で抑える:
//!
//! - **上限**: ロール集合 [`MAX_ROLE_SET`]（超過は fail-closed エラー＝設定異常として拒否）、
//!   共有 ID [`MAX_SHARED_IDS`]（超過分は**落とす**＝可視が減る方向・`shares_truncated` で通知）。
//! - SQL 側は `= ANY($n::text[]/uuid[])` の**配列バインド**で渡し、SQL テキストは肥大しない。
//!
//! # キャッシュしない理由（Codex #6/#7/#8）
//!
//! 材料は FGA 由来の**認可判定**そのものであり、権限剥奪・ロール変更・共有解除は
//! **即時に反映されねばならない**。プロセスローカルなキャッシュはマルチレプリカで
//! 撤回が TTL 分だけ遅延し（confused-deputy 化）、principal 種別を跨いだ取り違えも招く。
//! よって毎クエリ FGA を引く（`HigherConsistency` 相当の即時性）。IN 肥大は上限＋配列
//! バインドで別途抑えており、round-trip 削減が必要になった時点で cross-replica 無効化
//! （Redis pub/sub 等）を伴うキャッシュを別途設計する。

use authz::{AuthContext, AuthzClient, ObjectType, Relation};
use std::sync::Arc;
use uuid::Uuid;

use crate::policy::ast::{PolicyExpr, PolicyOperand};
use crate::DataError;

/// ロール集合（直接/実効）の上限。超過は fail-closed（クエリ全体をエラー）。
pub(crate) const MAX_ROLE_SET: usize = 1_000;
/// 個別共有 ID 集合の上限。超過分は切り詰め（可視減方向）＋ `shares_truncated`。
pub(crate) const MAX_SHARED_IDS: usize = 10_000;

/// 解決済みの述語材料（1 リクエスト分・コンパイルは同期でこれを参照する）。
#[derive(Debug, Default)]
pub(crate) struct PolicyMaterial {
    /// `$user.roles`（直接タプルのみ・subtree=false）。
    pub roles_direct: Option<Arc<Vec<String>>>,
    /// `$user.roles`（継承展開済みの実効集合・subtree=true）。
    pub roles_effective: Option<Arc<Vec<String>>>,
    /// 個別共有された data_record の local id 集合（viewer の実効集合＝role 共有も含む）。
    pub shared_record_ids: Vec<Uuid>,
    /// 共有集合が上限で切り詰められたか（応答へ伝搬し監査に残す）。
    pub shares_truncated: bool,
}

impl PolicyMaterial {
    /// HasRole / UserRoles の判定用ロール集合。
    pub(crate) fn roles(&self, subtree: bool) -> &[String] {
        let set = if subtree {
            self.roles_effective.as_ref()
        } else {
            self.roles_direct.as_ref()
        };
        set.map_or(&[][..], |v| v.as_slice())
    }
}

/// AST が必要とする材料の解析結果。
#[derive(Debug, Default, Clone, Copy)]
struct Needs {
    roles_direct: bool,
    roles_effective: bool,
}

fn analyze(expr: &PolicyExpr, needs: &mut Needs) {
    match expr {
        PolicyExpr::Any(children) | PolicyExpr::All(children) => {
            for c in children {
                analyze(c, needs);
            }
        }
        PolicyExpr::HasRole { subtree, .. }
        | PolicyExpr::FieldCmp {
            value: PolicyOperand::UserRoles { subtree },
            ..
        } => {
            if *subtree {
                needs.roles_effective = true;
            } else {
                needs.roles_direct = true;
            }
        }
        _ => {}
    }
}

/// 述語が要求する材料を FGA から解決する（キャッシュなし・毎回最新）。
///
/// `exprs` はこのクエリで合成される全式（read または write）。共有 ID 集合は式に依らず
/// 常に解決する（`OR id = ANY(共有)` の枝が必ず付くため）。
pub(crate) async fn resolve(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    exprs: &[&PolicyExpr],
) -> Result<PolicyMaterial, DataError> {
    let mut needs = Needs::default();
    for e in exprs {
        analyze(e, &mut needs);
    }

    let mut material = PolicyMaterial::default();
    if needs.roles_direct {
        material.roles_direct = Some(Arc::new(role_set(ctx, authz, false).await?));
    }
    if needs.roles_effective {
        material.roles_effective = Some(Arc::new(role_set(ctx, authz, true).await?));
    }

    // 個別共有集合は**実効集合**で引く（`list_objects` は role 共有も継承展開する・Codex #3）。
    let shared = authz
        .list_objects(&ctx.subject(), Relation::Viewer, ObjectType::DataRecord)
        .await
        .map_err(|e| DataError::Internal(format!("authz: {e}")))?;
    let ns = ctx.ns();
    let mut ids: Vec<Uuid> = Vec::with_capacity(shared.len().min(MAX_SHARED_IDS));
    for o in shared {
        let Some((_, id_part)) = o.split_once(':') else {
            continue;
        };
        let Some(local) = ns.strip_object_id(id_part) else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(local) else {
            continue;
        };
        if ids.len() >= MAX_SHARED_IDS {
            material.shares_truncated = true;
            tracing::warn!(
                user = %ctx.principal.id,
                limit = MAX_SHARED_IDS,
                "個別共有集合が上限を超過（超過分は不可視・PIT-18 fail-closed）"
            );
            break;
        }
        ids.push(id);
    }
    material.shared_record_ids = ids;
    Ok(material)
}

/// ロール集合（direct=read_subject_objects / effective=list_objects）を上限付きで解決する。
async fn role_set(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    effective: bool,
) -> Result<Vec<String>, DataError> {
    let raw = if effective {
        authz
            .list_objects(&ctx.subject(), Relation::Member, ObjectType::Role)
            .await
    } else {
        authz
            .read_subject_objects(&ctx.subject(), ObjectType::Role)
            .await
    }
    .map_err(|e| DataError::Internal(format!("authz: {e}")))?;
    if raw.len() > MAX_ROLE_SET {
        // ロール爆発は述語の部分適用（見えすぎ/見えなさすぎの不定）を招くため
        // fail-closed でクエリごと拒否する（PIT-18: 閾値超のフォールバックを仕様化）。
        return Err(DataError::Invalid(format!(
            "実行主体のロール集合が上限（{MAX_ROLE_SET}）を超えています。管理者にロール構成の見直しを依頼してください"
        )));
    }
    let ns = ctx.ns();
    let mut roles = Vec::with_capacity(raw.len());
    for o in raw {
        let Some((_, id_part)) = o.split_once(':') else {
            continue;
        };
        if let Some(local) = ns.strip_object_id(id_part) {
            roles.push(local.to_string());
        }
    }
    Ok(roles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ast::CmpOp;

    #[test]
    fn analyze_detects_role_needs() {
        let mut needs = Needs::default();
        analyze(
            &PolicyExpr::Any(vec![
                PolicyExpr::HasRole {
                    role: "a".into(),
                    subtree: true,
                },
                PolicyExpr::FieldCmp {
                    field: "dept".into(),
                    op: CmpOp::In,
                    value: PolicyOperand::UserRoles { subtree: false },
                },
            ]),
            &mut needs,
        );
        assert!(needs.roles_effective);
        assert!(needs.roles_direct);
    }
}
