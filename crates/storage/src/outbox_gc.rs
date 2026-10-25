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
//! - 削除対象は「登録済み全台帳コンシューマへ配送済み・購読ストリームの保持期間外」の行だけ。
//!   `processed_at` で ack するコンシューマ（RAG の relay・`outbox_ack_consumer`）が登録されている配備では
//!   `processed_at` も待つ。**未配送・未 ack の行は時間経過でも消さない**（遅い/停止中のコンシューマが
//!   イベントを失わない）。
//! - 配送を待つべきコンシューマ集合は `outbox_consumer` から読む（[`gc_delivered_registered`]）。
//!   呼び出し側に渡させないのは安全性の要請（フラグ off のレプリカが他コンシューマ宛の未配送
//!   イベントを消す事故を防ぐ・`event::registered_consumers` の doc 参照）。
//! - `FOR UPDATE SKIP LOCKED` で刻むので**全レプリカで同時に走っても安全**。リーダー選出は不要。
//! - GC が進めないときは滞留を warn で出し、**先頭の行が消せない実際の理由**（未配送のコンシューマ・
//!   `processed_at` の ack 待ち・保持期間内）を示す。今回のバグは「静かに溜まる」形だったので隠れ、
//!   2 回目は「lag が大きいコンシューマが原因」と全コンシューマ lag 0 のまま誤った原因を出していた
//!   （3 回目の検証 3-D-1）。推測ではなく判定と同じ条件で原因を引く。
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
/// 上限に達したらその周期は打ち切り、[`CATCH_UP_INTERVAL`] だけ休んで続ける（一括取込の後の
/// 数十万行を 1 分に 2 万行ずつではなく、刻みながら続けて消す）。
const MAX_BATCHES_PER_CYCLE: u32 = 20;

