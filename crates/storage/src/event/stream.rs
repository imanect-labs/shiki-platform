//! 購読ストリームの配信位置（`delivery_seq`・Phase 15 Task 15.6・data-platform §9.2）。
//!
//! [`super::claim_undelivered`] は存在性ベースの anti-join で、**単調な配信位置を持たない**（id 順・
//! コミット順に依存しないことが目的）。リアルタイム配信の再開（`Last-Event-ID`）にはストリーム内で
//! 単調で穴の無い位置が要るので、配送台帳に `delivery_seq` を持たせる。
//!
//! # 採番の規則（PIT-70）
//!
//! - **consumer（ストリーム）ごとの連番**を `outbox_delivery_seq` の行から払い出す。グローバルな
//!   `bigserial` にしない（他ストリームのイベントで穴が空き、連続確認済みの位置が定義できなくなる）。
//! - 採番行を `UPDATE … RETURNING` で取るので、**同じストリームへの採番は直列化され、コミット順が
//!   番号順に一致する**（後から番号を取った Tx は前の Tx のコミットを待つ）。読み手は番号 N を見たら
//!   N 未満は全てコミット済みと判断できる。採番と台帳行の挿入は同じ Tx なので、払い出して失敗した穴は
//!   残らない（ロールバックで採番も戻る）。
//! - [`mark_delivered_seq`] は**ストリームへの永続化**であって、ネットワーク送信の成功ではない。
//!   送信は永続化の後に行い、到達は読み手の `Last-Event-ID`（low-water mark）で確かめる（at-least-once）。
//!
//! # 保持期間
//!
//! 採番した台帳行は `retain_until` まで GC しない（[`super::gc_delivered`]）。その間はリプレイできる。
//! 保持期間を過ぎた位置からの再開は [`StreamBounds::resumable`] が偽になる（呼び出し側が
//! `stream.reset` / 410 を返す）。

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;

use crate::error::StorageError;

/// 採番行を確保して返す（無ければ作る）。**呼び出し側の Tx 上で**行ロックを保持する。
///
/// 同じ consumer の採番・排他を必要とする処理（リレーの 1 バッチ）の先頭で呼ぶと、並行リレーは
/// コミットまで待つ（同じ consumer への配信の直列化）。別 consumer とは競合しない。
pub async fn lock_consumer_seq(
    conn: &mut PgConnection,
    consumer: &str,
    tenant_id: Option<&str>,
) -> Result<i64, StorageError> {
    sqlx::query(
        "INSERT INTO outbox_delivery_seq (consumer, tenant_id) VALUES ($1, $2) \
         ON CONFLICT (consumer) DO NOTHING",
    )
    .bind(consumer)
    .bind(tenant_id)
    .execute(&mut *conn)
    .await?;
    let next: i64 = sqlx::query_scalar(
        "SELECT next_seq FROM outbox_delivery_seq WHERE consumer = $1 FOR UPDATE",
    )
    .bind(consumer)
    .fetch_one(&mut *conn)
    .await?;
    Ok(next)
}

