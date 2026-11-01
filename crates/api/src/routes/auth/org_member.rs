//! 所属する組織の member のタプルを、ログインとセッションの更新のたびに IdP の所属グループへそろえる（D-130）。
//!
//! 組織の正は IdP の所属グループ（`/tenants/<tenant>/<org>[/<部署>]`）で、決め方は
//! [`authz::org_group::resolve_org`]（内部 API の `AuthContext` と同じ規則・単一定義）。そこから決まる組織の
//! `organization:<tenant>|<org>#member@user:<tenant>|<sub>` を、認可のチョークポイント（[`AuthzClient`]）で書き、
//! ほかの組織の member の直接タプルを外す。アプリのゲートウェイはトークンの所属グループを読まず、インストールの
//! 組織の member をこのタプルで確かめる（`app_gateway::router`）。
//!
//! - **書かない**: 所属グループから組織が決まらない人（既定の組織 `default` に落ちる人）。グループが無いことから
//!   権限を作らない（fail-safe）。組織の名前が FGA の識別子にできない文字を含むときも書かない。
//! - **外す**: 決まった組織のほかに持っている member（組織を移った人・グループから外れた人）。外し損ねると、組織を
//!   離れた人がアプリのゲートウェイでその組織に入り続ける。外すのは次のログインかセッションの更新（アクセス
//!   トークンの寿命ごと・ロールの同期と同じ周期）で、それまでの間は残る。
//! - **触らない**: admin・audit_viewer（組織の管理の側の付与・PIT-65）。他テナントのタプル（名前空間で除く）。
//! - 書く前に check で確かめる（毎回の書き込みと、OpenFGA の重複の誤りを避ける）。今の直接タプルを読めないときは
//!   外さない（未知の状態に破壊的な操作をしない・ロールの同期と同じ）。
//! - 失敗してもログインは止めない。付け損ねは権限が減る向き（`/me` とゲートウェイが断る）、外し損ねは次の同期で
//!   収束する。

use authz::{
    AuthzClient, Consistency, GroupLayout, Namespace, ObjectType, Principal, PrincipalKind,
    Relation, Subject,
};

/// 1 回の同期で変えたもの（ログ・テスト用）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct OrgMemberSync {
    /// 所属グループから決まった組織（決まらなければ `None`）。
    pub desired: Option<String>,
    /// この同期で member を書いた。
    pub granted: bool,
    /// この同期で member を外した組織。
    pub revoked: Vec<String>,
}

/// 所属グループから、member にする組織を決める（純粋関数）。
///
/// [`authz::org_group::resolve_org`] が組織を返さないとき（既定の組織に落ちるとき）と、FGA の識別子にできない
/// 名前のときは `None`。
pub(crate) fn desired_org(
    groups: &[String],
    tenant_id: &str,
    layout: GroupLayout,
) -> Option<String> {
    authz::org_group::resolve_org(groups, tenant_id, layout)
        .filter(|org| authz::validate_local_id(org).is_ok())
}

/// 利用者の組織の member を、所属グループから決まる組織だけにそろえる（本文はモジュールの説明）。
///
/// 利用者でない主体（ワークフロー・アプリの主体）には何もしない。
pub(crate) async fn sync_org_member(
    authz: &dyn AuthzClient,
    principal: &Principal,
    tenant_id: &str,
    layout: GroupLayout,
) -> OrgMemberSync {
    let mut out = OrgMemberSync::default();
    if principal.kind != PrincipalKind::User {
        return out;
    }
    let ns = Namespace::for_tenant(tenant_id);
    let subject = ns.user(&principal.id);
    out.desired = desired_org(&principal.groups, tenant_id, layout);
    if let Some(org) = &out.desired {
        out.granted = grant(authz, &subject, &ns, org).await;
    }
    out.revoked = revoke_others(authz, &subject, &ns, out.desired.as_deref()).await;
    out
}

/// 決まった組織の member を、無ければ書く（書いたら `true`）。
async fn grant(authz: &dyn AuthzClient, subject: &Subject, ns: &Namespace<'_>, org: &str) -> bool {
    let object = ns.organization(org);
    // 書いた直後のゲートウェイ・`/me` の判定と食い違わないよう、強い整合で確かめる。
    let present = authz
        .check(
            subject,
            Relation::Member,
            &object,
            Consistency::HigherConsistency,
        )
        .await;
    let wrote = match present {
        Ok(true) => return false,
        Ok(false) => authz.write_tuple(subject, Relation::Member, &object).await,
        Err(e) => Err(e),
    };
    match wrote {
        Ok(wrote) => {
            if wrote {
                tracing::info!(%org, "所属グループから組織の member を付けた");
            }
            wrote
        }
        Err(e) => {
            tracing::warn!(%org, error = %e, "組織の member を付けられません（次のログイン・更新で付け直す）");
            false
        }
    }
}

