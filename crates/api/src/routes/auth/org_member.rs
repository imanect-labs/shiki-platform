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
//!   トークンの寿命ごと・ロールの同期と同じ周期）と、Keycloak の所属グループとの定期の照合
//!   （[`crate::membership_reconcile`]・D-136。shiki の web にログインしないままアプリのトークンだけを取る人も外れる）。
//! - **残す**: 組織が決まらない人の、既定の組織（`AuthContext` が落ちる先・[`DEFAULT_ORG`]）の member。グループを
//!   使わずに既定の組織の member を手で書いて使わせる構成を壊さない（BUG-200）。ほかの組織の member は外す。
//!   定期の照合は、既定の組織の member をいつも残す（[`orgs_kept_by_reconcile`]。既定の組織を外すのはここだけ）。
//! - **触らない**: admin・audit_viewer（組織の管理の側の付与・PIT-65）。他テナントのタプル（名前空間で除く）。
//! - 書く前に check で確かめる（毎回の書き込みと、OpenFGA の重複の誤りを避ける）。今の直接タプルを読めないときは
//!   外さない（未知の状態に破壊的な操作をしない・ロールの同期と同じ）。
//! - 失敗してもログインは止めない。付け損ねは権限が減る向き（`/me` とゲートウェイが断る）、外し損ねは次の同期で
//!   収束する。
//! - 付け外しは、システムを主体にした監査に組織ごとに 1 行ずつ残す（[`super::membership_audit`]・GAP-78）。付けたことを
//!   記録できなければ付けたタプルを外し、外したことを記録できなくても外したものは戻さない（どちらも権限が減る向き）。
//!   付ける書き込みが誤りで返ったとき（応答が切れても適用されていることがある）は確かめ直し、あれば付けたものとして
//!   記録し、確かめられなければ外して、外したことを記録する（記録の無い付与を残さない）。

use authz::{
    AuthzClient, Consistency, GroupLayout, Namespace, ObjectType, Principal, PrincipalKind,
    Relation, Subject, TenantId,
};

use super::membership_audit::{AuditSink, MembershipAudit, MembershipSource};
use crate::extract::DEFAULT_ORG;

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

/// 利用者の組織の member を、所属グループから決まる組織だけにそろえ、付け外しを監査に残す（本文はモジュールの説明）。
///
/// `sink` と `source` は監査の記録係と仕事（ログインかセッションの更新か）。利用者でない主体（ワークフロー・アプリの
/// 主体）には何もしない。
pub(crate) async fn sync_org_member_audited(
    authz: &dyn AuthzClient,
    sink: &dyn AuditSink,
    source: MembershipSource,
    principal: &Principal,
    tenant: &TenantId,
    layout: GroupLayout,
) -> OrgMemberSync {
    let mut out = OrgMemberSync::default();
    if principal.kind != PrincipalKind::User {
        return out;
    }
    let audit = MembershipAudit::new(sink, source, tenant, &principal.id);
    let ns = Namespace::for_tenant(tenant);
    let subject = ns.user(&principal.id);
    out.desired = desired_org(&principal.groups, tenant, layout);
    if let Some(org) = &out.desired {
        out.granted = grant(authz, &audit, &subject, &ns, org).await;
    }
    // 組織が決まらない人は既定の組織に落ちるので、その member は残す（BUG-200）。
    let keep = out.desired.as_deref().unwrap_or(DEFAULT_ORG);
    out.revoked = revoke_others(authz, &audit, &subject, &ns, keep).await;
    out
}

/// 監査を確かめない試し用の [`sync_org_member_audited`]（記録は捨てる・仕事はログイン）。
#[cfg(test)]
pub(crate) async fn sync_org_member(
    authz: &dyn AuthzClient,
    principal: &Principal,
    tenant_id: &str,
    layout: GroupLayout,
) -> OrgMemberSync {
    let Ok(tenant) = TenantId::parse(tenant_id) else {
        return OrgMemberSync::default();
    };
    let sink = super::membership_audit::Discard;
    sync_org_member_audited(
        authz,
        &sink,
        MembershipSource::Login,
        principal,
        &tenant,
        layout,
    )
    .await
}

