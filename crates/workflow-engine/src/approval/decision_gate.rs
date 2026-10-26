//! 判断の受付の同時実行の枠（プロセス内・判断 API の [`super::ApprovalStore::begin_decision`]）。
//!
//! 判断の受付は、台帳の行のロックを持った接続を 1 本持ったまま、承認者本人の遷移（data の Tx）と監査でもう 1 本を
//! 使う。どちらも同じプールから取るので、同時に走る受付がプールの上限に近づくと、全員が 1 本目を持ったまま
//! 2 本目を待ち、サーバ全体が接続を取れなくなる。そこで、
//!
//! - 受付の同時実行数を、プールの上限より十分小さい枠（上限の 4 分の 1・最低 1）に抑える。枠は接続を取る前に
//!   取り、接続を持たずに待つ（待ち切れなければ「混み合っている」）。
//! - 同じ承認への重複した要求は、このプロセスで受付が進んでいれば枠も接続も取らずに「判断中」で返す（別の
//!   プロセスの受付とは、台帳の行の `FOR UPDATE NOWAIT` で待たずに見分ける）。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

/// 枠が空くのを待つ上限。
const SLOT_WAIT: Duration = Duration::from_secs(3);

/// プールの上限 `max_connections` に対する受付の枠の数（上限の 4 分の 1・最低 1）。
pub(super) fn slots_for(max_connections: u32) -> usize {
    usize::try_from(max_connections / 4).unwrap_or(1).max(1)
}

/// 判断の受付の枠と、受付中の承認。
pub(super) struct DecisionGate {
    slots: Arc<Semaphore>,
    in_flight: Mutex<HashSet<(String, Uuid)>>,
}

/// 受付を始められなかった理由。
pub(super) enum Refused {
    /// 同じ承認の受付がこのプロセスで進んでいる。
    Busy,
    /// 枠が空かなかった。
    Crowded,
}

impl DecisionGate {
    pub(super) fn new(slots: usize) -> Self {
        DecisionGate {
            slots: Arc::new(Semaphore::new(slots)),
            in_flight: Mutex::new(HashSet::new()),
        }
    }

    /// 承認 1 件の受付の枠を取る（重複は即座に [`Refused::Busy`]・枠は接続を持たずに待つ）。
    pub(super) async fn enter(
        self: &Arc<Self>,
        tenant_id: &str,
        approval_id: Uuid,
    ) -> Result<DecisionHold, Refused> {
        let key = (tenant_id.to_string(), approval_id);
        if !self.lock().insert(key.clone()) {
            return Err(Refused::Busy);
        }
        // 先に受付中の印を付け、枠を待てなかったら印を外す（`claim` の drop）。
        let claim = Claim {
            gate: Arc::clone(self),
            key,
        };
        let slot = tokio::time::timeout(SLOT_WAIT, Arc::clone(&self.slots).acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or(Refused::Crowded)?;
        Ok(DecisionHold {
            _slot: slot,
            _claim: claim,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<(String, Uuid)>> {
        // 印の集合は挿入と削除しかしないので、毒された Mutex でも中身はそのまま使える。
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 受付中の印（drop で外す）。
struct Claim {
    gate: Arc<DecisionGate>,
    key: (String, Uuid),
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.gate.lock().remove(&self.key);
    }
}

/// 受付の枠と受付中の印（判断の受付の Tx が持ち、確定・取り消し・drop で返す）。
pub(super) struct DecisionHold {
    _slot: OwnedSemaphorePermit,
    _claim: Claim,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_a_quarter_of_the_pool() {
        assert_eq!(slots_for(0), 1);
        assert_eq!(slots_for(5), 1);
        assert_eq!(slots_for(8), 2);
        assert_eq!(slots_for(10), 2);
        assert_eq!(slots_for(40), 10);
    }

    #[tokio::test]
    async fn duplicates_are_busy_and_slots_bound_the_rest() {
        let gate = Arc::new(DecisionGate::new(1));
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let held = gate.enter("t", a).await;
        assert!(held.is_ok(), "枠が空いている");
        assert!(matches!(gate.enter("t", a).await, Err(Refused::Busy)));
        // 別の承認は枠が空くまで待つ（待ち切れなければ混み合っている）。
        let waiting = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.enter("t", b).await.is_ok() }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "枠が空くまで待つ");
        drop(held);
        assert!(waiting.await.unwrap(), "枠が空いたら受け付ける");
        // 返した印は外れている（同じ承認をまた受け付けられる）。
        assert!(gate.enter("t", a).await.is_ok());
    }
}
