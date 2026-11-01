//! 述語材料の解決（PIT-18・Task 9.3）。
//!
//! `$user.roles` や個別共有 ID 集合は OpenFGA 側にあり、行データは Postgres にある。
//! 1 クエリごとに全展開すると `IN(数千)` が肥大するため（PIT-1 と同型の
//! カーディナリティ問題）、次で抑える:
//!
//! - **上限**: ロール集合 [`MAX_ROLE_SET`]（超過は fail-closed エラー＝設定異常として拒否）、
//!   共有 ID [`MAX_SHARED_IDS`]（超過分は**落とす**＝可視が減る方向・`shares_truncated` で通知）。
//! - **ListObjects の応答の上限**（[`authz::visible::LIST_OBJECTS_MAX_RESULTS`]・BUG-161）: 実効集合の応答は上限で
//!   黙って切れる。ロールの実効集合は上限に届いたら超過と同じく fail-closed。共有 ID は本人と所属ロールの直接タプルを
//!   Read API（件数の上限が無い）で読み直して足す（[`shared_ids`]）。
//! - SQL 側は `= ANY($n::text[]/uuid[])` の**配列バインド**で渡し、SQL テキストは肥大しない。
//!
//! # リクエストを跨いでキャッシュしない（PIT-18・Codex #6/#7/#8）
//!
//! 材料は FGA 由来の**認可判定**そのものであり、権限剥奪・ロール変更・共有解除は
//! **即時に反映されねばならない**。プロセスローカルなキャッシュはマルチレプリカで
//! 撤回が TTL 分だけ遅延し（confused-deputy 化）、principal 種別を跨いだ取り違えも招く。
//! よってリクエストごとに FGA を引く（`HigherConsistency`）。
//!
//! 同一リクエスト内では [`AuthzMemo`] が答えを覚える（Task 15.9・data-platform §16.4）。メモの
//! キーは主体（`PrincipalKind` / principal id / `org` / `tenant_id`）＋式集合＋共有集合の要否で、
//! 生存範囲はリクエストに閉じる（型で外へ持ち出せない・[`super::memo`]）。
//!
//! # ロールの解決
//!
//! - `has_role(subtree=true)` は**名指しのロールだけ**を BatchCheck（`role#member`）で判定する。
//!   `list_objects(Role)` の実効集合に含まれるかと同じ問いで、集合の上限も切り詰めも無い。
//! - `has_role(subtree=false)`（直接所属）と `$user.roles`（集合そのものが要る）は従来どおり
//!   集合で引き、[`MAX_ROLE_SET`] 超過は fail-closed。

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use authz::{AuthContext, FgaObject, ObjectType, Relation};
use uuid::Uuid;

use crate::policy::ast::{PolicyExpr, PolicyOperand};
use crate::policy::memo::{AuthzMemo, MaterialKey};
use crate::DataError;

/// ロール集合（直接/実効）の上限。超過は fail-closed（クエリ全体をエラー）。
pub(crate) const MAX_ROLE_SET: usize = 1_000;
/// 個別共有 ID 集合の上限。超過分は切り詰め（可視減方向）＋ `shares_truncated`。
pub(crate) const MAX_SHARED_IDS: usize = 10_000;
/// OpenFGA の object 識別子の最大長（`^[^\s]{2,256}$`）。
const MAX_FGA_OBJECT_LEN: usize = 256;

/// 解決済みの述語材料（1 リクエスト分・コンパイルは同期でこれを参照する）。
#[derive(Debug, Default)]
pub(crate) struct PolicyMaterial {
    /// `$user.roles`（直接タプルのみ・subtree=false）。
    pub roles_direct: Option<Arc<Vec<String>>>,
    /// `$user.roles`（継承展開済みの実効集合・subtree=true）。`$user.roles { subtree: true }` があるときだけ引く。
    pub roles_effective: Option<Arc<Vec<String>>>,
    /// `has_role(subtree=true)` の判定（ロール名 → 配下ロール込みで member か・BatchCheck）。
    pub role_members: HashMap<String, bool>,
    /// 個別共有された data_record の local id 集合（viewer の実効集合＝role 共有も含む）。
    pub shared_record_ids: Vec<Uuid>,
    /// 共有集合が上限で切り詰められたか（応答へ伝搬し監査に残す）。
    pub shares_truncated: bool,
}

