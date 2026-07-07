//! 書込イベント発行（Task 1.8）。
//!
//! create/update/delete/move 等の書込ごとに**ドメインイベント**（node_id, version, op,
//! org, actor）を `storage_event_outbox` へ発行する。発行は各 [`StorageService`] 書込
//! メソッドの**既存トランザクションに相乗り**し（[`emit_on`]）、メタ書込と原子的にコミット
//! される（outbox パターン＝書込とイベントの整合を担保）。
//!
//! 購読側（Phase 2 ingestion）は [`claim`] → 処理 → [`mark_processed`] の順で **at-least-once**
//! に消費する（commit 前にクラッシュすればロールバックで再配信される）。jobq への relay・
//! DLQ・リトライは消費者がいる Phase 2（Task 2.8）で配線する。FUSE 経由の書込（Phase 4）も
//! StorageService を通るため、この経路に自動で乗る。
//!
//! [`StorageService`]: crate::service::StorageService

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::StorageError;

/// 書込操作の種別。購読側は `op` で再索引の挙動を切り替える
/// （create/update→再パース、move/rename→authz_tags 再評価、delete→索引除去、restore→再索引）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOp {
    Create,
    Update,
    Rename,
    Move,
    Delete,
    Restore,
}

impl WriteOp {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteOp::Create => "create",
            WriteOp::Update => "update",
            WriteOp::Rename => "rename",
            WriteOp::Move => "move",
            WriteOp::Delete => "delete",
            WriteOp::Restore => "restore",
        }
    }
}

/// 発行する 1 件の書込イベント（正規化フィールド）。
///
/// `(node_id, version)` が購読側の冪等キー。`payload` には kind・blob_sha256・親の変化など
/// 消費者の利便/冪等に要る詳細を入れる（org/tenant_id/actor は `AuthContext` から束ねる）。
pub struct WriteEvent {
    pub node_id: Uuid,
    pub version: i64,
    pub op: WriteOp,
    pub payload: Value,
}

/// 既存トランザクション上で 1 件発行する（書込と同一 txn で原子的に outbox へ入れる）。
pub async fn emit_on(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    event: WriteEvent,
    trace_id: Option<&str>,
) -> Result<(), StorageError> {
    let payload_str = event.payload.to_string();
    sqlx::query(
        "INSERT INTO storage_event_outbox \
         (org, tenant_id, node_id, version, op, actor, trace_id, payload) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb)",
    )
    .bind(&ctx.org)
    .bind(&ctx.tenant_id)
    .bind(event.node_id)
    .bind(event.version)
    .bind(event.op.as_str())
    .bind(&ctx.principal.id)
    .bind(trace_id)
    .bind(&payload_str)
    .execute(conn)
    .await?;
    Ok(())
}

/// outbox から取り出した未処理イベント（購読側 DTO）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEvent {
    pub id: i64,
    pub org: String,
    pub tenant_id: String,
    pub node_id: Uuid,
    pub version: i64,
    pub op: String,
    pub actor: String,
    pub trace_id: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

/// 未処理イベントを FIFO で `limit` 件まで取り出す（**呼び出し側の txn 上で**）。
///
/// `FOR UPDATE SKIP LOCKED` で同時消費者が同じ行を二重に掴まないようにする。掴んだ行は
/// 同一 txn 内で処理 → [`mark_processed`] → commit する。commit 前に失敗すればロックが解放され
/// **未処理のまま再配信**される（at-least-once）。
pub async fn claim(conn: &mut PgConnection, limit: i64) -> Result<Vec<OutboxEvent>, StorageError> {
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT id, org, tenant_id, node_id, version, op, actor, trace_id, payload, created_at \
         FROM storage_event_outbox \
         WHERE processed_at IS NULL \
         ORDER BY id \
         FOR UPDATE SKIP LOCKED \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(OutboxRow::into_event).collect())
}

/// 処理済みイベントを ack する（`processed_at` を立てる・[`claim`] と同一 txn 内で呼ぶ）。
pub async fn mark_processed(conn: &mut PgConnection, ids: &[i64]) -> Result<(), StorageError> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query("UPDATE storage_event_outbox SET processed_at = now() WHERE id = ANY($1)")
        .bind(ids)
        .execute(conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// per-consumer fan-out（P10-A0・配送台帳 `outbox_delivery`）。
//
// 既存 [`claim`]/[`mark_processed`]（RAG relay 専用・`processed_at` 経路）はそのまま温存し、
// **追加コンシューマ**（workflow の event matcher 等）はこちらの配送台帳ベースの API を使う。
// これにより「片方の消費が他方を消す」取りこぼしを避けつつ、生成側（[`emit_on`]）は不変＝
// outbox が真の fan-out 点として機能する（roadmap/phase-10.md P10-A0）。
// ---------------------------------------------------------------------------

/// 指定コンシューマがまだ配送していない未処理イベントを FIFO で `limit` 件まで取り出す。
///
/// [`claim`] と同じく `FOR UPDATE SKIP LOCKED` で同時実行の二重取得を防ぐが、判定を
/// `processed_at` 破壊的消費ではなく **`NOT EXISTS(outbox_delivery for consumer)`** で行う。
/// 存在性ベースの anti-join なので id 順・コミット順に依存せず、**後からコミットした小さい id の
/// 行も次スキャンで拾える**（単純 last_seq カーソルの「未コミット飛び越し」問題を回避）。
/// 掴んだ行は同一 txn 内で処理 → [`mark_delivered`] → commit する（at-least-once）。
pub async fn claim_undelivered(
    conn: &mut PgConnection,
    consumer: &str,
    limit: i64,
) -> Result<Vec<OutboxEvent>, StorageError> {
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT o.id, o.org, o.tenant_id, o.node_id, o.version, o.op, o.actor, o.trace_id, \
                o.payload, o.created_at \
         FROM storage_event_outbox o \
         WHERE NOT EXISTS ( \
             SELECT 1 FROM outbox_delivery d \
             WHERE d.consumer = $1 AND d.event_id = o.id \
         ) \
         ORDER BY o.id \
         FOR UPDATE OF o SKIP LOCKED \
         LIMIT $2",
    )
    .bind(consumer)
    .bind(limit)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(OutboxRow::into_event).collect())
}