/// 決まった組織の member を、無ければ書く（書いて監査に残せたら `true`）。
///
/// 書いたことを監査に残せなければ、書いたタプルを外して `false`（記録の無い付与を残さない・次の同期で付け直して記録する）。
/// 書き込みが誤りで返ったら、適用されたかを [`settle_unknown_write`] で確かめる（あれば書いたものとして記録する。次の同期
/// では check が真になって記録しないので、ここで記録しないと記録の無い付与が残り続ける）。
async fn grant(
    authz: &dyn AuthzClient,
    audit: &MembershipAudit<'_>,
    subject: &Subject,
    ns: &Namespace<'_>,
    org: &str,
) -> bool {
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
    match present {
        Ok(true) => return false,
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(%org, error = %e, "組織の member を確かめられません（次のログイン・更新で付け直す）");
            return false;
        }
    }
    let wrote = match authz.write_tuple(subject, Relation::Member, &object).await {
        Ok(wrote) => wrote,
        Err(e) => {
            tracing::warn!(%org, error = %e, "組織の member の書き込みが誤りで返った（適用されたかを確かめる）");
            settle_unknown_write(authz, audit, subject, &object, org).await
        }
    };
    if !wrote {
        return false;
    }
    if let Err(e) = audit.org_granted(org).await {
        tracing::error!(%org, error = %e, "組織の member を付けたことを監査に残せません（付けたタプルを外す）");
        if let Err(e) = authz.delete_tuple(subject, Relation::Member, &object).await {
            tracing::error!(%org, error = %e,
                "監査に残せなかった組織の member を外せません（記録の無い付与が残っている・手で確かめる）");
        }
        return false;
    }
    tracing::info!(%org, "所属グループから組織の member を付けた");
    true
}

