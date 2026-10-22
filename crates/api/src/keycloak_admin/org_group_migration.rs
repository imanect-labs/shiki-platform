//! テナント無修飾の旧 org group（`/acme`・`/acme/eng`）を `/tenants/<tenant>/...` へ移す（#811）。
//!
//! `shiki-admin migrate-org-groups` の本体。読み取り（[`snapshot`]）→ 計画（[`plan_tree`]・純粋関数）→
//! 適用（[`apply_tree`]）に分け、dry-run は計画の報告までで止める。
//!
//! - 旧 group の subtree（部署を含む）を、移行先テナントごとに `/tenants/<tenant>/<org>/...` へ複製する。
//! - 各メンバーはテナント（multi は `tenant` 属性・single は設定の固定値）に従って同じ相対位置の
//!   新 group へ移し、旧 group から外す。テナントを決められないメンバーは動かさず報告する。
//! - 全メンバーを移せた旧 group は subtree ごと削除する。1 人でも残れば保持する。
//! - 旧 group の role 割り当て・属性・それを参照するマッパーは**コピーしない**（テナントをまたいで
//!   効いていた権限的意味を黙って引き継がない）。件数だけ報告し、必要なら手で付け直す。
//! - 全段冪等: 作成・追加・削除はいずれも既存/不在を成功に倒すので、途中失敗は再実行で収束する。

use std::collections::{BTreeMap, BTreeSet};

use authz::org_group::TENANTS_GROUP;

use super::{KeycloakAdmin, KeycloakAdminError};

/// 移行先テナントの決め方（テナンシーモードの写し）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantRule {
    /// multi: ユーザーの `tenant` 属性（ログイン時の claim `tenant` と同じ取得元）。
    Attribute,
    /// single: 設定 `auth.tenant_id` の固定値（全員が同じテナントに解決される）。
    Fixed(String),
}

/// 旧 group のメンバー（必要フィールドのみ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyMember {
    pub user_id: String,
    pub username: String,
    /// `tenant` 属性の先頭値。
    pub tenant: Option<String>,
}

/// 旧 group subtree の 1 ノード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyNode {
    pub group_id: String,
    /// トップレベルからの名前の列（`["acme", "eng"]`）。
    pub segments: Vec<String>,
    pub members: Vec<LegacyMember>,
    /// 直接付いた role 割り当て（realm ＋ client）の件数。
    pub role_mappings: usize,
    /// 属性のキー数。
    pub attributes: usize,
}

/// テナント無修飾のトップレベル group 1 つ分（＝旧 org とその部署）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyTree {
    pub top_id: String,
    pub org: String,
    /// 先頭がトップレベル自身。親は子より先に並ぶ。
    pub nodes: Vec<LegacyNode>,
}

/// 1 メンバーの移動。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub user_id: String,
    pub username: String,
    pub from_group_id: String,
    pub tenant: String,
    pub segments: Vec<String>,
}

/// 旧 group 1 つ分の移行計画。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreePlan {
    pub top_id: String,
    pub org: String,
    /// 複製先テナント（昇順）。
    pub targets: Vec<String>,
    /// 用意する group（テナント, `/tenants/<tenant>` からの名前の列）。親が子より先。
    pub create: Vec<(String, Vec<String>)>,
    pub moves: Vec<Move>,
    /// 動かせないメンバー（`<username> (<旧パス>): <理由>`）。
    pub unresolved: Vec<String>,
    /// 旧 subtree を削除するか（全員を移せて、複製先が 1 つ以上あるとき）。
    pub delete_old: bool,
    /// コピーしない role 割り当て・属性の件数（subtree 合計）。
    pub role_mappings: usize,
    pub attributes: usize,
}

