//! outbox の claim（台帳 consumer の 1 バッチ）を走査の下限つきで行う（2 回目の検証 2-D-P3-2）。
//!
//! 一括取込で 10 万件のイベントが溜まると、`claim_undelivered` の anti-join は配送済みの先頭から読み直し
//! 1 回 1 秒を超えていた（配信の要約が 600 行/秒・取込の遅れ）。直近に取った最大の id を下限にして
//! テールから読み、定期的な全走査で取りこぼしを拾う（[`storage::event::ClaimHints`]）。

use sqlx::PgConnection;

use crate::DataError;

/// `consumer` の未配送のイベントを 1 バッチ取る（`pool_id` は id の空間を分けるキー）。
pub(crate) async fn claim_batch(
    tx: &mut PgConnection,
    hints: &storage::event::ClaimHints,
    pool_id: &str,
    consumer: &str,
    limit: i64,
) -> Result<Vec<storage::OutboxEvent>, DataError> {
    let key = format!("{pool_id}\0{consumer}");
    let after = hints.lower_bound(&key);
    let events = storage::event::claim_undelivered_after(&mut *tx, consumer, after, limit)
        .await
        .map_err(|e| DataError::Internal(format!("outbox: {e}")))?;
    hints.observe(&key, &events, limit);
    Ok(events)
}