/// 指定コンシューマへの配送を台帳に記録する（[`claim_undelivered`] と同一 txn 内で呼ぶ）。
///
/// `(consumer, event_id)` 主キーで冪等（再配信で同じ行を掴んでも二重記録にならない）。
pub async fn mark_delivered(
    conn: &mut PgConnection,
    consumer: &str,
    ids: &[i64],
) -> Result<(), StorageError> {
    if ids.is_empty() {
        return Ok(());
    }
    // tenant_id は outbox 行から写す（台帳の絞り込み・監査用）。
    sqlx::query(
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id) \
         SELECT $1, o.id, o.tenant_id \
         FROM storage_event_outbox o \
         WHERE o.id = ANY($2) \
         ON CONFLICT (consumer, event_id) DO NOTHING",
    )
    .bind(consumer)
    .bind(ids)
    .execute(conn)
    .await?;
    Ok(())
}

/// 新規追加コンシューマを **現時点のバックログを飛ばして** 登録する（初回配送の暴発防止）。
///
/// 台帳ベースの [`claim_undelivered`] は「自分の delivery が無い行」を全て返すため、コンシューマを
/// 初めて有効化すると **過去の全 storage.write を再配送**してしまう（新規 workflow matcher が
/// 履歴イベントで一斉発火する）。これを避けるため、登録時に**現スナップショットで可視な**（＝コミット
/// 済みの）outbox 行を全て「配送済み」として台帳に刻む。**未コミットの in-flight イベントはこの
/// スナップショットに映らない**ため delivery が付かず、コミット後に正しく配送される（＝有効化以降の
/// イベントのみ処理・未コミット飛び越しも起こさない）。冪等（`ON CONFLICT DO NOTHING`）。
///
/// 起動時 wiring から consumer 有効化のたびに呼べる（**初回登録時のみ** fast-forward）。
///
/// `outbox_consumer` 台帳に consumer 名を一度だけ記録し、初回だけ現バックログを配送済みに刻む。
/// 2 回目以降（再起動）は no-op ＝ **停止中に到着した未配送イベントを取りこぼさない**。返り値は刻んだ件数。
pub async fn register_consumer(
    conn: &mut PgConnection,
    consumer: &str,
) -> Result<u64, StorageError> {
    // 初回登録か判定（RETURNING で挿入できたら初回）。
    let first: Option<String> = sqlx::query_scalar(
        "INSERT INTO outbox_consumer (name) VALUES ($1) \
         ON CONFLICT (name) DO NOTHING RETURNING name",
    )
    .bind(consumer)
    .fetch_optional(&mut *conn)
    .await?;
    if first.is_none() {
        // 既登録: fast-forward しない（未配送を温存）。
        return Ok(0);
    }
    let done = sqlx::query(
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id) \
         SELECT $1, o.id, o.tenant_id FROM storage_event_outbox o \
         ON CONFLICT (consumer, event_id) DO NOTHING",
    )
    .bind(consumer)
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

/// 全コンシューマへ配送済み **かつ** RAG（`processed_at`）ack 済みの outbox 行を GC する
/// （配送台帳は ON DELETE CASCADE で同時に消える）。
///
/// **未 ack の行は決して削除しない**（retention による time-based バイパスは持たない・遅い/停止中の
/// コンシューマがイベントを失わない）。`ledger_consumers` は「現在有効な台帳コンシューマ集合」を
/// 呼び出し側（起動時 wiring）が渡す。生成側は関与しない。空配列なら processed_at のみで判定する。
/// 返り値は削除件数。
pub async fn gc_delivered(
    conn: &mut PgConnection,
    ledger_consumers: &[&str],
) -> Result<u64, StorageError> {
    let consumers: Vec<String> = ledger_consumers.iter().map(|s| (*s).to_string()).collect();
    let expected = i64::try_from(consumers.len())
        .map_err(|_| StorageError::Invalid("consumer 数が多すぎます".into()))?;
    let deleted = sqlx::query(
        "DELETE FROM storage_event_outbox o \
         WHERE o.processed_at IS NOT NULL \
           AND ( \
               SELECT count(*) FROM outbox_delivery d \
               WHERE d.event_id = o.id AND d.consumer = ANY($1) \
           ) >= $2",
    )
    .bind(&consumers)
    .bind(expected)
    .execute(conn)
    .await?;
    Ok(deleted.rows_affected())
}

#[derive(sqlx::FromRow)]
struct OutboxRow {
    id: i64,
    org: String,
    tenant_id: String,
    node_id: Uuid,
    version: i64,
    op: String,
    actor: String,
    trace_id: Option<String>,
    payload: Value,
    created_at: DateTime<Utc>,
}

impl OutboxRow {
    fn into_event(self) -> OutboxEvent {
        OutboxEvent {
            id: self.id,
            org: self.org,
            tenant_id: self.tenant_id,
            node_id: self.node_id,
            version: self.version,
            op: self.op,
            actor: self.actor,
            trace_id: self.trace_id,
            payload: self.payload,
            created_at: self.created_at,
        }
    }
}
