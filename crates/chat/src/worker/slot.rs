//! 生成ワーカーの枠（同時に生成する run の数）と、承認待ちの間の枠の返却（検証 C-P1-3）。
//!
//! 以前は「ワーカータスク数 = 同時に処理する run 数」で、承認待ちの run が最大 30 分タスクごと枠を
//! 握ったため、未回答の承認が 4 件あると全ユーザー（他テナントを含む）のチャットが止まった。
//! いまは**枠をセマフォで持ち**、承認待ちに入った run は枠を返して保留（park）し、決定・キャンセル・
//! 期限切れで枠を取り直して再開する。保留中の run はタスクとして待つだけ（生成・ツール実行はしない）。
//!
//! 保留できる run の数には上限がある（[`ParkLimits`]・R3-5）:
//!
//! - **プロセス全体**で [`MAX_PARKED_APPROVALS`] 件まで。ワーカーのタスク数は「枠 + この上限 + 1」
//!   （[`worker_tasks`]）なので、保留が上限まで溜まっても、ジョブを取りに行けるタスクが常に枠の数より
//!   多く残る（承認待ちが全タスクを塞いで全テナントのチャットが止まることはない）。
//! - **ユーザーごと**に [`MAX_PARKED_APPROVALS_PER_USER`] 件まで（1 人の未回答の承認がプロセスの
//!   上限を食い尽くさない）。
//!
//! 上限を超える承認要求は**待たずに却下**する（枠を握ったまま待たない・既存の却下・期限切れと同じ
//! 経路。理由は承認者がストリームに知らせる）。保留の数え上げは、再開して枠を取り直すまで続く
//! （取り直しを待つ間もタスクはジョブを取りに行けないため）。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// 承認待ちで枠を返して保留できる run の数（プロセスあたり）。
pub const MAX_PARKED_APPROVALS: usize = 64;

/// 承認待ちで保留できる run の数（ユーザーあたり・テナント × ユーザー）。
pub const MAX_PARKED_APPROVALS_PER_USER: usize = 5;

/// 枠が `concurrency` のときに起動するワーカーのタスク数（枠 + 保留の上限 + 1）。
///
/// 保留の上限はタスク数 − 枠**未満**なので、保留が上限まで溜まってもジョブを取りに行けるタスクが
/// 枠の数より多く残る。
#[must_use]
pub fn worker_tasks(concurrency: usize) -> usize {
    concurrency.max(1) + MAX_PARKED_APPROVALS + 1
}

/// 保留できなかった理由（承認者が却下の理由として知らせる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkRefusal {
    /// このユーザーの承認待ちが上限に達している。
    PerUser,
    /// プロセス全体の承認待ちが上限に達している。
    Process,
}

impl ParkRefusal {
    /// 利用者向けの文言（ストリームの `failure_recovery` に出す）。
    #[must_use]
    pub fn detail(self) -> String {
        match self {
            ParkRefusal::PerUser => format!(
                "承認待ちが多すぎるため実行しませんでした（同時に承認を待てるのは 1 人あたり \
                 {MAX_PARKED_APPROVALS_PER_USER} 件までです。他の承認に回答してからやり直してください）"
            ),
            ParkRefusal::Process => "承認待ちが多すぎるため実行しませんでした（混み合っています。\
                 しばらくしてからやり直してください）"
                .to_string(),
        }
    }
}

/// 承認待ちの保留の上限（プロセス全体・ユーザーごと）の数え上げ。
pub struct ParkLimits {
    total_cap: usize,
    per_user_cap: usize,
    counts: std::sync::Mutex<ParkCounts>,
}

#[derive(Default)]
struct ParkCounts {
    total: usize,
    per_user: HashMap<(String, String), usize>,
}

impl ParkLimits {
    /// 既定の上限（[`MAX_PARKED_APPROVALS`]・[`MAX_PARKED_APPROVALS_PER_USER`]）。
    #[must_use]
    pub fn standard() -> Arc<Self> {
        Self::new(MAX_PARKED_APPROVALS, MAX_PARKED_APPROVALS_PER_USER)
    }

    #[must_use]
    pub fn new(total_cap: usize, per_user_cap: usize) -> Arc<Self> {
        Arc::new(ParkLimits {
            total_cap,
            per_user_cap,
            counts: std::sync::Mutex::new(ParkCounts::default()),
        })
    }

    /// 1 件の保留を予約する（上限を超えるなら拒否）。返した札を落とすと数え上げから外れる。
    fn try_reserve(self: &Arc<Self>, tenant: &str, user: &str) -> Result<ParkTicket, ParkRefusal> {
        let key = (tenant.to_string(), user.to_string());
        let mut c = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mine = c.per_user.get(&key).copied().unwrap_or(0);
        if mine >= self.per_user_cap {
            return Err(ParkRefusal::PerUser);
        }
        if c.total >= self.total_cap {
            return Err(ParkRefusal::Process);
        }
        c.total += 1;
        *c.per_user.entry(key.clone()).or_insert(0) += 1;
        Ok(ParkTicket {
            limits: Arc::clone(self),
            key,
        })
    }

    fn release(&self, key: &(String, String)) {
        let mut c = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per_user.get_mut(key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.per_user.remove(key);
            }
        }
    }

    /// いま保留している件数（テスト・観測用）。
    #[cfg(test)]
    pub(crate) fn parked(&self) -> usize {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total
    }
}

