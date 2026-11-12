//! 組織の member と部署・ロールの member の付け外しを、システムを主体にした監査に残す（GAP-78・NFR-6）。
//!
//! 付け外すのはログイン・セッションの更新（[`super::org_member`]・[`super::role_member`]）と、Keycloak の所属グループとの
//! 定期の照合（[`crate::membership_reconcile`]）。どれも IdP の所属から決まった変更で本人の操作ではないので、主体は
//! 仕事の名前（[`SYNC_ACTOR`]・[`RECONCILE_ACTOR`]）にする。本人を主体にすると、監査を「この人の操作」で絞ったときに
//! 本人の操作でない行が混ざる。
//!
//! # 行の形
//!
//! - `object_type = "user"`・`object_id = <sub>`（監査の画面の対象の絞り込みで、人ごとに引ける）。
//! - org の列: 組織の付け外しは member を変えた組織。ロールの付け外しは、ロールを持つ組織
//!   （[`super::role_member::role_owner_org`]）。所属グループから来たロール（部署の `<org>/<dept>` と、2 つ目の組織の
//!   グループの `<org>`）はその組織で、属性 `roles` のロールは（`/` を含んでも）、`AuthContext` と同じ規則で決まる
//!   利用者の組織（ログインの同期は `ctx.org()`、照合は所属グループから決めた組織）。外すロールは今の直接タプルから
//!   決めるのでどこから来たかが分からず、台帳（`directory_role`）がそのロールを先頭のセグメントの組織のものとして
//!   いるときだけ所属グループから来たものとする（[`super::role_member::with_ledger_sources`]）。監査は組織ごとに引き、
//!   部署の台帳も組織の中に閉じているので、ほかの組織の部署の付け外しをその組織の監査に書かない。
//!   属性 `roles` のロールは、付けたときと外したときで利用者の組織が変わっていれば、付けた行と外した行が別の組織の
//!   監査に分かれる（属性 `roles` のロールはテナント全体のもので、持ち主の組織が無い）。
//! - metadata: 組織は `{source, org}`、ロールは `{source, roles: […]}`。`source` は `login`・`session_refresh`・
//!   `reconcile`。所属グループの一覧は入れない。
//! - 組織は 1 つにつき 1 行、ロールは 1 回の同期の付与・剥奪を、ロールを持つ組織ごとにそれぞれ 1 行にまとめる。変えた
//!   ときだけ記録する（間引きも要約もしない）。
//! - 権限そのものの変更なので、ハッシュチェーンにつなぐ（共有・監査閲覧者の付与と同じ・
//!   [`storage::audit::AuditRecorder::record_chained`]）。
//!
//! # 記録できなかったとき（どちらの側でも権限が減る向き）
//!
//! - 付けた側: 付けたタプルを外す（記録の無い付与を残さない）。付け損ねは次の同期で付け直して記録する。
//! - 外した側: 外したものは戻さない（戻すと外した権限がよみがえる）。error のログに残し、[`MembershipAudit::failed`]
//!   に数える（照合は `ReconcileReport::audit_failed` に出す）。

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use authz::{AuthContext, Principal, PrincipalKind, TenantId};
use serde_json::{json, Value};
use storage::audit::{AuditAction, AuditEntry, AuditRecorder, Decision};
use storage::StorageError;

/// ログインとセッションの更新の同期の主体。
pub(crate) const SYNC_ACTOR: &str = "system:membership-sync";
/// 定期の照合（D-136）の主体。
pub(crate) const RECONCILE_ACTOR: &str = "system:membership-reconcile";
/// 行の object_type（object_id は利用者の sub）。
pub(crate) const OBJECT_TYPE: &str = "user";

/// 監査に 1 行を書く口。本番は [`AuditRecorder`]（ハッシュチェーンにつなぐ）。
///
/// [`AuditRecorder`] は接続プールを持つ具体の型で差し替えられないので、試しの偽の記録係はこのトレイトで作る
/// （api の中に閉じた型）。
#[async_trait]
pub(crate) trait AuditSink: Send + Sync {
    async fn record(&self, ctx: &AuthContext, entry: AuditEntry<'_>) -> Result<(), StorageError>;
}

#[async_trait]
impl AuditSink for AuditRecorder {
    async fn record(&self, ctx: &AuthContext, entry: AuditEntry<'_>) -> Result<(), StorageError> {
        self.record_chained(ctx, entry).await
    }
}

/// 記録を捨てる記録係（監査を確かめない試し用）。
#[cfg(test)]
pub(crate) struct Discard;

#[cfg(test)]
#[async_trait]
impl AuditSink for Discard {
    async fn record(&self, _: &AuthContext, _: AuditEntry<'_>) -> Result<(), StorageError> {
        Ok(())
    }
}

/// 付け外しをした仕事（metadata の `source` と主体を決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MembershipSource {
    /// ログイン（`/auth/callback`）。
    Login,
    /// セッションの更新（アクセストークンの寿命ごと）。
    SessionRefresh,
    /// Keycloak の所属グループとの定期の照合（D-136）。
    Reconcile,
}

