//! 待機中の consumer を LISTEN/NOTIFY で起こす（#1332）。
//!
//! 空のキューを短い間隔で取りに行くと、ジョブが無い間（顧客の居ない時間帯・小さな配備）も問い合わせが
//! 止まらない。そこで投入（[`crate::enqueue_on`] / [`crate::requeue_dead`]）が同じ文で
//! `pg_notify('jobq', キュー名)` を送り、consumer はキューが空なら [`Waker::wait`] で眠る。
//! NOTIFY はコミット時にだけ配られるので、トランザクショナル enqueue の性質（コミット前のジョブを
//! 誰も取らない）は崩れない。同じトランザクション内の同じ通知は Postgres が 1 通にまとめる。
//!
//! # 通知だけでは起きられない経路
//!
//! 通知は「新しいジョブが積まれた」ことしか伝えない。次の 2 つは別の手当てで拾う:
//!
//! - **時刻で見えるようになるジョブ**（[`crate::fail`] のバックオフ明け・[`crate::defer`] の先送り明け・
//!   consumer のクラッシュ後の可視期限切れ）: 眠る前にキューの最小 `visible_at` を引き、その時刻までしか
//!   眠らない（[`DUE_FLOOR`] を下限にする: 取り出し中のトランザクションが押さえている行は他の接続から
//!   「もう見えている」ように読めるため、0 まで縮めると押さえている間ずっと空回りする）。
//! - **通知の取りこぼし**（LISTEN 接続が切れている間の通知は失われる）: 張り直した直後に、待っている
//!   consumer を全て起こす（起きた consumer は取りに行き、空ならまた眠る）。
//!
//! LISTEN 接続が無い・張れていない間は、呼び出し側が渡した `poll`（従来のポーリング間隔）で眠る。
//! 通知が届く前提が崩れているときに長く眠らない。
//!
//! # 接続
//!
//! `PgListener` はプールから 1 本借りて持ち続ける。consumer ごとに作るとワーカーの種類の数だけ接続を
//! 占有するので、プロセスで 1 つ作って全ワーカーで共有する（`Clone` は参照カウントだけ）。
//! テナントのプールが分かれても（`PoolRouter::all`）、プールごとに 1 本で足りる。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use sqlx::postgres::PgListener;
use sqlx::PgPool;
use tokio::sync::Notify;

/// 投入の通知に使うチャネル（ペイロードはキュー名）。
pub const CHANNEL: &str = "jobq";

/// LISTEN が生きているときに眠る上限の既定値。通知と最小 `visible_at` で起きるので、これは
/// 「どちらも働かなかったとき」の保険でしかない。
pub const DEFAULT_IDLE_MAX: Duration = Duration::from_mins(5);

/// 次に見えるようになるジョブまで眠るときの下限（モジュール冒頭）。
pub const DUE_FLOOR: Duration = Duration::from_secs(1);

/// LISTEN の張り直しに失敗したときの待ちの上限。
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// 待機中の consumer を起こす部品。[`Waker::polling`] は LISTEN を持たず、従来どおり眠るだけ。
#[derive(Clone)]
pub struct Waker {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    pools: Vec<PgPool>,
    /// キューごとの起床通知（初めて待たれたときに作る）。
    queues: Mutex<HashMap<String, Arc<Notify>>>,
    /// LISTEN できているプールの数。`pools.len()` と等しいときだけ長く眠る。
    listening: AtomicUsize,
    idle_max: Duration,
}

impl Waker {
    /// LISTEN を持たない（呼び出し側の `poll` で眠るだけ）。テスト・単体起動用。
    pub fn polling() -> Self {
        Waker { inner: None }
    }

    /// 各プールで LISTEN を張る（バックグラウンドのタスク。tokio のランタイム上で呼ぶこと）。
    pub fn listen(pools: Vec<PgPool>) -> Self {
        Self::listen_with_idle_max(pools, DEFAULT_IDLE_MAX)
    }

    /// [`Waker::listen`] の眠る上限を指定する版（テスト用）。
    pub fn listen_with_idle_max(pools: Vec<PgPool>, idle_max: Duration) -> Self {
        let inner = Arc::new(Inner {
            pools,
            queues: Mutex::new(HashMap::new()),
            listening: AtomicUsize::new(0),
            idle_max,
        });
        for pool in &inner.pools {
            tokio::spawn(listen_loop(pool.clone(), Arc::downgrade(&inner)));
        }
        Waker { inner: Some(inner) }
    }