impl TreePlan {
    /// dry-run / 実行時の報告行。
    pub fn report(&self) -> Vec<String> {
        let mut out = vec![format!(
            "/{org} → {targets}（group {groups} 個・メンバー移動 {moves} 件）",
            org = self.org,
            targets = if self.targets.is_empty() {
                "複製先なし".to_string()
            } else {
                self.targets
                    .iter()
                    .map(|t| format!("/{TENANTS_GROUP}/{t}/{}", self.org))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            groups = self.create.len(),
            moves = self.moves.len(),
        )];
        for u in &self.unresolved {
            out.push(format!("  ⚠️ 移せないメンバー: {u}"));
        }
        if self.role_mappings > 0 || self.attributes > 0 {
            out.push(format!(
                "  ⚠️ 旧 group の role 割り当て {} 件・属性 {} 件はコピーしない（必要なら新 group へ手で付け直す）",
                self.role_mappings, self.attributes
            ));
        }
        out.push(if self.delete_old {
            format!("  旧 /{} は移行後に subtree ごと削除する", self.org)
        } else if self.targets.is_empty() {
            format!(
                "  旧 /{} は保持する（メンバーも登録テナントも無く、複製先を決められない）",
                self.org
            )
        } else {
            format!("  旧 /{} は保持する（移せないメンバーが残る）", self.org)
        });
        out
    }
}

/// 適用結果の件数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub groups_ensured: usize,
    pub members_moved: usize,
    pub old_deleted: bool,
}

/// realm からテナント無修飾の旧 group（`/tenants` 以外のトップレベル）を subtree ごと読む。
pub async fn snapshot(kc: &KeycloakAdmin<'_>) -> Result<Vec<LegacyTree>, KeycloakAdminError> {
    let mut trees = Vec::new();
    for top in kc.list_top_level_groups().await? {
        if top.name == TENANTS_GROUP {
            continue;
        }
        let mut nodes = Vec::new();
        // 深さ優先（親を子より先に積む）。
        let mut stack = vec![(top.clone(), vec![top.name.clone()])];
        while let Some((group, segments)) = stack.pop() {
            let members = kc
                .group_members(&group.id)
                .await?
                .into_iter()
                .map(|u| LegacyMember {
                    tenant: u.tenant().map(str::to_string),
                    user_id: u.id,
                    username: u.username,
                })
                .collect();
            if group.sub_group_count != Some(0) {
                for child in kc.list_child_groups(&group.id).await?.into_iter().rev() {
                    let mut child_segments = segments.clone();
                    child_segments.push(child.name.clone());
                    stack.push((child, child_segments));
                }
            }
            nodes.push(LegacyNode {
                role_mappings: group.role_mapping_count(),
                attributes: group.attributes.len(),
                group_id: group.id,
                segments,
                members,
            });
        }
        trees.push(LegacyTree {
            top_id: top.id,
            org: top.name,
            nodes,
        });
    }
    Ok(trees)
}

/// 旧 group 1 つ分の移行計画を立てる（純粋関数）。
///
/// `registered` は同じ org slug を使う登録済みテナント（tenant 登録簿・multi のみ使う）。
/// メンバーの居ない旧 group も、登録テナントへは複製する（作成時に用意された空の org group）。
pub fn plan_tree(tree: &LegacyTree, rule: &TenantRule, registered: &[String]) -> TreePlan {
    let mut targets: BTreeSet<String> = match rule {
        TenantRule::Fixed(t) => BTreeSet::from([t.clone()]),
        TenantRule::Attribute => registered.iter().cloned().collect(),
    };
    let mut moves = Vec::new();
    let mut unresolved = Vec::new();
    for node in &tree.nodes {
        for m in &node.members {
            let tenant = match rule {
                TenantRule::Fixed(t) => Ok(t.clone()),
                TenantRule::Attribute => member_tenant(m),
            };
            match tenant {
                Ok(tenant) => {
                    targets.insert(tenant.clone());
                    moves.push(Move {
                        user_id: m.user_id.clone(),
                        username: m.username.clone(),
                        from_group_id: node.group_id.clone(),
                        tenant,
                        segments: node.segments.clone(),
                    });
                }
                Err(reason) => unresolved.push(format!(
                    "{} (/{}): {reason}",
                    m.username,
                    node.segments.join("/")
                )),
            }
        }
    }
    // 親が子より先に並ぶ（nodes の順）ので、テナントごとにその順で用意する。
    let create = targets
        .iter()
        .flat_map(|t| {
            tree.nodes
                .iter()
                .map(move |n| (t.clone(), n.segments.clone()))
        })
        .collect();
    let delete_old = unresolved.is_empty() && !targets.is_empty();
    TreePlan {
        top_id: tree.top_id.clone(),
        org: tree.org.clone(),
        targets: targets.into_iter().collect(),
        create,
        moves,
        unresolved,
        delete_old,
        role_mappings: tree.nodes.iter().map(|n| n.role_mappings).sum(),
        attributes: tree.nodes.iter().map(|n| n.attributes).sum(),
    }
}

