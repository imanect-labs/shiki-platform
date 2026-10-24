//! 生成ワーカーの枠（同時に生成する run の数）と、承認待ちの間の枠の返却（検証 C-P1-3）。
//!
//! 以前は「ワーカータスク数 = 同時に処理する run 数」で、承認待ちの run が最大 30 分タスクごと枠を
//! 握ったため、未回答の承認が 4 件あると全ユーザー（他テナントを含む）のチャットが止まった。
//! いまは**枠をセマフォで持ち**、承認待ちに入った run は枠を返して保留（park）し、決定・キャンセル・
//! 期限切れで枠を取り直して再開する。保留中の run はタスクとして待つだけ（生成・ツール実行はしない）。
//! 保留できる run の数には上限（[`MAX_PARKED_APPROVALS`]）があり、それを超える分は従来どおり枠を
//! 握ったまま待つ（タスク数 = 枠 + 保留の上限）。

use std::sync::Arc;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// 承認待ちで枠を返して保留できる run の数（プロセスあたり）。
pub const MAX_PARKED_APPROVALS: usize = 64;

/// 1 run が使う枠（承認待ちの間だけ返す）。
pub struct RunSlot {
    sem: Arc<Semaphore>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

impl RunSlot {
    pub fn new(sem: Arc<Semaphore>, permit: OwnedSemaphorePermit) -> Arc<Self> {
        Arc::new(RunSlot {
            sem,
            permit: Mutex::new(Some(permit)),
        })
    }

    /// 枠を返す（承認待ちに入る）。返した枠で他の run が生成できる。
    pub async fn park(&self) {
        self.permit.lock().await.take();
    }

    /// 枠を取り直す（承認待ちから戻る）。空くまで待つ。
    pub async fn resume(&self) {
        let mut g = self.permit.lock().await;
        if g.is_none() {
            if let Ok(p) = Arc::clone(&self.sem).acquire_owned().await {
                *g = Some(p);
            }
        }
    }

    /// いま枠を持っているか（テスト・観測用）。
    #[cfg(test)]
    pub(crate) async fn holding(&self) -> bool {
        self.permit.lock().await.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parking_returns_the_slot_and_resume_takes_it_back() {
        let sem = Arc::new(Semaphore::new(1));
        let p = Arc::clone(&sem).acquire_owned().await.unwrap();
        let slot = RunSlot::new(Arc::clone(&sem), p);
        assert_eq!(sem.available_permits(), 0);
        slot.park().await;
        assert_eq!(sem.available_permits(), 1, "承認待ちの間は枠を返す");
        assert!(!slot.holding().await);
        // 他の run が枠を使っている間は、再開は空くまで待つ。
        let other = Arc::clone(&sem).acquire_owned().await.unwrap();
        let s2 = Arc::clone(&slot);
        let h = tokio::spawn(async move { s2.resume().await });
        tokio::task::yield_now().await;
        assert!(!h.is_finished());
        drop(other);
        h.await.unwrap();
        assert!(slot.holding().await);
        assert_eq!(sem.available_permits(), 0);
    }
}
