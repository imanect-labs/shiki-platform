//! 承認ポリシの実効判定（#350）と再開材料の復元（#351）。
//!
//! `generate.rs` から分割（行数規約）。モード写像・クランプ判定そのものは
//! [`crate::autonomous`] に集約されており、ここは run（スナップショット）と
//! thread の現在値・org キャップを突き合わせて **実行時のポリシ**へ落とす層。

use authz::AuthContext;

use super::ChatWorker;
use crate::autonomous::AutonomousMode;
use crate::model::StreamEventKind;
use crate::store::ClaimedRun;
use crate::ChatError;

impl ChatWorker {
    /// 自律 run の実効承認ポリシを決める（#350）: run スナップショット×thread の現在モード×
    /// org キャップ（写像と実効判定は autonomous.rs に集約）。クランプ時は SSE で明示する
    /// （黙って降格しない・generation_event として replay/監査にも残る）。
    /// 戻り値は（スナップショットモード, 実効ポリシ）。
    pub(super) async fn autonomous_approval(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
    ) -> Result<(AutonomousMode, agent_core::ApprovalPolicy), ChatError> {
        let snapshot = AutonomousMode::parse(&run.autonomous_mode).unwrap_or_default();
        let (current, set_by) = self
            .store
            .thread_autonomous_mode(run.thread_id, &ctx.tenant_id)
            .await?;
        let bypass_allowed = self.store.autonomous_bypass_allowed(&ctx.tenant_id).await?;
        let (effective, clamp) = crate::autonomous::effective_mode(
            snapshot,
            current,
            set_by.as_deref(),
            &ctx.principal.id,
            bypass_allowed,
        );
        if let Some(clamp) = clamp {
            let _ = self
                .store
                .append_stream_event(
                    run.run_id,
                    run.fencing_token,
                    &StreamEventKind::FailureRecovery {
                        detail: clamp.detail().to_string(),
                        action: "mode_clamped".to_string(),
                    },
                )
                .await;
        }
        Ok((snapshot, effective.approval_policy()))
    }
}

/// 保存済みチェックポイント封筒を復元する（#351・自律 run のみ）。
///
/// 復元できない（旧形式等の）チェックポイントは警告して新規開始へフォールバックする
/// （run を止めない・副作用の収束は版管理と冪等キーが担う）。
pub(super) fn restore_checkpoint(run: &ClaimedRun) -> Option<super::sink::CheckpointEnvelope> {
    if !run.autonomous {
        return None;
    }
    run.checkpoint
        .as_ref()
        .and_then(|j| match serde_json::from_value(j.0.clone()) {
            Ok(envelope) => Some(envelope),
            Err(e) => {
                tracing::warn!(run_id = %run.run_id, error = %e,
                    "checkpoint の復元に失敗（新規開始へフォールバック）");
                None
            }
        })
}
