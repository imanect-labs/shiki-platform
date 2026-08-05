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
//! **モジュール構成**:
//! - 本モジュール: 発行（[`emit_on`]）・`processed_at` 経路の消費（[`claim`]/[`mark_processed`]）・
//!   読み取り専用のライブテール（[`latest_event_id`]/[`peek_app_events_after`]）。
//! - [`delivery`]: per-consumer 配送台帳（P10-A0）。
//! - [`gc`]: GC と滞留観測（#413）。バックグラウンド実行は [`crate::outbox_gc`]。
//!
//! [`StorageService`]: crate::service::StorageService

mod delivery;
mod gc;

pub use delivery::{
    claim_undelivered, mark_delivered, register_consumer, registered_consumers, unregister_consumer,
};
pub use gc::{
    gc_delivered, gc_delivered_registered, outbox_backlog, ConsumerLag, OutboxBacklog,
    DEFAULT_GC_BATCH,
};

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
    /// 対象ノードが**システム領域**か（#392・`node.system`）。
    ///
    /// 索引消費者（`shiki_rag` の relay）はこれが true のイベントを **rag_ingest へ流さない**
    /// （使い捨ての作業メモを社内検索に載せない）。outbox 自体は忠実なログのままにしておき、
    /// 「索引すべきか」の判断は索引側に置く（他の consumer の挙動を変えない）。
    pub system: bool,
}

/// 未処理イベントを FIFO で `limit` 件まで取り出す（**呼び出し側の txn 上で**）。
///
/// `FOR UPDATE SKIP LOCKED` で同時消費者が同じ行を二重に掴まないようにする。掴んだ行は
/// 同一 txn 内で処理 → [`mark_processed`] → commit する。commit 前に失敗すればロックが解放され
/// **未処理のまま再配信**される（at-least-once）。
pub async fn claim(conn: &mut PgConnection, limit: i64) -> Result<Vec<OutboxEvent>, StorageError> {
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT o.id, o.org, o.tenant_id, o.node_id, o.version, o.op, o.actor, o.trace_id, \
                o.payload, o.created_at, coalesce(n.system, false) AS system \
         FROM storage_event_outbox o \
         LEFT JOIN node n ON n.id = o.node_id \
         WHERE o.processed_at IS NULL \
         ORDER BY o.id \
         FOR UPDATE OF o SKIP LOCKED \
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
// 読み取り専用の覗き見（app-gateway `events.subscribe`・Task 9.8）。
//
// 配送状態（processed_at / outbox_delivery）に一切触れない**ライブテール**用。SSE 購読者は
// 接続時点のカーソル（[`latest_event_id`]）から [`peek_app_events_after`] をポーリングする。
// 耐久配送ではない（GC 済みイベントは見えない）——B2 関数トリガの at-least-once 消費は
// [`claim_undelivered`]（配送台帳）を使うこと。
// ---------------------------------------------------------------------------

/// テナント内 outbox の現在の最大 id（SSE 購読の開始カーソル）。イベントが無ければ 0。
pub async fn latest_event_id(pool: &sqlx::PgPool, tenant_id: &str) -> Result<i64, StorageError> {
    let (id,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(id), 0) FROM storage_event_outbox WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// `after_id` より後の**アプリ向けドメインイベント**（`payload.event_type` 付き）を id 順に返す。
///
/// storage の生の書込イベント（file create 等・event_type なし）は含めない（ノード可視性の
/// 再検証なしにアプリへ流さない）。呼び出し側がテーブル束縛等の追加フィルタを行う。
pub async fn peek_app_events_after(
    pool: &sqlx::PgPool,
    tenant_id: &str,
    after_id: i64,
    limit: i64,
) -> Result<Vec<OutboxEvent>, StorageError> {
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT o.id, o.org, o.tenant_id, o.node_id, o.version, o.op, o.actor, o.trace_id, \
                o.payload, o.created_at, coalesce(n.system, false) AS system \
         FROM storage_event_outbox o \
         LEFT JOIN node n ON n.id = o.node_id \
         WHERE o.tenant_id = $1 AND o.id > $2 AND o.payload ? 'event_type' \
         ORDER BY o.id LIMIT $3",
    )
    .bind(tenant_id)
    .bind(after_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(OutboxRow::into_event).collect())
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
    /// `#[sqlx(default)]`: **この行構造体は 3 つのクエリで共有される**
    /// （本モジュールの [`claim`]・[`peek_app_events_after`]、および
    /// [`delivery::claim_undelivered`]）。列を足し忘れたクエリがあると `query_as` は実行時に
    /// 失敗し、その経路の機能が丸ごと止まる（app-gateway の SSE が無音になる形で実際に踏んだ）。
    /// 既定 false へ落とすことで、最悪でも「system 判定が付かない＝従来どおり索引する」に留める。
    /// 判定を必要とする経路（rag の relay）が読むクエリには必ず列を入れること。
    #[sqlx(default)]
    system: bool,
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
            system: self.system,
        }
    }
}
