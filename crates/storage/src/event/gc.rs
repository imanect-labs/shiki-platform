//! outbox の GC と滞留観測（#413）。
//!
//! `storage_event_outbox` は書込チョークポイントの真下にある最ホットな表で、**放置すると
//! 永久に成長する**。そして [`claim_undelivered`] の anti-join は「配送済みの全行を走査してから
//! 未配送のテールに到達する」ので、行数がそのまま claim のコストになる（実測: 1 万行 3.2ms →
//! 100 万行 524ms）。**GC はこの表の性能前提**であり、あると嬉しい掃除ではない。
//!
//! バックグラウンド実行は [`crate::outbox_gc`] が担う。
//!
//! [`claim_undelivered`]: super::claim_undelivered

use sqlx::PgConnection;

use super::registered_consumers;
use crate::error::StorageError;

/// 1 回の GC で削除する最大行数（既定）。大きな `DELETE` を 1 文で撃たず刻む。
pub const DEFAULT_GC_BATCH: i64 = 1_000;

/// 全コンシューマへ配送済み **かつ** RAG（`processed_at`）ack 済みの outbox 行を最大 `batch` 件 GC する
/// （配送台帳は `ON DELETE CASCADE` で同時に消える）。
///
/// **未 ack の行は決して削除しない**（retention による time-based バイパスは持たない・遅い/停止中の
/// コンシューマがイベントを失わない）。`ledger_consumers` は「配送を待つべきコンシューマ集合」。
/// 空配列なら `processed_at` のみで判定する（台帳コンシューマが 1 つも登録されていない配備）。
///
/// 削除対象は `FOR UPDATE SKIP LOCKED` で確保するので、**全レプリカで同時に呼んでも安全**
/// （互いをブロックせず、同じ行を二重に掴まない）。返り値は削除件数。
///
/// ⚠️ `SKIP LOCKED` の帰結として、**消費者が claim 中（`FOR UPDATE` 保持中）の行はその回スキップ
/// される**。GC は「消せるものを取りこぼさない」より「hot path をブロックしない」を優先する
/// （取り逃した行は次周期で消える）。したがって単発呼び出しの戻り値が 0 でも「消すものが無い」とは
/// 断定できない。滞留判定には [`outbox_backlog`] を使うこと。
///
/// 本番からは [`gc_delivered_registered`] を使うこと（コンシューマ集合を呼び出し側に渡させない）。
/// この関数が明示集合を取るのはテストと管理コマンドのため。
pub async fn gc_delivered(
    conn: &mut PgConnection,
    ledger_consumers: &[&str],
    batch: i64,
) -> Result<u64, StorageError> {
    let consumers: Vec<String> = ledger_consumers.iter().map(|s| (*s).to_string()).collect();
    let expected = i64::try_from(consumers.len())
        .map_err(|_| StorageError::Invalid("consumer 数が多すぎます".into()))?;
    let deleted = sqlx::query(
        "DELETE FROM storage_event_outbox \
         WHERE id IN ( \
             SELECT o.id FROM storage_event_outbox o \
             WHERE o.processed_at IS NOT NULL \
               AND ( \
                   SELECT count(*) FROM outbox_delivery d \
                   WHERE d.event_id = o.id AND d.consumer = ANY($1) \
               ) >= $2 \
             ORDER BY o.id \
             LIMIT $3 \
             FOR UPDATE SKIP LOCKED \
         )",
    )
    .bind(&consumers)
    .bind(expected)
    .bind(batch.max(1))
    .execute(conn)
    .await?;
    Ok(deleted.rows_affected())
}

/// **本番の GC 入口**: 配送を待つべきコンシューマ集合を `outbox_consumer` から読んで GC する。
///
/// コンシューマ集合を呼び出し側に渡させないのは安全性の要請（[`registered_consumers`] の
/// doc コメント参照。フラグ off のレプリカが他コンシューマ宛の未配送イベントを消す事故を防ぐ）。
pub async fn gc_delivered_registered(
    conn: &mut PgConnection,
    batch: i64,
) -> Result<u64, StorageError> {
    let consumers = registered_consumers(&mut *conn).await?;
    let refs: Vec<&str> = consumers.iter().map(String::as_str).collect();
    gc_delivered(conn, &refs, batch).await
}

/// 台帳コンシューマ 1 つの配送進捗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerLag {
    pub consumer: String,
    /// このコンシューマが配送済みの最大 event id（未配送しかなければ 0）。
    pub delivered_upto: i64,
    /// `latest_event_id - delivered_upto`。0 なら追いついている。
    pub lag: i64,
}

/// outbox の滞留状況（GC が進めない原因の切り分け用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxBacklog {
    /// `pg_class.reltuples` による**概算**行数（`count(*)` の全表走査を避けるため）。
    /// autovacuum/ANALYZE のタイミングで更新されるので厳密値ではない。傾向を見るためのもの。
    pub estimated_rows: i64,
    /// 現在の最大 event id。
    pub latest_event_id: i64,
    /// 登録コンシューマごとの遅れ。
    pub consumers: Vec<ConsumerLag>,
}

impl OutboxBacklog {
    /// 最も遅れているコンシューマの lag（登録が無ければ 0）。
    pub fn max_lag(&self) -> i64 {
        self.consumers.iter().map(|c| c.lag).max().unwrap_or(0)
    }
}

/// outbox の滞留状況を O(log n) で取る（定期ログ/メトリクス用）。
///
/// 「未配送の正確な件数」は全表走査になるので取らない。代わりに **id の進捗差（lag）** を見る。
/// `max(event_id)` は `outbox_delivery` の索引後方スキャン、`max(id)` は主キーの index-only scan、
/// 行数は `reltuples` 概算なので、100 万行でも合計 1ms 未満で済む
/// （素直に `LEFT JOIN ... GROUP BY` で書くと `outbox_delivery` の全走査になり 586ms かかる）。
pub async fn outbox_backlog(conn: &mut PgConnection) -> Result<OutboxBacklog, StorageError> {
    let latest_event_id: i64 =
        sqlx::query_scalar("SELECT coalesce(max(id), 0) FROM storage_event_outbox")
            .fetch_one(&mut *conn)
            .await?;
    let estimated_rows: i64 = sqlx::query_scalar(
        "SELECT greatest(coalesce(reltuples, 0), 0)::bigint FROM pg_class \
         WHERE relname = 'storage_event_outbox'",
    )
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(0);
    // 相関サブクエリで per-consumer の max を索引から引く（結合にすると全走査になる）。
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT c.name, \
                coalesce((SELECT max(d.event_id) FROM outbox_delivery d \
                          WHERE d.consumer = c.name), 0) \
         FROM outbox_consumer c ORDER BY c.name",
    )
    .fetch_all(conn)
    .await?;
    let consumers = rows
        .into_iter()
        .map(|(consumer, delivered_upto)| ConsumerLag {
            consumer,
            delivered_upto,
            lag: (latest_event_id - delivered_upto).max(0),
        })
        .collect();
    Ok(OutboxBacklog {
        estimated_rows,
        latest_event_id,
        consumers,
    })
}