/// 直接タプルを持つ組織（relation は問わずに読む）のうち、`keep` でないものの member を外し、外した組織を返す。
///
/// member が無ければ消す操作は何もしない（admin・audit_viewer だけを持つ組織は no-op になる）。読めないときは
/// 何も外さない。
async fn revoke_others(
    authz: &dyn AuthzClient,
    subject: &Subject,
    ns: &Namespace<'_>,
    keep: Option<&str>,
) -> Vec<String> {
    let current = match authz
        .read_subject_objects(subject, ObjectType::Organization)
        .await
    {
        Ok(objects) => objects,
        Err(e) => {
            tracing::warn!(error = %e, "組織の直接タプルを読めません（外すのは次の同期に回す）");
            return Vec::new();
        }
    };
    let mut stale: Vec<String> = current
        .iter()
        .filter_map(|raw| raw.strip_prefix("organization:"))
        .filter_map(|id_part| ns.strip_object_id(id_part))
        .filter(|org| keep != Some(*org))
        .map(str::to_string)
        .collect();
    stale.sort();
    stale.dedup();
    let mut revoked = Vec::new();
    for org in stale {
        match authz
            .delete_tuple(subject, Relation::Member, &ns.organization(&org))
            .await
        {
            Ok(true) => {
                tracing::info!(%org, "所属グループから外れた組織の member を外した");
                revoked.push(org);
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(%org, error = %e, "組織の member を外せません（次のログイン・更新で外し直す）");
            }
        }
    }
    revoked
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Mutex;

    use async_trait::async_trait;
    use authz::{AuthzError, FgaObject, ReadTupleKey};

    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn desired_org_follows_the_org_rule_and_skips_the_default() {
        let layout = GroupLayout::TenantScoped;
        // 部署のグループからも組織が決まる（内部 API の resolve_org と同じ）。
        assert_eq!(
            desired_org(&v(&["/tenants/t/acme/eng"]), "t", layout).as_deref(),
            Some("acme")
        );
        // 他テナント配下・グループ無しは決まらない（既定の組織には member を書かない）。
        assert_eq!(desired_org(&v(&["/tenants/u/acme"]), "t", layout), None);
        assert_eq!(desired_org(&[], "t", layout), None);
        // FGA の識別子にできない名前は書かない。
        assert_eq!(desired_org(&v(&["/tenants/t/a#b"]), "t", layout), None);
        // single の従来の直下のグループも同じ規則で読む。
        assert_eq!(
            desired_org(&v(&["/acme/eng"]), "t", GroupLayout::TenantScopedOrLegacy).as_deref(),
            Some("acme")
        );
    }

    /// 直接タプルをメモリに持つ認可（check は member の直接タプルの有無・read は relation を問わず返す）。
    #[derive(Default)]
    struct Tuples {
        rows: Mutex<Vec<(String, Relation, String)>>,
        fail_read: bool,
    }

    impl Tuples {
        fn has(&self, s: &Subject, r: Relation, o: &FgaObject) -> bool {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .any(|(a, b, c)| a == s.as_str() && *b == r && c == o.as_str())
        }
        fn members(&self) -> Vec<String> {
            let mut out: Vec<String> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, r, _)| *r == Relation::Member)
                .map(|(_, _, o)| o.clone())
                .collect();
            out.sort();
            out
        }
    }

    #[async_trait]
    impl AuthzClient for Tuples {
        async fn check(
            &self,
            s: &Subject,
            r: Relation,
            o: &FgaObject,
            _: Consistency,
        ) -> Result<bool, AuthzError> {
            Ok(self.has(s, r, o))
        }
        async fn write_tuple(
            &self,
            s: &Subject,
            r: Relation,
            o: &FgaObject,
        ) -> Result<bool, AuthzError> {
            if self.has(s, r, o) {
                return Ok(false);
            }
            self.rows
                .lock()
                .unwrap()
                .push((s.as_str().into(), r, o.as_str().into()));
            Ok(true)
        }
        async fn delete_tuple(
            &self,
            s: &Subject,
            r: Relation,
            o: &FgaObject,
        ) -> Result<bool, AuthzError> {
            let mut rows = self.rows.lock().unwrap();
            let before = rows.len();
            rows.retain(|(a, b, c)| !(a == s.as_str() && *b == r && c == o.as_str()));
            Ok(rows.len() != before)
        }
        async fn read_tuples(
            &self,
            _: &FgaObject,
            _: Option<Relation>,
        ) -> Result<Vec<ReadTupleKey>, AuthzError> {
            Ok(vec![])
        }
        async fn list_objects(
            &self,
            _: &Subject,
            _: Relation,
            _: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            Ok(vec![])
        }
        async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
            Ok(0)
        }
        async fn read_subject_objects(
            &self,
            s: &Subject,
            ty: ObjectType,
        ) -> Result<Vec<String>, AuthzError> {
            if self.fail_read {
                return Err(AuthzError::Unexpected {
                    status: 500,
                    body: "read".into(),
                });
            }
            let prefix = format!("{}:", ty.as_str());
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(a, _, o)| a == s.as_str() && o.starts_with(&prefix))
                .map(|(_, _, o)| o.clone())
                .collect())
        }
    }

    fn user(groups: &[&str]) -> Principal {
        Principal {
            kind: PrincipalKind::User,
            id: "u1".into(),
            email: None,
            groups: v(groups),
            roles: vec![],
            tenant_id: Some("t".into()),
        }
    }

    #[tokio::test]
    async fn grants_the_org_from_groups_and_revokes_the_rest() {
        let fga = Tuples::default();
        let ns = Namespace::for_tenant("t");
        let me = ns.user("u1");
        // 前に別の組織の member と管理者、別のテナントの member を持っていた。
        for (r, o) in [
            (Relation::Member, ns.organization("old")),
            (Relation::Admin, ns.organization("old")),
            (
                Relation::Member,
                Namespace::for_tenant("other").organization("acme"),
            ),
        ] {
            fga.write_tuple(&me, r, &o).await.unwrap();
        }
        let layout = GroupLayout::TenantScoped;
        let out = sync_org_member(&fga, &user(&["/tenants/t/acme/eng"]), "t", layout).await;
        assert_eq!(out.desired.as_deref(), Some("acme"));
        assert!(out.granted);
        assert_eq!(out.revoked, ["old"]);
        assert_eq!(
            fga.members(),
            ["organization:other|acme", "organization:t|acme"],
            "別のテナントのタプルには触らない"
        );
        assert!(
            fga.has(&me, Relation::Admin, &ns.organization("old")),
            "管理者は外さない"
        );

        // 2 回目は何も変えない（書き込みを繰り返さない）。
        let again = sync_org_member(&fga, &user(&["/tenants/t/acme/eng"]), "t", layout).await;
        assert!(!again.granted && again.revoked.is_empty());

        // 所属グループから外れた（グループが無い）人は、member を外す。既定の組織には書かない。
        let left = sync_org_member(&fga, &user(&[]), "t", layout).await;
        assert_eq!(left.desired, None);
        assert_eq!(left.revoked, ["acme"]);
        assert_eq!(fga.members(), ["organization:other|acme"]);
    }

    #[tokio::test]
    async fn keeps_members_when_the_current_tuples_cannot_be_read() {
        let fga = Tuples {
            fail_read: true,
            ..Tuples::default()
        };
        let ns = Namespace::for_tenant("t");
        fga.write_tuple(&ns.user("u1"), Relation::Member, &ns.organization("old"))
            .await
            .unwrap();
        let out = sync_org_member(
            &fga,
            &user(&["/tenants/t/acme"]),
            "t",
            GroupLayout::TenantScoped,
        )
        .await;
        assert!(out.granted, "付けるのは読めなくても行う");
        assert!(out.revoked.is_empty(), "読めないときは外さない");
        assert_eq!(fga.members(), ["organization:t|acme", "organization:t|old"]);
    }

    #[tokio::test]
    async fn ignores_principals_that_are_not_users() {
        let fga = Tuples::default();
        let mut p = user(&["/tenants/t/acme"]);
        p.kind = PrincipalKind::Workflow;
        let out = sync_org_member(&fga, &p, "t", GroupLayout::TenantScoped).await;
        assert_eq!(out, OrgMemberSync::default());
        assert!(fga.members().is_empty());
    }
}
