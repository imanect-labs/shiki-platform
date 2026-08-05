//! outbox GC のバックグラウンドタスク（#413）。
//!
//! `storage_event_outbox` は書込チョークポイントの真下にある最ホットな表で、**GC が回っていないと
//! 永久に成長する**。そして [`claim_undelivered`] の anti-join は「配送済みの全行を走査してから
//! 未配送のテールに到達する」ため、行数がそのまま claim のコストになる
//! （実測: 1 万行 3.2ms → 10 万行 34ms → 100 万行 524ms・temp 溢れ）。
//!
//! さらに workflow のリーダー tick は relay とタイマー起床/キャンセル回収を**直列**に回すので、
//! outbox の走査長が伸びると `docs/workflow/engine.md` が約束する起床遅延の上限まで崩れる。
//! つまりこの GC は「あると嬉しい掃除」ではなく**イベント経路の性能前提**である。
//!
//! 設計:
//! - 削除対象は「`processed_at` ack 済み **かつ** 登録済み全台帳コンシューマへ配送済み」の行だけ。
//!   **未 ack の行は時間経過でも消さない**（遅い/停止中のコンシューマがイベントを失わない）。
//! - 配送を待つべきコンシューマ集合は `outbox_consumer` から読む（[`gc_delivered_registered`]）。
//!   呼び出し側に渡させないのは安全性の要請（フラグ off のレプリカが他コンシューマ宛の未配送
//!   イベントを消す事故を防ぐ・`event::registered_consumers` の doc 参照）。
//! - `FOR UPDATE SKIP LOCKED` で刻むので**全レプリカで同時に走っても安全**。リーダー選出は不要。
//! - GC が進めないとき（＝どれかのコンシューマが遅延/停止）は滞留を warn で出す。今回のバグは
//!   「静かに溜まる」形だったので隠れた。同じ隠れ方を二度させない。
//!
//! [`claim_undelivered`]: crate::event::claim_undelivered
//! [`gc_delivered_registered`]: crate::event::gc_delivered_registered

use std::time::Duration;

use crate::event::{gc_delivered_registered, outbox_backlog, DEFAULT_GC_BATCH};

/// GC 周期。イベント発行レートに対して十分速く、アイドル時の空クエリが無視できる間隔。
const GC_INTERVAL: Duration = Duration::from_mins(1);

/// 1 周期で削除する最大バッチ数（`DEFAULT_GC_BATCH` × これが 1 周期の上限行数）。
///
/// 溜まった分を一気に消して長時間トランザクション/WAL バーストを作らないための刻み。
/// 上限に達したらその周期は打ち切り、次周期で続ける（バックログは徐々に減る）。
const MAX_BATCHES_PER_CYCLE: u32 = 20;

/// この行数を超えていたら滞留として warn を出す。
///
/// 実測で劣化が体感され始める 10 万行より十分手前に置く（3.2ms → 34ms の間）。
const BACKLOG_WARN_ROWS: i64 = 50_000;

/// 連続失敗時の初期バックオフ。
const MIN_BACKOFF: Duration = Duration::from_secs(5);

/// 連続失敗時のバックオフ上限。
const MAX_BACKOFF: Duration = Duration::from_mins(10);

/// outbox GC をバックグラウンド起動する（shiki-server の起動フローから呼ぶ）。
///
/// 返した `JoinHandle` は保持不要（プロセス生存中は動き続ける）。
pub fn spawn_outbox_gc(pool: sqlx::PgPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = MIN_BACKOFF;
        loop {
            match gc_cycle(&pool).await {
                Ok(deleted) => {
                    backoff = MIN_BACKOFF;
                    if deleted > 0 {
                        tracing::info!(deleted, "outbox の配送済みイベントを GC しました");
                    }
                    // 削除が無い＝進めていない可能性がある。滞留していれば原因を出す。
                    if deleted == 0 {
                        report_backlog(&pool).await;
                    }
                    tokio::time::sleep(GC_INTERVAL).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "outbox GC に失敗しました（次周期で再試行）");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    })
}

/// 1 周期分の GC。バッチが埋まらなくなる（＝消せる行が尽きた）か上限バッチ数まで刻む。
async fn gc_cycle(pool: &sqlx::PgPool) -> Result<u64, crate::error::StorageError> {
    let mut total = 0u64;
    for _ in 0..MAX_BATCHES_PER_CYCLE {
        let mut conn = pool.acquire().await?;
        let deleted = gc_delivered_registered(&mut conn, DEFAULT_GC_BATCH).await?;
        total = total.saturating_add(deleted);
        // バッチが埋まらなかった＝これ以上消せる行は無い。
        if deleted < u64::try_from(DEFAULT_GC_BATCH).unwrap_or(u64::MAX) {
            break;
        }
    }
    Ok(total)
}

/// 滞留状況を観測してログに出す（GC が進めなかった周期のみ）。
///
/// 行数が閾値未満なら「消すものが無い」だけなので黙る。閾値超過なら**どのコンシューマが
/// 遅れているか**を出す（それが GC を止めている原因）。
async fn report_backlog(pool: &sqlx::PgPool) {
    let mut conn = match pool.acquire().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "outbox 滞留の観測用接続に失敗しました");
            return;
        }
    };
    let backlog = match outbox_backlog(&mut conn).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "outbox 滞留の観測に失敗しました");
            return;
        }
    };
    if backlog.estimated_rows < BACKLOG_WARN_ROWS {
        return; // 消すものが無いだけ（正常）。
    }
    let lags = backlog
        .consumers
        .iter()
        .map(|c| format!("{}={}", c.consumer, c.lag))
        .collect::<Vec<_>>()
        .join(" ");
    tracing::warn!(
        estimated_rows = backlog.estimated_rows,
        latest_event_id = backlog.latest_event_id,
        max_lag = backlog.max_lag(),
        consumer_lags = %lags,
        "outbox が滞留しています（GC が進めていない）。lag が大きいコンシューマが原因です。\
         恒久廃止したコンシューマが残っている場合は event::unregister_consumer で登録を外してください"
    );
}
