use crate::Error;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BugReportStatus {
    Draft,
    PendingApproval,
    Queued,
    Held,
    Rejected,
    Cancelled,
    Sending,
    Sent,
    Failed,
    DeliveryUnknown,
}
impl BugReportStatus {
    /// 配送遷移は PR2 の内部 worker 用。利用者向けサービスからは実行不能。
    pub fn permits(self, next: Self) -> bool {
        use BugReportStatus::{
            Cancelled, DeliveryUnknown, Draft, Failed, Held, PendingApproval, Queued, Rejected,
            Sending, Sent,
        };
        matches!(
            (self, next),
            (Draft, PendingApproval | Queued | Held | Cancelled)
                | (PendingApproval, Queued | Rejected | Cancelled | Held)
                | (Queued, Sending | Cancelled | Held | PendingApproval)
                | (Sending, Sent | Failed | DeliveryUnknown)
                | (Failed, Queued)
                | (DeliveryUnknown, Sent | Failed)
                | (Held, Draft | Cancelled)
                | (Rejected, Draft)
        )
    }
}

/// 添付・会話参照・任意環境 JSON は受け取らない閉じた DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportContent {
    pub category: Category,
    pub title: String,
    pub symptom: String,
    pub steps_tried: String,
    pub expected_behavior: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Bug,
    Usage,
    Other,
}
impl ReportContent {
    pub fn validate(&self) -> Result<(), Error> {
        if self.title.trim().is_empty() || self.title.chars().count() > 200 {
            return Err(Error::Invalid);
        }
        for text in [
            &self.title,
            &self.symptom,
            &self.steps_tried,
            &self.expected_behavior,
        ] {
            if ["sk-", "ghp_", "github_pat_", "-----BEGIN", "Bearer "]
                .iter()
                .any(|prefix| text.contains(prefix))
                || text.chars().count() > 2000
                || text.contains(['<', '>'])
                || text.contains("://")
                || text.contains("www.")
                || text.contains("![")
                || text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                return Err(Error::Invalid);
            }
        }
        if serde_json::to_vec(self).map_err(|_| Error::Invalid)?.len() > 16 * 1024 {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub(crate) fn metadata_only(&self) -> Self {
        Self {
            category: self.category,
            title: "不具合報告（カテゴリのみ）".into(),
            symptom: String::new(),
            steps_tried: String::new(),
            expected_behavior: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BugReportPolicy {
    pub enabled: bool,
    pub require_approval: bool,
    pub allow_content_sharing: bool,
    pub rev: i64,
}
impl Default for BugReportPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            require_approval: true,
            allow_content_sharing: false,
            rev: 0,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BugReport {
    pub id: Uuid,
    pub reporter_id: String,
    pub status: BugReportStatus,
    pub rev: i64,
    pub submission_rev: i64,
    pub content: ReportContent,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Submit {
        policy_rev: i64,
        consent: bool,
        notice_version: String,
    },
    Approve,
    Retry,
    Reject {
        reason: String,
    },
    Cancel,
    Reopen,
}

impl BugReportPolicy {
    /// 宛先が有効でも policy と本人の確認が先行する。disabled は常に held。
    pub fn submitted_status(&self, destination_available: bool) -> BugReportStatus {
        if !destination_available {
            BugReportStatus::Held
        } else if self.require_approval {
            BugReportStatus::PendingApproval
        } else {
            BugReportStatus::Queued
        }
    }
}

/// 一覧には本文を含めない。
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ReportSummary {
    pub id: Uuid,
    pub status: BugReportStatus,
    pub rev: i64,
    pub title: String,
}
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ReportPage {
    pub items: Vec<ReportSummary>,
    pub next_cursor: Option<Uuid>,
}
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ReportDetail {
    pub report: BugReport,
    pub submission: Option<SubmissionDetail>,
}
#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct SubmissionDetail {
    pub content: ReportContent,
    pub content_consent: bool,
    pub sink_profile_id: String,
    pub sink_config_version: i64,
    pub notice_version: String,
    pub policy_rev: i64,
    pub consent_at: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
    pub rejected_by: Option<String>,
    pub rejected_at: Option<String>,
    pub rejection_reason: Option<String>,
}
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ReporterPolicy {
    pub policy: BugReportPolicy,
    pub destination_available: bool,
    pub destination_version: Option<i64>,
}