impl PolicyMaterial {
    /// `$user.roles` の集合（UserRoles の比較用）。
    pub(crate) fn roles(&self, subtree: bool) -> &[String] {
        let set = if subtree {
            self.roles_effective.as_ref()
        } else {
            self.roles_direct.as_ref()
        };
        set.map_or(&[][..], |v| v.as_slice())
    }

    /// `has_role` の判定。名指しの判定（subtree=true）があればそれ、無ければ集合に含まれるか
    /// （どちらも無ければ不成立＝fail-closed）。
    pub(crate) fn has_role(&self, role: &str, subtree: bool) -> bool {
        if subtree {
            if let Some(m) = self.role_members.get(role) {
                return *m;
            }
        }
        self.roles(subtree).iter().any(|r| r == role)
    }
}

/// AST が必要とする材料の解析結果。
#[derive(Debug, Default, Clone)]
pub(crate) struct Needs {
    roles_direct: bool,
    roles_effective: bool,
    /// `has_role(subtree=true)` の名指しロール。
    pub(crate) members: BTreeSet<String>,
}

pub(crate) fn analyze(expr: &PolicyExpr, needs: &mut Needs) {
    match expr {
        PolicyExpr::Any(children) | PolicyExpr::All(children) => {
            for c in children {
                analyze(c, needs);
            }
        }
        PolicyExpr::HasRole {
            role,
            subtree: true,
        } => {
            needs.members.insert(role.clone());
        }
        PolicyExpr::HasRole { subtree: false, .. }
        | PolicyExpr::FieldCmp {
            value: PolicyOperand::UserRoles { subtree: false },
            ..
        } => needs.roles_direct = true,
        PolicyExpr::FieldCmp {
            value: PolicyOperand::UserRoles { subtree: true },
            ..
        } => needs.roles_effective = true,
        _ => {}
    }
}