/// `event_ids`（この順で）を `consumer` のストリームへ永続化し、連番を払い出す。
///
/// 返り値は `(event_id, delivery_seq)`（入力順・番号は連続）。既に同じ consumer の台帳にある
/// event はスキップする（冪等・再配信で二重採番しない）。`retain` は台帳行を GC しない期間。
///
/// 採番行の `UPDATE … RETURNING` が行ロックを取り、Tx のコミットまで保持する（並行リレーの排他）。
pub async fn mark_delivered_seq(
    conn: &mut PgConnection,
    consumer: &str,
    tenant_id: &str,
    event_ids: &[i64],
    retain: Duration,
) -> Result<Vec<(i64, i64)>, StorageError> {
    if event_ids.is_empty() {
        return Ok(Vec::new());
    }
    lock_consumer_seq(conn, consumer, Some(tenant_id)).await?;
    // 既に採番済みの event を除く（入力順を保つ）。
    let done: Vec<i64> = sqlx::query_scalar(
        "SELECT event_id FROM outbox_delivery WHERE consumer = $1 AND event_id = ANY($2)",
    )
    .bind(consumer)
    .bind(event_ids)
    .fetch_all(&mut *conn)
    .await?;
    let todo: Vec<i64> = event_ids
        .iter()
        .copied()
        .filter(|id| !done.contains(id))
        .collect();
    if todo.is_empty() {
        return Ok(Vec::new());
    }
    let n = i64::try_from(todo.len())
        .map_err(|_| StorageError::Invalid("採番する件数が多すぎます".into()))?;
    let first: i64 = sqlx::query_scalar(
        "UPDATE outbox_delivery_seq SET next_seq = next_seq + $2, updated_at = now() \
         WHERE consumer = $1 RETURNING next_seq - $2",
    )
    .bind(consumer)
    .bind(n)
    .fetch_one(&mut *conn)
    .await?;
    let seqs: Vec<i64> = (0..n).map(|i| first + i).collect();
    sqlx::query(
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id, delivery_seq, retain_until) \
         SELECT $1, e.id, $3, e.seq, now() + make_interval(secs => $4::double precision) \
         FROM unnest($2::int8[], $5::int8[]) AS e(id, seq)",
    )
    .bind(consumer)
    .bind(&todo)
    .bind(tenant_id)
    .bind(retain.as_secs_f64())
    .bind(&seqs)
    .execute(&mut *conn)
    .await?;
    Ok(todo.into_iter().zip(seqs).collect())
}

/// claim したイベントの consumer 非公開の材料（`private_payload`）を id で引く（無い行は含めない）。
///
/// 汎用の DTO（[`super::OutboxEvent`]）は非公開部を持たない（汎用の consumer へ渡さない）ので、
/// 必要な consumer だけがこれで引く。
pub async fn private_payloads(
    conn: &mut PgConnection,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, Value>, StorageError> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows: Vec<(i64, Value)> = sqlx::query_as(
        "SELECT id, private_payload FROM storage_event_outbox \
         WHERE id = ANY($1) AND private_payload IS NOT NULL",
    )
    .bind(ids)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// ストリームの 1 件（配信位置・本文の参照）。
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEntry {
    pub seq: i64,
    pub event_id: i64,
    pub tenant_id: String,
    pub payload: Value,
    /// consumer 非公開の材料（before 判定の旧値・内部キー）。**外へ出さない**。
    pub private_payload: Option<Value>,
    pub created_at: DateTime<Utc>,
}

/// `after` より後の配信位置を昇順に最大 `limit` 件読む（読み取り専用・台帳は変更しない）。
pub async fn stream_entries_after(
    conn: &mut PgConnection,
    consumer: &str,
    after: i64,
    limit: i64,
) -> Result<Vec<StreamEntry>, StorageError> {
    let rows: Vec<EntryRow> = sqlx::query_as(
        "SELECT d.delivery_seq AS seq, o.id AS event_id, o.tenant_id, o.payload, \
                o.private_payload, o.created_at \
         FROM outbox_delivery d JOIN storage_event_outbox o ON o.id = d.event_id \
         WHERE d.consumer = $1 AND d.delivery_seq > $2 \
         ORDER BY d.delivery_seq LIMIT $3",
    )
    .bind(consumer)
    .bind(after)
    .bind(limit.max(1))
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| StreamEntry {
            seq: r.seq,
            event_id: r.event_id,
            tenant_id: r.tenant_id,
            payload: r.payload,
            private_payload: r.private_payload,
            created_at: r.created_at,
        })
        .collect())
}

#[derive(sqlx::FromRow)]
struct EntryRow {
    seq: i64,
    event_id: i64,
    tenant_id: String,
    payload: Value,
    private_payload: Option<Value>,
    created_at: DateTime<Utc>,
}