/// 保留 1 件の札（落とすと数え上げから外れる）。
struct ParkTicket {
    limits: Arc<ParkLimits>,
    key: (String, String),
}

impl Drop for ParkTicket {
    fn drop(&mut self) {
        self.limits.release(&self.key);
    }
}

/// 1 run が使う枠（承認待ちの間だけ返す）。
pub struct RunSlot {
    sem: Arc<Semaphore>,
    limits: Arc<ParkLimits>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
    ticket: Mutex<Option<ParkTicket>>,
}

impl RunSlot {
    pub fn new(
        sem: Arc<Semaphore>,
        limits: Arc<ParkLimits>,
        permit: OwnedSemaphorePermit,
    ) -> Arc<Self> {
        Arc::new(RunSlot {
            sem,
            limits,
            permit: Mutex::new(Some(permit)),
            ticket: Mutex::new(None),
        })
    }

    /// 枠を返す（承認待ちに入る）。返した枠で他の run が生成できる。
    ///
    /// 保留の上限（プロセス全体・`tenant` × `user` ごと）に達していれば**枠を返さずに**拒否する
    /// （呼び出し側は待たずに却下する）。既に保留中なら何もしない。
    pub async fn park(&self, tenant: &str, user: &str) -> Result<(), ParkRefusal> {
        let mut t = self.ticket.lock().await;
        if t.is_none() {
            *t = Some(self.limits.try_reserve(tenant, user)?);
        }
        self.permit.lock().await.take();
        Ok(())
    }

    /// 枠を取り直す（承認待ちから戻る）。空くまで待つ。取り直してから保留の数え上げを外す。
    pub async fn resume(&self) {
        let mut g = self.permit.lock().await;
        if g.is_none() {
            if let Ok(p) = Arc::clone(&self.sem).acquire_owned().await {
                *g = Some(p);
            }
        }
        self.ticket.lock().await.take();
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

    async fn slot(sem: &Arc<Semaphore>, limits: &Arc<ParkLimits>) -> Arc<RunSlot> {
        let p = Arc::clone(sem).acquire_owned().await.unwrap();
        RunSlot::new(Arc::clone(sem), Arc::clone(limits), p)
    }

    #[tokio::test]
    async fn parking_returns_the_slot_and_resume_takes_it_back() {
        let sem = Arc::new(Semaphore::new(1));
        let limits = ParkLimits::standard();
        let slot = slot(&sem, &limits).await;
        assert_eq!(sem.available_permits(), 0);
        slot.park("t", "u").await.unwrap();
        assert_eq!(sem.available_permits(), 1, "承認待ちの間は枠を返す");
        assert!(!slot.holding().await);
        assert_eq!(limits.parked(), 1);
        // 他の run が枠を使っている間は、再開は空くまで待つ（その間も保留に数える）。
        let other = Arc::clone(&sem).acquire_owned().await.unwrap();
        let s2 = Arc::clone(&slot);
        let h = tokio::spawn(async move { s2.resume().await });
        tokio::task::yield_now().await;
        assert!(!h.is_finished());
        assert_eq!(limits.parked(), 1);
        drop(other);
        h.await.unwrap();
        assert!(slot.holding().await);
        assert_eq!(sem.available_permits(), 0);
        assert_eq!(limits.parked(), 0, "枠を取り直したら保留から外れる");
    }

    /// R3-5: ユーザーごとの上限を超える保留は拒否し、枠は返さない（他のユーザーは保留できる）。
    #[tokio::test]
    async fn per_user_cap_refuses_without_returning_the_slot() {
        let sem = Arc::new(Semaphore::new(4));
        let limits = ParkLimits::new(10, 2);
        let a1 = slot(&sem, &limits).await;
        let a2 = slot(&sem, &limits).await;
        let a3 = slot(&sem, &limits).await;
        a1.park("t", "alice").await.unwrap();
        a2.park("t", "alice").await.unwrap();
        assert_eq!(
            a3.park("t", "alice").await,
            Err(ParkRefusal::PerUser),
            "1 人あたりの上限"
        );
        assert!(a3.holding().await, "拒否された run は枠を返さない");
        // 別テナントの同名ユーザー・別ユーザーは別に数える。
        let b = slot(&sem, &limits).await;
        b.park("t2", "alice").await.unwrap();
        assert_eq!(limits.parked(), 3);
        // 1 件が終われば（札を落とせば）また保留できる。
        drop(a1);
        a3.park("t", "alice").await.unwrap();
    }

    /// R3-5: プロセス全体の上限を超える保留は拒否する。上限はタスク数 − 枠より小さい。
    #[tokio::test]
    async fn process_cap_refuses_and_leaves_tasks_to_claim() {
        let sem = Arc::new(Semaphore::new(3));
        let limits = ParkLimits::new(2, 5);
        let s1 = slot(&sem, &limits).await;
        let s2 = slot(&sem, &limits).await;
        let s3 = slot(&sem, &limits).await;
        s1.park("t", "a").await.unwrap();
        s2.park("t", "b").await.unwrap();
        assert_eq!(s3.park("t", "c").await, Err(ParkRefusal::Process));
        assert!(ParkRefusal::Process.detail().contains("承認待ちが多すぎる"));
        // 既定の上限: 保留が上限まで溜まっても、ジョブを取りに行けるタスクが枠より多く残る。
        for c in [1, 4, 16] {
            assert!(worker_tasks(c) - MAX_PARKED_APPROVALS > c);
        }
    }
}
