//! `processed_at` で ack するコンシューマの登録（3 回目の検証 3-D-1）。
//!
//! `processed_at`（[`super::claim`] / [`super::mark_processed`]）で消費するのは RAG の relay だけで、
//! 配送台帳（`outbox_delivery`）には載らない。GC が `processed_at` を待つべきかは「この配備で
//! そのコンシューマが動いているか」で決まるので、台帳コンシューマ（`outbox_consumer`）と同じく
//! **登録を永続化**し、GC はそこから読む（フラグの違うレプリカが判断を割らない・
//! [`super::registered_consumers`] と同じ理由）。
//!
//! - 登録が 1 行も無い配備（RAG 無効）では、GC は `processed_at` を待たない
//!   （待つと outbox が無制限に増える・実際に 54 万行が 1 行も消えなかった）。
//! - 1 行でもあれば、`processed_at` が立つまで消さない（RAG 有効時の従来の挙動）。

use sqlx::PgConnection;

use crate::error::StorageError;

/// RAG の relay が `processed_at` の ack コンシューマとして登録する名前。
pub const RAG_ACK_CONSUMER: &str = "rag";

/// `processed_at` の ack コンシューマとして登録する（冪等）。初回の登録なら `true`。
///
/// 起動時に（コンシューマのループより前に）呼ぶ。以後、GC は `processed_at` が立つまで行を消さない。
pub async fn register_ack_consumer(
    conn: &mut PgConnection,
    name: &str,
) -> Result<bool, StorageError> {
    let first: Option<String> = sqlx::query_scalar(
        "INSERT INTO outbox_ack_consumer (name) VALUES ($1) \
         ON CONFLICT (name) DO NOTHING RETURNING name",
    )
    .bind(name)
    .fetch_optional(conn)
    .await?;
    Ok(first.is_some())
}

/// ack コンシューマを**恒久的に廃止**する（RAG を畳んだ配備で GC を `processed_at` から外す）。
///
/// ⚠️ 一時的な停止で呼ばない。再び登録するまでの間に GC が未 ack の行を消し、その間の書込は索引されない
/// （[`super::unregister_consumer`] と同じ注意）。
pub async fn unregister_ack_consumer(
    conn: &mut PgConnection,
    name: &str,
) -> Result<bool, StorageError> {
    let removed: Option<String> =
        sqlx::query_scalar("DELETE FROM outbox_ack_consumer WHERE name = $1 RETURNING name")
            .bind(name)
            .fetch_optional(conn)
            .await?;
    Ok(removed.is_some())
}

/// 登録されている ack コンシューマ名（GC が `processed_at` を待つかの正本）。
pub async fn ack_consumers(conn: &mut PgConnection) -> Result<Vec<String>, StorageError> {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM outbox_ack_consumer ORDER BY name")
            .fetch_all(conn)
            .await?;
    Ok(names)
}