/// 書き込みが誤りで返った組織の member が実際に書けているかを、強い整合の check で確かめる（書けていれば `true`・
/// `tuple_commit.rs` の決まり (a) と同じく、誤りでも適用されていることがあるものとして扱う）。
///
/// 確かめられなければ、書けていたかもしれないタプルを外して `false`（冪等・権限が減る向きで、次の同期で付け直して
/// 記録する）。直前の check で無かったので、外すのはこの書き込みが足したものか、同じ人の並行する同期（ログインと
/// セッションの更新は直列にしていない）が足したもの。実際に外したときは、外したと記録する（並行する同期が付けて記録
/// した member を外したときも、監査とタプルが食い違わない）。並行する同期が付けた member を確かめ直しで見つけると、
/// 同じ付与を 2 回記録することがある（まれ・権限は変わらない）。
///
/// 確かめ直しは誤りを受けた直後なので、クライアントが打ち切った後でサーバーが書き込みを確定させたときは、無いと
/// 答えて外しもしない。そのときは記録の無い付与が残る（狭い時間の窓・定期の照合では見つけない）。
async fn settle_unknown_write(
    authz: &dyn AuthzClient,
    audit: &MembershipAudit<'_>,
    subject: &Subject,
    object: &authz::FgaObject,
    org: &str,
) -> bool {
    match authz
        .check(
            subject,
            Relation::Member,
            object,
            Consistency::HigherConsistency,
        )
        .await
    {
        Ok(present) => present,
        Err(e) => {
            tracing::warn!(%org, error = %e, "組織の member を確かめ直せません（書けていたかもしれないので外す）");
            match authz.delete_tuple(subject, Relation::Member, object).await {
                Ok(true) => audit.org_revoked(org).await,
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(%org, error = %e,
                        "確かめられなかった組織の member を外せません（記録の無い付与が残っているかもしれない）");
                }
            }
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
    audit: &MembershipAudit<'_>,
    subject: &Subject,
    ns: &Namespace<'_>,
    keep: &str,
) -> Vec<String> {
    match current_orgs(authz, subject, ns).await {
        Some(current) => {
            revoke_stale(authz, audit, subject, ns, &current, &[keep.to_string()]).await
        }
        None => Vec::new(),
    }
}

/// 利用者が直接タプルを持つこのテナントの組織（relation は問わない・名前の順・重複なし）。
///
/// 読めないときは `None`（呼ぶ側は何も外さない）。ログインの同期と定期の照合（D-136）が使う。
pub(crate) async fn current_orgs(
    authz: &dyn AuthzClient,
    subject: &Subject,
    ns: &Namespace<'_>,
) -> Option<Vec<String>> {
    let objects = match authz
        .read_subject_objects(subject, ObjectType::Organization)
        .await
    {
        Ok(objects) => objects,
        Err(e) => {
            tracing::warn!(error = %e, "組織の直接タプルを読めません（外すのは次の同期に回す）");
            return None;
        }
    };
    let mut orgs: Vec<String> = objects
        .iter()
        .filter_map(|raw| raw.strip_prefix("organization:"))
        .filter_map(|id_part| ns.strip_object_id(id_part))
        .map(str::to_string)
        .collect();
    orgs.sort();
    orgs.dedup();
    Some(orgs)
}

/// 定期の照合（D-136）で member を残す組織: 所属グループから決まりうる組織のすべて
/// （[`authz::org_group::org_candidates`]）と、既定の組織（[`DEFAULT_ORG`]）。
///
/// ログインの同期が残す組織（[`desired_org`]・決まらなければ既定の組織）は必ずこの中にある。照合はログインの同期が
/// 付けた member を外さず、所属するグループが 1 つも無くなった組織の member だけを外す（複数の組織のグループに
/// 入っている人を 1 つにそろえるのはログインの同期・GAP-75）。
///
/// 既定の組織の member は、所属グループと照らさずに必ず残す。既定の組織には自動で member を付けない（手で書いたものだけ
/// がある）ので、所属グループで外す理由が無い。照合は Keycloak の管理 API の所属グループで決め、ログインの同期は
/// トークンの `groups` で決めるので、shiki-web のクライアントに所属グループのマッパーが無い構成では 2 つが食い違う
/// （ログインの同期は組織が決まらず既定の組織を残すのに、照合は管理 API のグループから組織を決めて既定の組織を外し、
/// 次のログインでも戻らない）。既定の組織を外すのはログインの同期だけにする（BUG-200）。
pub(crate) fn orgs_kept_by_reconcile(
    groups: &[String],
    tenant_id: &str,
    layout: GroupLayout,
) -> Vec<String> {
    let mut orgs: Vec<String> = authz::org_group::org_candidates(groups, tenant_id, layout)
        .into_iter()
        .filter(|org| authz::validate_local_id(org).is_ok())
        .collect();
    if !orgs.iter().any(|org| org == DEFAULT_ORG) {
        orgs.push(DEFAULT_ORG.to_string());
    }
    orgs
}

/// `current` のうち `keep` にない組織の member を外し、外した組織を返す（ログインの同期と定期の照合で共用）。
///
/// 外すのは member だけ（admin・audit_viewer には触れない・PIT-65）。member が無い組織は no-op。外した組織は、外して
/// から 1 つずつ監査に残す（記録できなくても戻さない・[`MembershipAudit::org_revoked`]）。
pub(crate) async fn revoke_stale(
    authz: &dyn AuthzClient,
    audit: &MembershipAudit<'_>,
    subject: &Subject,
    ns: &Namespace<'_>,
    current: &[String],
    keep: &[String],
) -> Vec<String> {
    let mut revoked = Vec::new();
    for org in current.iter().filter(|org| !keep.contains(org)) {
        match authz
            .delete_tuple(subject, Relation::Member, &ns.organization(org))
            .await
        {
            Ok(true) => {
                tracing::info!(%org, "所属グループから外れた組織の member を外した");
                audit.org_revoked(org).await;
                revoked.push(org.clone());
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(%org, error = %e, "組織の member を外せません（次の同期で外し直す）");
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

    #[test]
    fn reconcile_keeps_every_org_the_login_sync_could_keep() {
        let layout = GroupLayout::TenantScopedOrLegacy;
        // 従来解釈は claim の先頭の組織を採る。照合は並びに依存せず、どちらも残す（並びの違いで外さない）。
        let groups = v(&["/beta/x", "/alpha"]);
        assert_eq!(desired_org(&groups, "t", layout).as_deref(), Some("beta"));
        assert_eq!(
            orgs_kept_by_reconcile(&groups, "t", layout),
            ["alpha", "beta", DEFAULT_ORG]
        );
        // 組織が決まらない人は既定の組織（ログインの同期と同じ・BUG-200）。識別子にできない名前は候補にしない。
        assert_eq!(orgs_kept_by_reconcile(&[], "t", layout), [DEFAULT_ORG]);
        assert_eq!(
            orgs_kept_by_reconcile(&v(&["/tenants/t/a#b"]), "t", GroupLayout::TenantScoped),
            [DEFAULT_ORG]
        );
        assert_eq!(
            orgs_kept_by_reconcile(&v(&["/tenants/u/acme"]), "t", GroupLayout::TenantScoped),
            [DEFAULT_ORG],
            "他テナント配下のグループは候補にしない"
        );
        // 選ばれる組織（名前の順で最小）が識別子にできない名前なら、ログインの同期は既定の組織を残すので照合も残す。
        let groups = v(&["/tenants/t/a#b", "/tenants/t/beta"]);
        assert_eq!(desired_org(&groups, "t", GroupLayout::TenantScoped), None);
        assert_eq!(
            orgs_kept_by_reconcile(&groups, "t", GroupLayout::TenantScoped),
            ["beta", DEFAULT_ORG]
        );
        // 既定の組織という名前のグループに入っている人も、既定の組織を 2 回並べない。
        assert_eq!(
            orgs_kept_by_reconcile(&v(&["/tenants/t/default"]), "t", GroupLayout::TenantScoped),
            [DEFAULT_ORG]
        );
    }

    /// 管理 API の所属グループから組織が決まっても、照合は既定の組織の member を外さない。ログインの同期はトークンの
    /// `groups` で決めるので、shiki-web に所属グループのマッパーが無い構成（トークンに groups が無い）では既定の組織を
    /// 残し、照合と食い違う（照合が外すと、次のログインでも戻らず /me が 403 になる・BUG-200）。
    #[tokio::test]
    async fn reconcile_keeps_the_default_org_even_when_admin_groups_decide_an_org() {
        let fga = Tuples::default();
        let ns = Namespace::for_tenant("t");
        let me = ns.user("u1");
        for org in [DEFAULT_ORG, "old"] {
            fga.write_tuple(&me, Relation::Member, &ns.organization(org))
                .await
                .unwrap();
        }
        // single の従来解釈では、共有している realm のどの直下のグループ（/IT）も組織の候補になる。
        let admin_groups = v(&["/IT"]);
        let layout = GroupLayout::TenantScopedOrLegacy;
        assert_eq!(
            desired_org(&admin_groups, "t", layout).as_deref(),
            Some("IT")
        );
        let current = current_orgs(&fga, &me, &ns).await.unwrap();
        let keep = orgs_kept_by_reconcile(&admin_groups, "t", layout);
        let tenant = TenantId::parse("t").unwrap();
        let audit = MembershipAudit::new(
            &super::super::membership_audit::Discard,
            MembershipSource::Reconcile,
            &tenant,
            "u1",
        );
        let revoked = revoke_stale(&fga, &audit, &me, &ns, &current, &keep).await;
        assert_eq!(
            revoked,
            ["old"],
            "所属グループの無い組織の member だけを外す"
        );
        assert_eq!(fga.members(), ["organization:t|default"]);
        // トークンに groups が無い人のログインの同期も、既定の組織を残す（照合と同じ結果）。
        let out = sync_org_member(&fga, &user(&[]), "t", layout).await;
        assert!(out.revoked.is_empty(), "{out:?}");
        assert_eq!(fga.members(), ["organization:t|default"]);
    }

    #[tokio::test]
    async fn keeps_the_default_org_member_when_groups_decide_no_org() {
        // グループを使わずに既定の組織の member を手で書いた構成（BUG-200）。
        let fga = Tuples::default();
        let ns = Namespace::for_tenant("t");
        let me = ns.user("u1");
        for org in [DEFAULT_ORG, "old"] {
            fga.write_tuple(&me, Relation::Member, &ns.organization(org))
                .await
                .unwrap();
        }
        let layout = GroupLayout::TenantScoped;
        let out = sync_org_member(&fga, &user(&[]), "t", layout).await;
        assert_eq!(out.desired, None);
        assert!(!out.granted, "既定の組織には書かない");
        assert_eq!(out.revoked, ["old"], "ほかの組織の member は外す");
        assert_eq!(fga.members(), ["organization:t|default"]);

        // 所属グループから組織が決まった人は、既定の組織の member も外す（決まった組織だけにそろえる）。
        let out = sync_org_member(&fga, &user(&["/tenants/t/acme"]), "t", layout).await;
        assert_eq!(out.revoked, [DEFAULT_ORG]);
        assert_eq!(fga.members(), ["organization:t|acme"]);
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