    /// 全プールで LISTEN できているか（運用・テスト用）。
    pub fn is_listening(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| i.healthy())
    }

    /// キュー `queue` が空だったときに眠る。通知・次のジョブの可視時刻・上限のいずれかで返る。
    ///
    /// `poll` は LISTEN が使えないときの眠る時間（従来のポーリング間隔）。
    pub async fn wait(&self, queue: &str, poll: Duration) {
        let cap = self.inner.as_ref().map_or(poll, |i| i.idle_max);
        self.wait_at_most(queue, poll, cap).await;
    }

    /// [`Waker::wait`] に眠る上限 `cap` を付けた版。ジョブの消費と別の周期処理（スイープ等）を
    /// 同じループで回すワーカーが、次の周期処理の時刻までしか眠らないために使う。
    pub async fn wait_at_most(&self, queue: &str, poll: Duration, cap: Duration) {
        let inner = self.inner.clone();
        self.wait_until_due(queue, poll, cap, || async move {
            match inner {
                Some(i) => i.next_due(queue).await,
                None => Ok(None),
            }
        })
        .await;
    }

    /// jobq 以外の取り出し（workflow の step など）を同じ仕組みで待つ版。
    ///
    /// `queue` は通知のペイロード（投入側が `pg_notify('jobq', queue)` を送ること。トリガでもよい）。
    /// `next_due` は「次に取り出せるようになるまでの時間（無ければ `None`）」を引く問い合わせで、
    /// 通知の登録の**後**に呼ぶ（引いた後・眠る前の投入を取りこぼさない）。LISTEN が使えないときは
    /// 呼ばずに `poll` だけ眠る。
    pub async fn wait_until_due<F, Fut>(
        &self,
        queue: &str,
        poll: Duration,
        cap: Duration,
        next_due: F,
    ) where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Option<Duration>, sqlx::Error>>,
    {
        let Some(inner) = self.inner.as_ref().filter(|i| i.healthy()) else {
            tokio::time::sleep(poll.min(cap)).await;
            return;
        };
        let notify = inner.notify_for(queue);
        let notified = notify.notified();
        tokio::pin!(notified);
        // 可視時刻を引く**前に**登録する: 引いた後・眠る前に積まれたジョブの通知を取りこぼさない。
        notified.as_mut().enable();
        let sleep_for = match next_due().await {
            Ok(Some(due)) => due.max(DUE_FLOOR).min(cap),
            Ok(None) => cap,
            Err(e) => {
                tracing::warn!(error = %e, queue, "jobq: 次のジョブの可視時刻を引けません");
                poll.min(cap)
            }
        };
        tokio::select! {
            () = &mut notified => {}
            () = tokio::time::sleep(sleep_for) => {}
        }
    }
}

impl Inner {
    fn healthy(&self) -> bool {
        !self.pools.is_empty() && self.listening.load(Ordering::Acquire) == self.pools.len()
    }

    fn notify_for(&self, queue: &str) -> Arc<Notify> {
        let mut queues = self
            .queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(queues.entry(queue.to_string()).or_default())
    }

    fn wake(&self, queue: &str) {
        let queues = self
            .queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(n) = queues.get(queue) {
            n.notify_waiters();
        }
    }

    fn wake_all(&self) {
        let queues = self
            .queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for n in queues.values() {
            n.notify_waiters();
        }
    }

    /// キューで次に見えるようになるジョブまでの時間（全プールの最小）。ジョブが無ければ `None`。
    ///
    /// 眠る直前（＝取り出しが空だった直後）にだけ引く。残っている行は処理中・バックオフ中のものだけ
    /// なので、走査は小さい（`job_queue_fair_idx` のキュー接頭辞で辿る。可視時刻の索引は claim の
    /// 更新を非 HOT にするので足さない・migrations/0079）。
    async fn next_due(&self, queue: &str) -> Result<Option<Duration>, sqlx::Error> {
        let mut best: Option<f64> = None;
        for pool in &self.pools {
            let secs: Option<f64> = sqlx::query_scalar(
                // tenant-sql: allow(キュー全体の次の可視時刻（待機時間の算出）・行は返さない)
                "select extract(epoch from (min(visible_at) - now()))::float8 \
                 from job_queue where queue = $1",
            )
            .bind(queue)
            .fetch_one(pool)
            .await?;
            if let Some(s) = secs {
                best = Some(best.map_or(s, |b| b.min(s)));
            }
        }
        Ok(best.map(|s| Duration::from_secs_f64(s.max(0.0))))
    }
}

/// 1 プールの LISTEN を保ち、通知を待っている consumer へ配る。`Waker` が全て落ちたら終わる。
async fn listen_loop(pool: PgPool, inner: Weak<Inner>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if inner.strong_count() == 0 {
            return;
        }
        let mut listener = match connect(&pool).await {
            Ok(l) => l,
            Err(sqlx::Error::PoolClosed) => return,
            Err(e) => {
                tracing::warn!(error = %e, "jobq: LISTEN を張れません（ポーリングで待機）");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX);
                continue;
            }
        };
        backoff = Duration::from_secs(1);
        let Some(i) = inner.upgrade() else { return };
        i.listening.fetch_add(1, Ordering::AcqRel);
        // 張る前（切れていた間）の投入は通知が届かないので、待っている consumer を一度起こす。
        i.wake_all();
        drop(i);
        let closed = loop {
            match listener.try_recv().await {
                Ok(Some(n)) => {
                    let Some(i) = inner.upgrade() else { return };
                    i.wake(n.payload());
                }
                // 切れて張り直した（eager_reconnect）。切れていた間の通知を補う。
                Ok(None) => {
                    let Some(i) = inner.upgrade() else { return };
                    tracing::info!("jobq: LISTEN 接続を張り直しました");
                    i.wake_all();
                }
                Err(e) => break e,
            }
        };
        if let Some(i) = inner.upgrade() {
            i.listening.fetch_sub(1, Ordering::AcqRel);
            i.wake_all(); // 長く眠っている consumer をポーリングの待機へ戻す
        }
        if matches!(closed, sqlx::Error::PoolClosed) {
            return;
        }
        tracing::warn!(error = %closed, "jobq: LISTEN 接続が切れました（張り直します）");
        tokio::time::sleep(backoff).await;
    }
}

async fn connect(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(CHANNEL).await?;
    Ok(listener)
}