/// `role#member` を問える識別子か。OpenFGA が object として受け付けない形（`:` / `#` / 空白・制御文字・
/// 長すぎる）はタプルを書けない＝誰も member でないので、問わずに不成立とする（BatchCheck は
/// 1 問の形式違反で全体を拒否するため、ここで弾く）。
pub(crate) fn role_object(ctx: &AuthContext, role: &str) -> Option<FgaObject> {
    if role.is_empty()
        || role
            .chars()
            .any(|c| c == ':' || c == '#' || c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let obj = ctx.ns().role(role);
    (obj.as_str().len() <= MAX_FGA_OBJECT_LEN).then_some(obj)
}

/// 行述語の材料を解決する（ロール＋個別共有集合・リクエスト内はメモ）。
///
/// `exprs` はこのクエリで合成される全式（read または write）。共有 ID 集合は式に依らず
/// 常に解決する（`OR id = ANY(共有)` の枝が必ず付くため）。
pub(crate) async fn resolve(
    ctx: &AuthContext,
    az: &AuthzMemo<'_>,
    exprs: &[&PolicyExpr],
) -> Result<Arc<PolicyMaterial>, DataError> {
    resolve_with(ctx, az, exprs, true).await
}

/// ロールの材料だけを解決する（フィールドマスクの `readable_by` 用・共有集合は引かない）。
pub(crate) async fn resolve_roles(
    ctx: &AuthContext,
    az: &AuthzMemo<'_>,
    exprs: &[&PolicyExpr],
) -> Result<Arc<PolicyMaterial>, DataError> {
    resolve_with(ctx, az, exprs, false).await
}

async fn resolve_with(
    ctx: &AuthContext,
    az: &AuthzMemo<'_>,
    exprs: &[&PolicyExpr],
    shares: bool,
) -> Result<Arc<PolicyMaterial>, DataError> {
    let key = MaterialKey::new(ctx, exprs, shares);
    if let Some(m) = az.material(&key) {
        return Ok(m);
    }
    let mut needs = Needs::default();
    for e in exprs {
        analyze(e, &mut needs);
    }

    let mut material = PolicyMaterial::default();
    if needs.roles_direct {
        material.roles_direct = Some(Arc::new(role_set(ctx, az, false).await?));
    }
    if needs.roles_effective {
        material.roles_effective = Some(Arc::new(role_set(ctx, az, true).await?));
    }
    material.role_members = role_members(ctx, az, &needs.members).await?;

    if shares {
        let (ids, truncated) = shared_ids(ctx, az).await?;
        material.shared_record_ids = ids;
        material.shares_truncated = truncated;
    }
    let m = Arc::new(material);
    az.put_material(key, Arc::clone(&m));
    Ok(m)
}

/// 名指しロールの member 判定（BatchCheck・メモ済みなら往復なし）。
async fn role_members(
    ctx: &AuthContext,
    az: &AuthzMemo<'_>,
    roles: &BTreeSet<String>,
) -> Result<HashMap<String, bool>, DataError> {
    let mut out = HashMap::with_capacity(roles.len());
    let mut asked: Vec<(&String, FgaObject)> = Vec::new();
    for r in roles {
        match role_object(ctx, r) {
            Some(o) => asked.push((r, o)),
            None => {
                out.insert(r.clone(), false);
            }
        }
    }
    if asked.is_empty() {
        return Ok(out);
    }
    let items: Vec<(Relation, FgaObject)> = asked
        .iter()
        .map(|(_, o)| (Relation::Member, o.clone()))
        .collect();
    let got = az.check_all(ctx, &items).await?;
    for ((r, _), m) in asked.into_iter().zip(got) {
        out.insert(r.clone(), m);
    }
    Ok(out)
}

/// 個別共有集合（**実効集合**＝role 共有も継承展開・Codex #3）を上限付きで解決する（リクエスト内はメモ）。
///
/// ListObjects の応答が上限（[`authz::visible::LIST_OBJECTS_MAX_RESULTS`]）に届いたら、その応答は全部ではない
/// （BUG-161）。`data_record` の viewer は直接のタプル（`[user, role#member]`・editor を含む）だけなので、本人と
/// 所属ロール（配下ロール込みの実効集合）それぞれの直接タプルを Read API（件数の上限が無い）で読み直して足す。
/// 所属ロールの実効集合も上限に届いたら、読み直せないロールの共有は見えない方向に倒し `shares_truncated` を立てる。
async fn shared_ids(ctx: &AuthContext, az: &AuthzMemo<'_>) -> Result<(Vec<Uuid>, bool), DataError> {
    if let Some(got) = az.shared_records(ctx) {
        return Ok(got);
    }
    let listed = az
        .list_objects(ctx, Relation::Viewer, ObjectType::DataRecord)
        .await?;
    let got = if authz::visible::list_may_be_cut(listed.len()) {
        let (direct, roles_cut) = shared_by_direct_tuples(ctx, az).await?;
        let (ids, truncated) = cap_shared_ids(ctx, listed.iter().chain(direct.iter()));
        (ids, truncated || roles_cut)
    } else {
        cap_shared_ids(ctx, listed.iter())
    };
    az.put_shared_records(ctx, got.clone());
    Ok(got)
}

/// 本人と所属ロールの `data_record` の直接タプル（`type:id`）。2 つ目は所属ロールの実効集合が上限で切れたか
/// （切れたら読み直せないロールの共有が欠ける）。
async fn shared_by_direct_tuples(
    ctx: &AuthContext,
    az: &AuthzMemo<'_>,
) -> Result<(Vec<String>, bool), DataError> {
    let ns = ctx.ns();
    let roles = az
        .list_objects(ctx, Relation::Member, ObjectType::Role)
        .await?;
    let roles_cut = authz::visible::list_may_be_cut(roles.len());
    let mut subjects = vec![ctx.subject()];
    subjects.extend(
        roles
            .iter()
            .filter_map(|o| o.split_once(':').map(|(_, id)| id))
            .filter_map(|id| ns.strip_object_id(id))
            .map(|local| ns.role_member(local)),
    );
    let read = az
        .read_direct_objects(&subjects, ObjectType::DataRecord)
        .await?;
    if roles_cut {
        tracing::warn!(
            user = %ctx.principal.id,
            roles = roles.len(),
            "所属ロールの実効集合が上限に届いたため、読み直せないロールの個別共有は見えない方向に倒す（BUG-161）"
        );
    }
    Ok((read.into_iter().flatten().collect(), roles_cut))
}

/// `data_record:<tenant>|<uuid>` の並びから、このテナントの id を重ねずに [`MAX_SHARED_IDS`] 件まで取り出す
/// （超えたら切り詰めて `true`・可視が減る方向）。
fn cap_shared_ids<'a>(
    ctx: &AuthContext,
    objects: impl Iterator<Item = &'a String>,
) -> (Vec<Uuid>, bool) {
    let ns = ctx.ns();
    let mut seen = std::collections::HashSet::new();
    let mut ids: Vec<Uuid> = Vec::new();
    for o in objects {
        let Some((_, id_part)) = o.split_once(':') else {
            continue;
        };
        let Some(local) = ns.strip_object_id(id_part) else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(local) else {
            continue;
        };
        if seen.contains(&id) {
            continue;
        }
        if ids.len() >= MAX_SHARED_IDS {
            tracing::warn!(
                user = %ctx.principal.id,
                limit = MAX_SHARED_IDS,
                "個別共有集合が上限を超過（超過分は不可視・PIT-18 fail-closed）"
            );
            return (ids, true);
        }
        seen.insert(id);
        ids.push(id);
    }
    (ids, false)
}

