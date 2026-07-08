//! 承認者アダプタ（Task 5.6）。
//!
//! agent-core の [`Approver`] を **durable run に配線**する。承認が必要になると run を
//! `waiting_approval` にし、`run_approval` に決定が入るまで（or キャンセルまで）**ブロック**する。
//! 待機中もハートビート（別タスク）がリースを延長するため、リースは失効しない。決定/キャンセルで
//! 走行状態へ戻して継続する。**タイムアウトは既定 deny**（承認なしに破壊系を実行しない・fail-safe）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::{ApprovalDecision, Approver};
use uuid::Uuid;

use crate::model::RunStatus;
use crate::store::ChatStore;

/// 決定ポーリング間隔（既定）。
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 承認待ちの最大時間（既定・超過は deny）。
const MAX_WAIT: Duration = Duration::from_mins(30);

/// `run_approval` を待つ承認者。1 run に紐づく（run_id＋fencing でゾンビ書込を防ぐ）。
pub struct DbApprover {
    store: ChatStore,
    run_id: Uuid,
    fencing_token: i64,
    cancel: Arc<AtomicBool>,
    poll_interval: Duration,
    max_wait: Duration,
}

impl DbApprover {
    /// 本番構築（既定のポーリング/上限）。
    pub fn new(
        store: ChatStore,
        run_id: Uuid,
        fencing_token: i64,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        DbApprover {
            store,
            run_id,
            fencing_token,
            cancel,
            poll_interval: POLL_INTERVAL,
            max_wait: MAX_WAIT,
        }
    }

    /// テスト用に間隔/上限を差し替える。
    #[must_use]
    pub fn with_timing(mut self, poll_interval: Duration, max_wait: Duration) -> Self {
        self.poll_interval = poll_interval;
        self.max_wait = max_wait;
        self
    }

    /// 走行状態へ戻す（決定後の継続用・fencing 一致時のみ）。
    async fn resume_running(&self) {
        let _ = self
            .store
            .set_run_status_fenced(self.run_id, self.fencing_token, RunStatus::Running)
            .await;
    }
}

#[async_trait::async_trait]
impl Approver for DbApprover {
    async fn decide(
        &self,
        tool_call_id: &str,
        _name: &str,
        _input: &serde_json::Value,
    ) -> ApprovalDecision {
        // 承認待ちへ遷移（fencing 不一致なら no-op＝別ワーカーが takeover 済み）。
        let _ = self
            .store
            .set_run_status_fenced(self.run_id, self.fencing_token, RunStatus::WaitingApproval)
            .await;

        let start = Instant::now();
        loop {
            // 明示キャンセル（共有フラグ or DB フラグ）で待機を打ち切る。
            let db_cancel = self
                .store
                .is_cancel_requested(self.run_id)
                .await
                .unwrap_or(false);
            if self.cancel.load(Ordering::Relaxed) || db_cancel {
                self.resume_running().await;
                return ApprovalDecision::Cancelled;
            }
            // 決定が入っていれば適用する。
            if let Ok(Some(approved)) = self.store.poll_approval(self.run_id, tool_call_id).await {
                self.resume_running().await;
                return if approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Rejected
                };
            }
            // 未決 or 一時的な DB エラーは次ループで再試行（待機継続）。
            // タイムアウトは既定 deny（承認なしに破壊系を実行しない）。
            if start.elapsed() >= self.max_wait {
                self.resume_running().await;
                return ApprovalDecision::Rejected;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}