impl MembershipSource {
    /// metadata の `source` に書く値。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MembershipSource::Login => "login",
            MembershipSource::SessionRefresh => "session_refresh",
            MembershipSource::Reconcile => "reconcile",
        }
    }

    /// 行の主体（actor）。
    pub(crate) fn actor(self) -> &'static str {
        match self {
            MembershipSource::Login | MembershipSource::SessionRefresh => SYNC_ACTOR,
            MembershipSource::Reconcile => RECONCILE_ACTOR,
        }
    }
}

/// 1 人の付け外しを監査に残す係（記録係・仕事・テナント・利用者を束ねる）。
pub(crate) struct MembershipAudit<'a> {
    sink: &'a dyn AuditSink,
    source: MembershipSource,
    tenant: &'a TenantId,
    user: &'a str,
    failed: AtomicUsize,
}

impl<'a> MembershipAudit<'a> {
    pub(crate) fn new(
        sink: &'a dyn AuditSink,
        source: MembershipSource,
        tenant: &'a TenantId,
        user: &'a str,
    ) -> Self {
        MembershipAudit {
            sink,
            source,
            tenant,
            user,
            failed: AtomicUsize::new(0),
        }
    }

    /// 記録できなかった行の数（付けた側も外した側も数える）。
    pub(crate) fn failed(&self) -> usize {
        self.failed.load(Ordering::Relaxed)
    }

    /// 付けた組織の member を記録する。記録できなければ誤りを返す（呼ぶ側が付けたタプルを外す）。
    pub(crate) async fn org_granted(&self, org: &str) -> Result<(), StorageError> {
        self.write(
            AuditAction::OrgMemberGrant,
            org,
            org_metadata(self.source, org),
        )
        .await
    }

    /// 外した組織の member を記録する。記録できなくても外したものは戻さない（error のログに残して数える）。
    pub(crate) async fn org_revoked(&self, org: &str) {
        if let Err(e) = self
            .write(
                AuditAction::OrgMemberRevoke,
                org,
                org_metadata(self.source, org),
            )
            .await
        {
            tracing::error!(tenant_id = %self.tenant, user = %self.user, %org, source = self.source.as_str(),
                error = %e, "組織の member を外したことを監査に残せません（外したものは戻さない）");
        }
    }

    /// 付けたロールを 1 行で記録する（`org` はロールを持つ組織・[`super::role_member::role_owner_org`]）。記録できなければ誤りを返す（呼ぶ側が付けたタプルを外す）。
    pub(crate) async fn roles_granted(
        &self,
        org: &str,
        roles: &[String],
    ) -> Result<(), StorageError> {
        self.write(
            AuditAction::RoleMemberGrant,
            org,
            roles_metadata(self.source, roles),
        )
        .await
    }

    /// 外したロールを 1 行で記録する（`org` はロールを持つ組織・[`super::role_member::role_owner_org`]）。記録できなくても外したものは戻さない。
    pub(crate) async fn roles_revoked(&self, org: &str, roles: &[String]) {
        if let Err(e) = self
            .write(
                AuditAction::RoleMemberRevoke,
                org,
                roles_metadata(self.source, roles),
            )
            .await
        {
            tracing::error!(tenant_id = %self.tenant, user = %self.user, %org, ?roles, source = self.source.as_str(),
                error = %e, "ロールの member を外したことを監査に残せません（外したものは戻さない）");
        }
    }

    async fn write(
        &self,
        action: AuditAction,
        org: &str,
        metadata: Value,
    ) -> Result<(), StorageError> {
        let ctx = actor_context(self.source, self.tenant, org);
        let entry = AuditEntry {
            action,
            object_type: OBJECT_TYPE,
            object_id: self.user,
            decision: Decision::Allow,
            trace_id: None,
            metadata,
        };
        let recorded = self.sink.record(&ctx, entry).await;
        if recorded.is_err() {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
        recorded
    }
}

/// 行の `AuthContext`（主体は仕事の名前・org の列は `org`）。
pub(crate) fn actor_context(source: MembershipSource, tenant: &TenantId, org: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: source.actor().to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.to_string()),
        },
        org.to_string(),
        tenant.clone(),
    )
}

/// 組織の付け外しの metadata（`{source, org}`）。
pub(crate) fn org_metadata(source: MembershipSource, org: &str) -> Value {
    json!({ "source": source.as_str(), "org": org })
}

/// ロールの付け外しの metadata（`{source, roles}`）。
pub(crate) fn roles_metadata(source: MembershipSource, roles: &[String]) -> Value {
    json!({ "source": source.as_str(), "roles": roles })
}
