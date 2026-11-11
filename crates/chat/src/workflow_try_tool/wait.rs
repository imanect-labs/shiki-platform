//! 試運転の run を待つ（[`super::TryWorkflowTool`]・T04）。
//!
//! - 待つ前に生成の枠を返す（[`RunSlot::park`]・承認待ちと同じ保留の上限を共有する）。待つ間、別の会話は生成を
//!   進められる。保留の上限で断られたら待たずに返す（[`WaitEnd::ParkRefused`]）。
//! - 待つ間は [`RunOutcomeReader::live_states`] を、最初の間隔から上限の間隔まで倍にしながら読む。終端になるか、
//!   待っているもの（承認・できごと・時間）が出たら返す。社内文書の取り込み待ち（index）と繰り返しの要素の待ちは
//!   run としては実行中なので待ち続ける。
//! - 待つ上限を過ぎたら、その時点の状態で返す（順番待ち・実行中）。run も台帳の行もそのまま残す。
//! - 停止（cancel）が立ったら待つのをやめる。run は取り消さない（作ったものは画面で止める）。
//! - 抜けるときは必ず枠を取り直す（[`RunSlot::resume`]・空くまで待ち、停止を見ない）。取り直した後、ループが停止を
//!   見て止まる。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use authz::AuthContext;
use tokio::time::Instant;
use uuid::Uuid;
use workflow_engine::{LiveState, RunOutcomeReader, RunStatus, WaitingFor};

use crate::worker::slot::RunSlot;
use crate::worker::TryWaitLimits;

/// 停止を見る間隔（読む間隔より短く・待つ間も停止にすぐ応える）。
const CANCEL_CHECK: Duration = Duration::from_millis(100);

/// 待つのをやめた理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WaitEnd {
    /// 終わった（成功・失敗・取り消し）か、待っているもの（承認・できごと・時間）が出た。
    Settled,
    /// 上限まで待っても終わらなかった（順番待ち・実行中）。
    TimedOut,
    /// 停止の指示で待つのをやめた（run は取り消していない）。
    Cancelled,
    /// 保留の上限（承認待ちと共有）で枠を返せなかったので、待たなかった。
    ParkRefused,
    /// run が無い（実行履歴の GC で消えた）か、状態を読めなかった。
    Unknown,
}

/// これ以上待たなくてよい状態か（終端か、人・できごと・時間を待っている）。
pub(super) fn settled(state: LiveState) -> bool {
    state.status.is_terminal()
        || (state.status == RunStatus::Running
            && matches!(
                state.waiting_for,
                Some(WaitingFor::Approval | WaitingFor::Event | WaitingFor::Timer)
            ))
}

/// 待つ相手（読む口・枠・停止の旗・上限）。
pub(super) struct Waiter<'a> {
    pub outcomes: &'a RunOutcomeReader,
    pub slot: &'a Arc<RunSlot>,
    pub cancel: &'a AtomicBool,
    pub limits: TryWaitLimits,
}

impl Waiter<'_> {
    /// run を待つ（上のモジュールの説明）。最後に読めた状態も返す。
    pub(super) async fn wait(
        &self,
        ctx: &AuthContext,
        run_id: Uuid,
    ) -> (WaitEnd, Option<LiveState>) {
        let first = self.read(ctx, run_id).await;
        match first {
            None => return (WaitEnd::Unknown, None),
            Some(s) if settled(s) => return (WaitEnd::Settled, Some(s)),
            Some(_) => {}
        }
        if self.cancel.load(Ordering::Relaxed) {
            return (WaitEnd::Cancelled, first);
        }
        if self
            .slot
            .park(ctx.tenant_id(), &ctx.principal().id)
            .await
            .is_err()
        {
            return (WaitEnd::ParkRefused, first);
        }
        let end = self.poll(ctx, run_id, first).await;
        // 抜けるときは必ず枠を取り直す（空くまで待つ・停止は見ない）。
        self.slot.resume().await;
        end
    }

    /// 枠を返した間の読み直し（終わる・待ちが出る・上限・停止のどれかまで）。
    async fn poll(
        &self,
        ctx: &AuthContext,
        run_id: Uuid,
        mut last: Option<LiveState>,
    ) -> (WaitEnd, Option<LiveState>) {
        let deadline = Instant::now() + self.limits.max_wait;
        let mut interval = self.limits.first_poll.max(Duration::from_millis(10));
        loop {
            let wake = (Instant::now() + interval).min(deadline);
            while Instant::now() < wake {
                if self.cancel.load(Ordering::Relaxed) {
                    return (WaitEnd::Cancelled, last);
                }
                let left = wake.saturating_duration_since(Instant::now());
                tokio::time::sleep(left.min(CANCEL_CHECK)).await;
            }
            if self.cancel.load(Ordering::Relaxed) {
                return (WaitEnd::Cancelled, last);
            }
            match self.read(ctx, run_id).await {
                None => return (WaitEnd::Unknown, last),
                Some(s) if settled(s) => return (WaitEnd::Settled, Some(s)),
                Some(s) => last = Some(s),
            }
            if Instant::now() >= deadline {
                return (WaitEnd::TimedOut, last);
            }
            interval = (interval * 2).min(self.limits.max_poll.max(interval));
        }
    }

    /// 今の状態（無い・読めないは `None`）。
    async fn read(&self, ctx: &AuthContext, run_id: Uuid) -> Option<LiveState> {
        match self.outcomes.live_states(ctx, &[run_id]).await {
            Ok(mut states) => states.remove(&run_id),
            Err(e) => {
                tracing::warn!(%run_id, error = %e, "試運転の run の状態を読めない（待つのをやめる）");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: RunStatus, waiting_for: Option<WaitingFor>) -> LiveState {
        LiveState {
            status,
            waiting_for,
        }
    }

    /// 終端と、人・できごと・時間の待ちは待たない。取り込み待ち（index）と実行中・順番待ちは待つ。
    #[test]
    fn settled_states() {
        for s in [
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert!(settled(state(s, None)), "{s:?}");
        }
        for w in [WaitingFor::Approval, WaitingFor::Event, WaitingFor::Timer] {
            assert!(settled(state(RunStatus::Running, Some(w))), "{w:?}");
        }
        assert!(!settled(state(
            RunStatus::Running,
            Some(WaitingFor::Index)
        )));
        assert!(!settled(state(RunStatus::Running, None)));
        assert!(!settled(state(RunStatus::Queued, None)));
    }
}
