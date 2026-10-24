//! SSE の配信イベント（`GET /data/tables/{id}/stream`・data-platform §9.3）。型は Rust が単一ソースで、
//! OpenAPI（utoipa）から TS 型を生成する（手書きの対応物を作らない）。
//!
//! ```text
//! event: stream.ready      id=<delivery_seq>   data={position}              接続直後の再開位置
//! event: record.upserted   id=<delivery_seq>   data={record, derived}       マスク・派生値は購読者本人の権限
//! event: record.removed    id=<delivery_seq>   data={record_id}             値を含まない（PIT-73）
//! event: schema.changed    id=<delivery_seq>   data={table_id, schema_version} 受けたら取り直す（接続は閉じる）
//! event: stream.reset      id=<delivery_seq>   data={reason}                手元を捨てて再取得（接続は閉じる）
//! event: awareness                              data={principal, record_id, field, at}  データを含まない
//! event: records.bulk_applied id=<delivery_seq> data={table_id}            一括取込で行が作られた（取り直す）
//! event: keepalive                              data={at}                  15 秒ごと（途絶したら再接続する）
//! ```
//!
//! **一括取込**（検証 D-11・3-D-2）: 取込が作った行は 1 件ずつの `record.upserted` ではなく、続く範囲ごとに
//! `records.bulk_applied` 1 件にまとめる。行ごとの可視性は判定せず、**件数も値も含めない**（見えない行まで
//! 数えた件数は、見えない行が何件取り込まれたかを漏らす）。クライアントは「取り込まれた」と案内して取り直す
//! （見える行だけが返る）。`id` はまとめた最後の位置（`Last-Event-ID` で欠落なく再開できる）。
//!
//! # クライアントの冪等規則（at-least-once・§9.2）
//!
//! - `record.upserted` は `rev` が手元より大きければ適用する。`derived = true`（派生値だけの再配信・
//!   `rev` は進まない）は `rev` が手元以上なら派生値を適用する。それ以外（既知の `rev` 以下）は捨てる。
//! - 手元に無い行への `record.removed` は捨てる。
//! - `Last-Event-ID` は**連続して処理し終えた位置**（サーバが昇順で送るので最後に受けた `id`）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::model::DataRecord;

/// 接続直後の再開位置（`Last-Event-ID` の初期値）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StreamReadyEvent {
    pub position: i64,
}

/// 行の新規出現・更新（マスク・派生値は購読者本人の権限で計算済み）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecordUpsertedEvent {
    pub record: DataRecord,
    /// 派生値（ルックアップ・ロールアップ・数式・対称列）だけの再配信（`rev` は進まない）。
    pub derived: bool,
}

/// 行が見えなくなった・削除された（**値を含まない**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecordRemovedEvent {
    pub record_id: Uuid,
}

/// スキーマが改訂された（クライアントはスキーマと行を取り直し、`Last-Event-ID` から再接続する）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SchemaChangedEvent {
    pub table_id: Uuid,
    pub schema_version: i64,
}

/// ストリームを続けられない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreamResetReason {
    /// 指定した位置が保持期間外（`Last-Event-ID` から欠落なく再開できない）。
    Retention,
    /// 権限（ロール・共有・マスク）が変わった。手元の行を捨てて取り直す。
    AuthzChanged,
    /// 派生値の影響が大きく、行ごとの再配信を畳んだ。参照先テーブルのスキーマ・権限が変わり、
    /// ルックアップ・リンク値の見え方が変わった場合も同じ（手元を捨てて取り直す）。
    Refresh,
    /// 購読者の受信が追いつかない（送信待ちが溢れた）。
    Overflow,
    /// テーブルが削除された。
    TableDeleted,
}

/// 手元の状態を捨てて再取得させる（以後の冪等判定の前提が崩れたため）。接続は閉じる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StreamResetEvent {
    pub reason: StreamResetReason,
}

/// 他ユーザーのカーソル・選択範囲（**データを含まない**・§9.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AwarenessEvent {
    pub principal: String,
    pub record_id: Option<Uuid>,
    pub field: Option<String>,
    pub at: DateTime<Utc>,
}

/// 一括取込で行が作られた（**件数・行・値を含まない**・取込の行を 1 件ずつ配らない）。
///
/// 購読者ごとの可視性は判定していないので、受けたら取り直して見える行を確かめる。件数を載せないのは、
/// 見えない行まで数えた件数が「見えない行が何件取り込まれたか」を漏らすため。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecordsBulkAppliedEvent {
    pub table_id: Uuid,
}

/// 接続が生きていることの通知（15 秒ごと・`at` はサーバの時刻）。途絶したらクライアントは繋ぎ直す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StreamKeepaliveEvent {
    pub at: DateTime<Utc>,
}

/// 配信イベント。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StreamEvent {
    Ready(StreamReadyEvent),
    Upserted(RecordUpsertedEvent),
    Removed(RecordRemovedEvent),
    SchemaChanged(SchemaChangedEvent),
    Reset(StreamResetEvent),
    Awareness(AwarenessEvent),
    BulkApplied(RecordsBulkAppliedEvent),
    Keepalive(StreamKeepaliveEvent),
}

impl StreamEvent {
    /// SSE の `event:` 名。
    pub fn name(&self) -> &'static str {
        match self {
            StreamEvent::Ready(_) => "stream.ready",
            StreamEvent::Upserted(_) => "record.upserted",
            StreamEvent::Removed(_) => "record.removed",
            StreamEvent::SchemaChanged(_) => "schema.changed",
            StreamEvent::Reset(_) => "stream.reset",
            StreamEvent::Awareness(_) => "awareness",
            StreamEvent::BulkApplied(_) => "records.bulk_applied",
            StreamEvent::Keepalive(_) => "keepalive",
        }
    }

    /// SSE の `data:`（イベント本体の JSON）。
    pub fn data_json(&self) -> serde_json::Value {
        let v = match self {
            StreamEvent::Ready(e) => serde_json::to_value(e),
            StreamEvent::Upserted(e) => serde_json::to_value(e),
            StreamEvent::Removed(e) => serde_json::to_value(e),
            StreamEvent::SchemaChanged(e) => serde_json::to_value(e),
            StreamEvent::Reset(e) => serde_json::to_value(e),
            StreamEvent::Awareness(e) => serde_json::to_value(e),
            StreamEvent::BulkApplied(e) => serde_json::to_value(e),
            StreamEvent::Keepalive(e) => serde_json::to_value(e),
        };
        v.unwrap_or(serde_json::Value::Null)
    }

    /// これを送ったら接続を閉じる（クライアントは取り直してから再接続する）。
    pub fn is_terminal(&self) -> bool {
        matches!(self, StreamEvent::SchemaChanged(_) | StreamEvent::Reset(_))
    }
}

/// 配信の 1 件（`id` は SSE の `id:`＝`delivery_seq`。awareness は位置を持たない）。
#[derive(Debug, Clone)]
pub struct StreamMessage {
    pub id: Option<i64>,
    pub event: StreamEvent,
}
