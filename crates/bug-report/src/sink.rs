use crate::ReportContent;
use async_trait::async_trait;
use authz::AuthContext;
use uuid::Uuid;

/// 外部に渡せる最終 snapshot のみ。会話やストレージの参照を持たない。
pub struct ConfirmedBugReport {
    pub content: ReportContent,
    pub payload_hash: String,
}
pub struct DeliveryIdentity {
    pub id: Uuid,
    pub report_id: Uuid,
    pub submission_rev: i64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Disabled,
    Sent { external_id: String },
    Unknown,
    Failed,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Disabled,
    Found { external_id: String },
    Inconclusive,
}
#[async_trait]
pub trait BugReportSink: Send + Sync {
    async fn send(
        &self,
        ctx: &AuthContext,
        report: &ConfirmedBugReport,
        identity: &DeliveryIdentity,
    ) -> SendOutcome;
    async fn reconcile(&self, ctx: &AuthContext, identity: &DeliveryIdentity) -> ReconcileOutcome;
}
pub struct DisabledBugReportSink;
#[async_trait]
impl BugReportSink for DisabledBugReportSink {
    async fn send(
        &self,
        _: &AuthContext,
        _: &ConfirmedBugReport,
        _: &DeliveryIdentity,
    ) -> SendOutcome {
        SendOutcome::Disabled
    }
    async fn reconcile(&self, _: &AuthContext, _: &DeliveryIdentity) -> ReconcileOutcome {
        ReconcileOutcome::Disabled
    }
}
/// production の設定 enum には存在せず、unit test だけでコンパイルする。
#[cfg(test)]
pub struct StubBugReportSink;
#[cfg(test)]
#[async_trait]
impl BugReportSink for StubBugReportSink {
    async fn send(
        &self,
        _: &AuthContext,
        _: &ConfirmedBugReport,
        _: &DeliveryIdentity,
    ) -> SendOutcome {
        SendOutcome::Sent {
            external_id: "stub".into(),
        }
    }
    async fn reconcile(&self, _: &AuthContext, _: &DeliveryIdentity) -> ReconcileOutcome {
        ReconcileOutcome::Found {
            external_id: "stub".into(),
        }
    }
}