/// メンバーの `tenant` 属性を移行先テナントとして検証する（ログイン時と同じ文字ポリシー）。
fn member_tenant(m: &LegacyMember) -> Result<String, &'static str> {
    let tenant = m
        .tenant
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or("tenant 属性が無い")?;
    authz::validate_tenant_id(tenant).map_err(|_| "tenant 属性が識別子として不正")?;
    Ok(tenant.to_string())
}

/// 計画を適用する（group 用意 → メンバー移動 → 旧 subtree 削除）。全段冪等。
pub async fn apply_tree(
    kc: &KeycloakAdmin<'_>,
    plan: &TreePlan,
) -> Result<ApplyStats, KeycloakAdminError> {
    let mut stats = ApplyStats::default();
    let mut ids: BTreeMap<(String, Vec<String>), String> = BTreeMap::new();
    for (tenant, segments) in &plan.create {
        let mut path: Vec<&str> = vec![TENANTS_GROUP, tenant];
        path.extend(segments.iter().map(String::as_str));
        let id = kc.ensure_group_path(&path).await?;
        ids.insert((tenant.clone(), segments.clone()), id);
        stats.groups_ensured += 1;
    }
    for mv in &plan.moves {
        let target = ids
            .get(&(mv.tenant.clone(), mv.segments.clone()))
            .ok_or_else(|| KeycloakAdminError::Status {
                status: 500,
                body: format!("移行先 group が計画にありません: {}", mv.segments.join("/")),
            })?;
        // 先に新 group へ入れてから旧 group を外す（途中失敗でも所属が消えない）。
        kc.add_user_to_group(&mv.user_id, target).await?;
        kc.remove_user_from_group(&mv.user_id, &mv.from_group_id)
            .await?;
        stats.members_moved += 1;
    }
    if plan.delete_old {
        kc.delete_group(&plan.top_id).await?;
        stats.old_deleted = true;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, tenant: Option<&str>) -> LegacyMember {
        LegacyMember {
            user_id: id.into(),
            username: format!("{id}-name"),
            tenant: tenant.map(str::to_string),
        }
    }

    fn node(id: &str, segments: &[&str], members: Vec<LegacyMember>) -> LegacyNode {
        LegacyNode {
            group_id: id.into(),
            segments: segments.iter().map(|s| s.to_string()).collect(),
            members,
            role_mappings: 0,
            attributes: 0,
        }
    }

    fn segs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn acme_tree() -> LegacyTree {
        LegacyTree {
            top_id: "g-acme".into(),
            org: "acme".into(),
            nodes: vec![
                node("g-acme", &["acme"], vec![member("u1", Some("t1"))]),
                node(
                    "g-eng",
                    &["acme", "eng"],
                    vec![member("u2", Some("t2")), member("u1", Some("t1"))],
                ),
            ],
        }
    }

    #[test]
    fn replicates_subtree_per_member_tenant() {
        // 同じ slug `/acme` を 2 テナントが共有していた場合、テナントごとに subtree を複製し、
        // 各メンバーは自分のテナント側の同じ相対位置へ移る。
        let plan = plan_tree(&acme_tree(), &TenantRule::Attribute, &[]);
        assert_eq!(plan.targets, segs(&["t1", "t2"]));
        assert_eq!(
            plan.create,
            vec![
                ("t1".to_string(), segs(&["acme"])),
                ("t1".to_string(), segs(&["acme", "eng"])),
                ("t2".to_string(), segs(&["acme"])),
                ("t2".to_string(), segs(&["acme", "eng"])),
            ]
        );
        let moved: Vec<(&str, &str, String)> = plan
            .moves
            .iter()
            .map(|m| (m.user_id.as_str(), m.tenant.as_str(), m.segments.join("/")))
            .collect();
        assert_eq!(
            moved,
            vec![
                ("u1", "t1", "acme".to_string()),
                ("u2", "t2", "acme/eng".to_string()),
                ("u1", "t1", "acme/eng".to_string()),
            ]
        );
        assert!(plan.unresolved.is_empty());
        assert!(plan.delete_old);
    }

    #[test]
    fn member_without_valid_tenant_keeps_old_group() {
        // tenant 属性が無い・不正なメンバーは動かさず、旧 group を残す（黙って別テナントへ寄せない）。
        let mut tree = acme_tree();
        tree.nodes[1].members.push(member("u3", None));
        tree.nodes[1].members.push(member("u4", Some("bad|tenant")));
        let plan = plan_tree(&tree, &TenantRule::Attribute, &[]);
        assert_eq!(plan.moves.len(), 3);
        assert_eq!(plan.unresolved.len(), 2);
        assert!(plan.unresolved[0].starts_with("u3-name (/acme/eng)"));
        assert!(!plan.delete_old);
        assert!(plan
            .report()
            .iter()
            .any(|l| l.contains("移せないメンバーが残る")));
    }

    #[test]
    fn fixed_tenant_moves_everyone_to_it() {
        // single: 属性に関わらず全員が設定の固定テナントへ（ログイン時の解決と同じ）。
        let mut tree = acme_tree();
        tree.nodes[1].members.push(member("u3", None));
        let plan = plan_tree(&tree, &TenantRule::Fixed("default".into()), &["x".into()]);
        assert_eq!(plan.targets, segs(&["default"]));
        assert!(plan.moves.iter().all(|m| m.tenant == "default"));
        assert_eq!(plan.moves.len(), 4);
        assert!(plan.delete_old);
    }

    #[test]
    fn empty_group_goes_to_registered_tenants_or_is_kept() {
        let tree = LegacyTree {
            top_id: "g-empty".into(),
            org: "empty".into(),
            nodes: vec![node("g-empty", &["empty"], vec![])],
        };
        // 登録簿に同 slug のテナントがあれば、その配下へ空の org group を用意して旧 group を消す。
        let plan = plan_tree(&tree, &TenantRule::Attribute, &["t9".into()]);
        assert_eq!(plan.create, vec![("t9".to_string(), segs(&["empty"]))]);
        assert!(plan.delete_old);
        // どこにも紐づかない空 group は複製先を決められないので保持する。
        let orphan = plan_tree(&tree, &TenantRule::Attribute, &[]);
        assert!(orphan.create.is_empty());
        assert!(!orphan.delete_old);
        assert!(orphan
            .report()
            .iter()
            .any(|l| l.contains("複製先を決められない")));
    }

    #[test]
    fn report_counts_uncopied_role_mappings() {
        let mut tree = acme_tree();
        tree.nodes[0].role_mappings = 2;
        tree.nodes[1].attributes = 1;
        let plan = plan_tree(&tree, &TenantRule::Attribute, &[]);
        assert_eq!((plan.role_mappings, plan.attributes), (2, 1));
        let report = plan.report();
        assert!(report[0].contains("/tenants/t1/acme, /tenants/t2/acme"));
        assert!(report
            .iter()
            .any(|l| l.contains("role 割り当て 2 件・属性 1 件")));
        assert!(report.iter().any(|l| l.contains("subtree ごと削除")));
    }
}
