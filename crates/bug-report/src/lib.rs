//! 本人提出の報告集約。HTTP・会話本文・外部通信を所有しない。
mod config;
pub mod delivery;
pub mod github;
pub use config::*;
mod help_case;
mod model;
pub use help_case::*;
mod queries;
mod service;
pub mod sink;
mod store;
pub use model::*;
pub use service::{BugReportService, CommandRequest};
pub use store::BugReportStore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("revision or idempotency conflict")]
    Conflict,
    #[error("invalid report")]
    Invalid,
    #[error("rate limited; retry after 3600 seconds")]
    RateLimited,
    #[error("disabled")]
    Disabled,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Audit(#[from] storage::error::StorageError),
    #[error(transparent)]
    Authorization(#[from] authz::AuthzError),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::pedantic)]
mod tests {
    use super::*;
    use sink::*;
    fn ctx() -> authz::AuthContext {
        authz::AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: None,
            },
            "org".into(),
            authz::TenantId::parse("test").unwrap(),
        )
    }
    fn content() -> ReportContent {
        ReportContent {
            category: Category::Bug,
            title: "不具合".into(),
            symptom: "症状".into(),
            steps_tried: String::new(),
            expected_behavior: String::new(),
        }
    }
    #[test]
    fn validation_and_production_config_fail_closed() {
        assert!(!BugReportConfig::default().enabled);
        for backend in ["stub", "unknown"] {
            assert!(serde_json::from_value::<BugReportConfig>(
                serde_json::json!({"enabled":true,"backend":backend})
            )
            .is_err());
        }
        assert!(content().validate().is_ok());
        for bad in [
            "<img>",
            "sk-test-secret",
            "ghp_test_secret",
            "https://example.com",
            "![x](x)",
            "www.example.com",
            "\u{0000}",
        ] {
            let mut c = content();
            c.symptom = bad.into();
            assert!(matches!(c.validate(), Err(Error::Invalid)));
        }
        let mut c = content();
        c.title = "あ".repeat(201);
        assert!(c.validate().is_err());
        c = content();
        c.symptom = "あ".repeat(2001);
        assert!(c.validate().is_err());
        c = content();
        c.title = " ".into();
        assert!(c.validate().is_err());
        assert!(serde_json::from_value::<ReportContent>(serde_json::json!({"category":"bug","title":"x","symptom":"","steps_tried":"","expected_behavior":"","attachment":"x"})).is_err());
    }
    #[test]
    fn api_key_validation_respects_boundaries_and_length() {
        for text in [
            "disk-full",
            "task-list",
            "mask-image",
            "sk-short",
            "sk-",
            "sk-!!!!!!!!!!",
            "task-abcdefghijklmnopqrstuvwxyz",
        ] {
            let mut c = content();
            c.title = text.into();
            c.symptom = text.into();
            assert!(c.validate().is_ok(), "{text}");
        }
        for text in [
            "sk-abcdefghij",
            "APIキー: sk-proj-abcdefghijklmnopqrstuvwxyz",
            "(sk-1234567890)",
            "キーはsk-abcdefghijです",
        ] {
            for field in 0..4 {
                let mut c = content();
                match field {
                    0 => c.title = text.into(),
                    1 => c.symptom = text.into(),
                    2 => c.steps_tried = text.into(),
                    _ => c.expected_behavior = text.into(),
                }
                assert!(c.validate().is_err(), "{text} in field {field}");
            }
        }
    }
    #[test]
    fn consent_approval_and_terminal_states() {
        use BugReportStatus::*;
        let mut p = BugReportPolicy::default();
        assert_eq!(p.submitted_status(false), Held);
        assert_eq!(p.submitted_status(true), PendingApproval);
        p.require_approval = false;
        assert_eq!(p.submitted_status(true), Queued);
        assert_eq!(p.submitted_status(false), Held);
        for terminal in [Sent, Cancelled] {
            for next in [Draft, Queued, Cancelled, Sending, Held] {
                assert!(!terminal.permits(next));
            }
        }
        assert!(!Sending.permits(Cancelled));
        assert!(!DeliveryUnknown.permits(Queued));
        for (old, next) in [
            (Draft, Held),
            (Draft, PendingApproval),
            (Draft, Queued),
            (Held, Draft),
            (Rejected, Draft),
            (PendingApproval, Rejected),
            (Queued, Sending),
            (Sending, DeliveryUnknown),
            (DeliveryUnknown, Sent),
            (Sending, Failed),
            (Failed, Queued),
            (Sending, Sent),
        ] {
            assert!(old.permits(next));
        }
    }
    #[tokio::test]
    async fn disabled_never_claims_success_and_stub_is_test_only() {
        let report = ConfirmedBugReport {
            content: content(),
            payload_hash: "hash".into(),
        };
        let id = DeliveryIdentity {
            id: uuid::Uuid::new_v4(),
            report_id: uuid::Uuid::new_v4(),
            submission_rev: 1,
        };
        assert_eq!(
            DisabledBugReportSink.send(&ctx(), &report, &id).await,
            SendOutcome::Disabled
        );
        assert_eq!(
            DisabledBugReportSink.reconcile(&ctx(), &id).await,
            ReconcileOutcome::Disabled
        );
        assert!(matches!(
            StubBugReportSink.send(&ctx(), &report, &id).await,
            SendOutcome::Sent { .. }
        ));
        assert!(matches!(
            StubBugReportSink.reconcile(&ctx(), &id).await,
            ReconcileOutcome::Found { .. }
        ));
    }
}

#[cfg(test)]
mod delivery_tests;