/// 周期の上限まで消した（まだ消せる行が残っている）ときの次の周期までの間隔。
const CATCH_UP_INTERVAL: Duration = Duration::from_secs(1);

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
        // 起動直後は動かない。`wire_storage`（ここ）は `spawn_workflow_runtime` / gateway 配線より
        // **先**に走るので、コンシューマの `register_consumer`（初回 fast-forward）はこの後に来る。
        // GC と fast-forward は advisory lock で排他済みだが、そもそも startup で競らせない方が
        // 素直で、配線順が壊れても壊れない（GC は遅れて困る処理ではない）。
        tokio::time::sleep(GC_INTERVAL).await;
        let mut backoff = MIN_BACKOFF;
        loop {
            match gc_cycle(&pool).await {
                Ok(cycle) => {
                    backoff = MIN_BACKOFF;
                    if cycle.deleted > 0 {
                        tracing::info!(
                            deleted = cycle.deleted,
                            "outbox の配送済みイベントを GC しました"
                        );
                    }
                    if cycle.truncated {
                        // まだ消せる行が残っている（追いつき中）。原因の表示はせず続ける。
                        tokio::time::sleep(CATCH_UP_INTERVAL).await;
                        continue;
                    }
                    // 消せる行が尽きた。滞留していれば、先頭の行が消せない理由を出す。
                    report_backlog(&pool).await;
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

/// 1 周期の GC の結果。
struct GcCycle {
    deleted: u64,
    /// 周期の上限まで消して打ち切った（まだ消せる行が残っている）。
    truncated: bool,
}

/// 1 周期分の GC。バッチが埋まらなくなる（＝消せる行が尽きた）か上限バッチ数まで刻む。
///
/// バッチごとに**明示トランザクション**を張る。`gc_delivered` が `register_consumer` との相互排除に
/// 使う advisory lock は **txn 単位**であり、`acquire()` の autocommit では文の終わりで解放されて
/// 排他にならない（＝相手を FK 違反で落とす）。1 バッチ = 1 txn なので長時間トランザクションにも
/// ならない。
async fn gc_cycle(pool: &sqlx::PgPool) -> Result<GcCycle, crate::error::StorageError> {
    let mut total = 0u64;
    for _ in 0..MAX_BATCHES_PER_CYCLE {
        let mut tx = pool.begin().await?;
        let deleted = gc_delivered_registered(&mut tx, DEFAULT_GC_BATCH).await?;
        tx.commit().await?;
        total = total.saturating_add(deleted);
        // バッチが埋まらなかった＝これ以上消せる行は無い。
        if deleted < u64::try_from(DEFAULT_GC_BATCH).unwrap_or(u64::MAX) {
            return Ok(GcCycle {
                deleted: total,
                truncated: false,
            });
        }
    }
    Ok(GcCycle {
        deleted: total,
        truncated: true,
    })
}

/// 滞留状況を観測してログに出す（消せる行が尽きた周期のみ）。
///
/// 行数が閾値未満なら「消すものが無い」だけなので黙る。閾値超過なら**先頭の行が消せない理由**
/// （[`crate::event::HeadBlocker`]）を出す。GC は id 順に消すので、それがそのまま GC が進めない原因。
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
    let Some(cause) = backlog_cause(&backlog) else {
        // 先頭の行は消せる（行数の概算が古いだけ・または並行する claim がロック中だった）。
        tracing::debug!(
            estimated_rows = backlog.estimated_rows,
            "outbox の行数の概算は閾値超過だが、先頭の行は GC できる"
        );
        return;
    };
    let lags = backlog
        .consumers
        .iter()
        .map(|c| format!("{}={}", c.consumer, c.lag))
        .collect::<Vec<_>>()
        .join(" ");
    tracing::warn!(
        estimated_rows = backlog.estimated_rows,
        latest_event_id = backlog.latest_event_id,
        head_event_id = backlog.head.as_ref().map_or(0, |h| h.event_id),
        max_lag = backlog.max_lag(),
        consumer_lags = %lags,
        "outbox が滞留しています（GC が進めていない）。原因: {cause}"
    );
}

/// 先頭の行が消せない理由の文言（消せるなら `None`）。
fn backlog_cause(backlog: &crate::event::OutboxBacklog) -> Option<String> {
    let head = backlog.head.as_ref()?;
    if head.is_collectable() {
        return None;
    }
    let mut causes = Vec::new();
    if !head.undelivered.is_empty() {
        causes.push(format!(
            "未配送のコンシューマがあります（{}）。停止・遅延していないか確認し、恒久廃止したコンシューマなら \
             event::unregister_consumer で登録を外してください",
            head.undelivered.join(", ")
        ));
    }
    if !head.awaiting_ack.is_empty() {
        causes.push(format!(
            "processed_at の ack を待っています（{}）。RAG の relay が動いているか確認し、RAG を恒久的に畳んだ \
             配備なら event::unregister_ack_consumer で登録を外してください",
            head.awaiting_ack.join(", ")
        ));
    }
    if let Some(until) = head.retained_until {
        causes.push(format!(
            "購読ストリームの保持期間内です（{} まで・期間を過ぎれば GC されます）",
            until.to_rfc3339()
        ));
    }
    Some(causes.join(" / "))
}

#[cfg(test)]
mod tests {
    use super::backlog_cause;
    use crate::event::{HeadBlocker, OutboxBacklog};

    fn backlog(head: Option<HeadBlocker>) -> OutboxBacklog {
        OutboxBacklog {
            estimated_rows: 100_000,
            latest_event_id: 100,
            consumers: Vec::new(),
            head,
        }
    }

    fn head() -> HeadBlocker {
        HeadBlocker {
            event_id: 1,
            created_at: chrono::Utc::now(),
            undelivered: Vec::new(),
            awaiting_ack: Vec::new(),
            retained_until: None,
        }
    }

    #[test]
    fn cause_names_the_actual_blocker() {
        assert_eq!(backlog_cause(&backlog(None)), None, "空の outbox");
        assert_eq!(backlog_cause(&backlog(Some(head()))), None, "消せる");
        let ack = HeadBlocker {
            awaiting_ack: vec!["rag".into()],
            ..head()
        };
        let msg = backlog_cause(&backlog(Some(ack))).unwrap();
        assert!(msg.contains("processed_at") && msg.contains("rag"), "{msg}");
        assert!(!msg.contains("未配送"), "{msg}");
        let lag = HeadBlocker {
            undelivered: vec!["workflow".into()],
            ..head()
        };
        let msg = backlog_cause(&backlog(Some(lag))).unwrap();
        assert!(
            msg.contains("workflow") && !msg.contains("processed_at"),
            "{msg}"
        );
        let kept = HeadBlocker {
            retained_until: Some(chrono::Utc::now()),
            ..head()
        };
        let msg = backlog_cause(&backlog(Some(kept))).unwrap();
        assert!(msg.contains("保持期間"), "{msg}");
    }
}
