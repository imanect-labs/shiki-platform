//! 承認者アダプタ（Task 5.6）。
//!
//! agent-core の [`Approver`] を **durable run に配線**する。承認が必要になると run を
//! `waiting_approval` にし、`run_approval` に決定が入るまで（or キャンセルまで）**ブロック**する。
//! 待機中もハートビート（別タスク）がリースを延長するため、リースは失効しない。決定/キャンセルで
//! 走行状態へ戻して継続する。**タイムアウトは既定 deny**（承認なしに破壊系を実行しない・fail-safe）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_core::{ApprovalDecision, ApprovalPolicy, Approver};
use uuid::Uuid;

use crate::autonomous::{AutonomousMode, ModeClamp};
use crate::model::{RunStatus, StreamEventKind};
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
    /// 自律 run の承認モード再評価コンテキスト（実行中トグル対応・#350）。非自律は `None`。
    mode: Option<ModeContext>,
    /// 実行中クランプ通知の dedup（同じ理由を毎回再送せず**遷移時のみ** SSE へ出す・#350）。
    last_clamp: Mutex<Option<ModeClamp>>,
}

/// 実行中の承認モード再評価に必要な材料（thread の現在値と突き合わせる）。
struct ModeContext {
    thread_id: Uuid,
    tenant_id: String,
    /// run の実行主体（緩和はこの本人による設定のみ有効）。
    actor: String,
    /// メッセージ投入時点のモード（発話者が同意した水準）。
    snapshot: AutonomousMode,
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
            mode: None,
            last_clamp: Mutex::new(None),
        }
    }

    /// テスト用に間隔/上限を差し替える。
    #[must_use]
    pub fn with_timing(mut self, poll_interval: Duration, max_wait: Duration) -> Self {
        self.poll_interval = poll_interval;
        self.max_wait = max_wait;
        self
    }

    /// 自律 run の承認モード再評価を有効化する（実行中トグル対応・#350）。
    ///
    /// 各破壊系呼び出しの直前と承認待ちポーリング中に thread の現在モードを読み直し、
    /// [`crate::autonomous::effective_mode`]（厳格化は誰でも・緩和は actor 本人のみ・org キャップ）
    /// で実効ポリシを決める。
    #[must_use]
    pub fn with_autonomous_mode(
        mut self,
        thread_id: Uuid,
        tenant_id: String,
        actor: String,
        snapshot: AutonomousMode,
    ) -> Self {
        self.mode = Some(ModeContext {
            thread_id,
            tenant_id,
            actor,
            snapshot,
        });
        self
    }

    /// thread の現在モードから実効モードを再評価する（DB エラー時は `None`＝スナップショット継続）。
    ///
    /// クランプ（org が bypass を禁止・他ユーザーによる緩和の打ち消し）は run 開始時と同じく
    /// SSE `failure_recovery(mode_clamped)` で明示する（黙って降格しない・#350）。破壊系呼び出し
    /// ごとに再評価されるため、**理由が変化した時のみ** 1 回出す（dedup）。
    async fn refresh_effective_mode(&self) -> Option<AutonomousMode> {
        let mc = self.mode.as_ref()?;
        let (current, set_by) = self
            .store
            .thread_autonomous_mode(mc.thread_id, &mc.tenant_id)
            .await
            .ok()?;
        let bypass_allowed = self
            .store
            .autonomous_bypass_allowed(&mc.tenant_id)
            .await
            .ok()?;
        let (mode, clamp) = crate::autonomous::effective_mode(
            mc.snapshot,
            current,
            set_by.as_deref(),
            &mc.actor,
            bypass_allowed,
        );
        self.notify_clamp_transition(clamp).await;
        Some(mode)
    }

    /// クランプ理由の**遷移時のみ** SSE 通知を出す（解消時は状態だけ戻す・失敗は握り潰さず warn）。
    async fn notify_clamp_transition(&self, clamp: Option<ModeClamp>) {
        let changed_to = {
            let mut last = self
                .last_clamp
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *last == clamp {
                None
            } else {
                *last = clamp;
                clamp
            }
        };
        let Some(clamp) = changed_to else { return };
        if let Err(e) = self
            .store
            .append_stream_event(
                self.run_id,
                self.fencing_token,
                &StreamEventKind::FailureRecovery {
                    detail: clamp.detail().to_string(),
                    action: "mode_clamped".to_string(),
                },
            )
            .await
        {
            tracing::warn!(run_id = %self.run_id, error = %e, "mode_clamped 通知の追記に失敗");
        }
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
    /// 実行中トグル（#350）: 承認ゲートは各破壊系呼び出しの直前にこれを参照する。
    /// `None`（非自律 or 一時的 DB エラー）は run 開始時のスナップショットへフォールバック。
    async fn current_policy(&self) -> Option<ApprovalPolicy> {
        Some(self.refresh_effective_mode().await?.approval_policy())
    }

    async fn decide(
        &self,
        tool_call_id: &str,
        name: &str,
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
            // 承認待ち中にモードが緩和されたら自動承認する（実行中トグル・#350。緩和は run の
            // actor 本人による設定のみ有効＝effective_mode が保証。承認カードは resolved で閉じる）。
            if let Some(mode) = self.refresh_effective_mode().await {
                if mode.approval_policy().is_pre_authorized(name) {
                    self.resume_running().await;
                    return ApprovalDecision::Approved;
                }
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
