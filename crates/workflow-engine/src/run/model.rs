//! run/step の状態機械（engine.md §3）。

use serde::{Deserialize, Serialize};

/// run の状態（engine.md §3.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => RunStatus::Queued,
            "running" => RunStatus::Running,
            "succeeded" => RunStatus::Succeeded,
            "failed" => RunStatus::Failed,
            "cancelled" => RunStatus::Cancelled,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

/// step の状態（engine.md §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Ready,
    Running,
    WaitingTimer,
    WaitingEvent,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

impl StepStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Ready => "ready",
            StepStatus::Running => "running",
            StepStatus::WaitingTimer => "waiting_timer",
            StepStatus::WaitingEvent => "waiting_event",
            StepStatus::Succeeded => "succeeded",
            StepStatus::Failed => "failed",
            StepStatus::Skipped => "skipped",
            StepStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => StepStatus::Pending,
            "ready" => StepStatus::Ready,
            "running" => StepStatus::Running,
            "waiting_timer" => StepStatus::WaitingTimer,
            "waiting_event" => StepStatus::WaitingEvent,
            "succeeded" => StepStatus::Succeeded,
            "failed" => StepStatus::Failed,
            "skipped" => StepStatus::Skipped,
            "cancelled" => StepStatus::Cancelled,
            _ => return None,
        })
    }

    /// terminal（output/taken_ports 確定・再実行しない）か。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            StepStatus::Succeeded
                | StepStatus::Failed
                | StepStatus::Skipped
                | StepStatus::Cancelled
        )
    }
}

/// 冪等キーを組む（`wf:{tenant_id}:{run_id}:{step_path}`・attempt 非依存・engine.md §7.2）。
pub fn idempotency_key(tenant_id: &str, run_id: uuid::Uuid, step_path: &str) -> String {
    format!("wf:{tenant_id}:{run_id}:{step_path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip() {
        for s in [RunStatus::Queued, RunStatus::Running, RunStatus::Failed] {
            assert_eq!(RunStatus::parse(s.as_str()), Some(s));
        }
        for s in [StepStatus::Pending, StepStatus::Ready, StepStatus::Skipped] {
            assert_eq!(StepStatus::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn terminal_classification() {
        assert!(RunStatus::Succeeded.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
        assert!(StepStatus::Failed.is_terminal());
        assert!(!StepStatus::Ready.is_terminal());
    }

    #[test]
    fn idempotency_key_shape() {
        let id = uuid::Uuid::nil();
        assert_eq!(
            idempotency_key("t1", id, "node_a"),
            format!("wf:t1:{id}:node_a")
        );
    }
}