/// ストリームの現在の範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamBounds {
    /// 払い出し済みの最大位置（まだ無ければ 0）。
    pub head: i64,
    /// 保持している最小位置（保持が無ければ `None`）。
    pub floor: Option<i64>,
}

impl StreamBounds {
    /// `since`（受信済みの low-water mark）から欠落なく再開できるか。
    ///
    /// `since + 1` 以降が全て保持されていること（`head <= since` なら再開位置が最新）。
    /// 未来の位置（`since > head`）は不正として偽（`stream.reset` で取り直させる）。
    pub fn resumable(&self, since: i64) -> bool {
        if since < 0 || since > self.head {
            return false;
        }
        if since == self.head {
            return true;
        }
        self.floor.is_some_and(|f| f <= since + 1)
    }
}

/// 台帳 consumer がまだ配送（採番）していない、テナント・テーブルのドメインイベントの本文
/// （`payload`・`private_payload`）。スナップショット読取が「基準位置より後の変更」を引くのに使う。
///
/// `payload ->> 'table_id'` は索引（`storage_event_outbox_data_change_idx`）の式と同じ形で書く。
pub async fn undelivered_table_events(
    conn: &mut PgConnection,
    consumer: &str,
    tenant_id: &str,
    event_type: &str,
    table_id: &str,
) -> Result<Vec<(Value, Option<Value>)>, StorageError> {
    let rows: Vec<(Value, Option<Value>)> = sqlx::query_as(
        "SELECT o.payload, o.private_payload FROM storage_event_outbox o \
         WHERE o.tenant_id = $1 AND o.payload ->> 'event_type' = $2 \
           AND o.payload ->> 'table_id' = $3 \
           AND NOT EXISTS ( \
               SELECT 1 FROM outbox_delivery d WHERE d.event_id = o.id AND d.consumer = $4 \
           ) \
         ORDER BY o.id",
    )
    .bind(tenant_id)
    .bind(event_type)
    .bind(table_id)
    .bind(consumer)
    .fetch_all(conn)
    .await?;
    Ok(rows)
}

/// ストリームの範囲（head は採番行・floor は台帳の索引の先頭）。
pub async fn stream_bounds(
    conn: &mut PgConnection,
    consumer: &str,
) -> Result<StreamBounds, StorageError> {
    let next: Option<i64> =
        sqlx::query_scalar("SELECT next_seq FROM outbox_delivery_seq WHERE consumer = $1")
            .bind(consumer)
            .fetch_optional(&mut *conn)
            .await?;
    let floor: Option<i64> = sqlx::query_scalar(
        "SELECT min(delivery_seq) FROM outbox_delivery \
         WHERE consumer = $1 AND delivery_seq IS NOT NULL",
    )
    .bind(consumer)
    .fetch_one(&mut *conn)
    .await?;
    Ok(StreamBounds {
        head: next.map_or(0, |n| n - 1),
        floor,
    })
}

/// ストリームを撤去する（採番行と台帳行。購読対象が削除されたとき）。
pub async fn drop_stream(conn: &mut PgConnection, consumer: &str) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM outbox_delivery WHERE consumer = $1")
        .bind(consumer)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM outbox_delivery_seq WHERE consumer = $1")
        .bind(consumer)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::StreamBounds;

    #[test]
    fn resumable_requires_contiguous_retention() {
        let b = StreamBounds {
            head: 10,
            floor: Some(4),
        };
        assert!(b.resumable(3), "4.. が保持されている");
        assert!(b.resumable(9));
        assert!(b.resumable(10), "最新");
        assert!(!b.resumable(2), "3 が GC 済み");
        assert!(!b.resumable(11), "未来の位置は不正");
        assert!(!b.resumable(-1));
        let empty = StreamBounds {
            head: 10,
            floor: None,
        };
        assert!(empty.resumable(10));
        assert!(!empty.resumable(9), "保持が無い = 9 以降は失われた");
        let fresh = StreamBounds {
            head: 0,
            floor: None,
        };
        assert!(fresh.resumable(0));
    }
}