/// ロール集合（direct=read_subject_objects / effective=list_objects）を上限付きで解決する。
async fn role_set(
    ctx: &AuthContext,
    az: &AuthzMemo<'_>,
    effective: bool,
) -> Result<Vec<String>, DataError> {
    let raw = if effective {
        az.list_objects(ctx, Relation::Member, ObjectType::Role)
            .await?
    } else {
        az.read_subject_objects(ctx, ObjectType::Role).await?
    };
    // 実効集合（ListObjects）は応答の上限で黙って切れるので、上限に届いたら超過と同じに扱う（BUG-161）。
    // 直接タプル（Read API）は件数の上限が無く、切れない。
    if raw.len() > MAX_ROLE_SET || (effective && authz::visible::list_may_be_cut(raw.len())) {
        // ロール爆発は述語の部分適用（見えすぎ/見えなさすぎの不定）を招くため
        // fail-closed でクエリごと拒否する（PIT-18: 閾値超のフォールバックを仕様化）。
        return Err(DataError::Invalid(format!(
            "実行主体のロール集合が上限（{MAX_ROLE_SET}）を超えています。管理者にロール構成の見直しを依頼してください"
        )));
    }
    let ns = ctx.ns();
    let mut roles = Vec::with_capacity(raw.len());
    for o in raw.iter() {
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
                PolicyExpr::HasRole {
                    role: "b".into(),
                    subtree: false,
                },
                PolicyExpr::FieldCmp {
                    field: "dept".into(),
                    op: CmpOp::In,
                    value: PolicyOperand::UserRoles { subtree: true },
                },
            ]),
            &mut needs,
        );
        // 配下込みの has_role は名指しの判定だけ（集合は $user.roles のために引く）。
        assert_eq!(needs.members, BTreeSet::from(["a".to_string()]));
        assert!(needs.roles_effective);
        assert!(needs.roles_direct);

        let mut only_named = Needs::default();
        analyze(
            &PolicyExpr::HasRole {
                role: "a".into(),
                subtree: true,
            },
            &mut only_named,
        );
        assert!(!only_named.roles_effective && !only_named.roles_direct);
    }

    #[test]
    fn has_role_prefers_named_check_and_fails_closed() {
        let m = PolicyMaterial {
            role_members: HashMap::from([("a".to_string(), true), ("b".to_string(), false)]),
            roles_direct: Some(Arc::new(vec!["d".into()])),
            ..PolicyMaterial::default()
        };
        assert!(m.has_role("a", true));
        assert!(!m.has_role("b", true));
        // 判定の無いロールは不成立（集合も無い）。
        assert!(!m.has_role("zzz", true));
        // 直接所属は集合で判定する（名指しの判定は配下込みの意味なので使わない）。
        assert!(m.has_role("d", false));
        assert!(!m.has_role("a", false));
    }

    #[test]
    fn role_object_rejects_identifiers_fga_cannot_hold() {
        let ctx = AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t".into()),
            },
            "o".into(),
            "t".into(),
        );
        assert!(role_object(&ctx, "経理").is_some());
        for bad in ["", "a:b", "a#b", "a b", "a\tb", &"x".repeat(300)] {
            assert!(role_object(&ctx, bad).is_none(), "{bad:?} は問わずに不成立");
        }
    }
}
