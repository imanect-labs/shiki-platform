//! イベント source のカタログとペイロードの形の単一定義（D-47・BUG-36・codegen が正）。
//!
//! イベントトリガの run 入力（`$from trigger`）と wait(event) の出力は、イベント源ごとに決まった形の
//! JSON になる。その形を **ここだけ**に置く:
//!
//! - ペイロードを組む型（[`StorageWritePayload`]・[`DataChangePayload`]）。relay（api）はこの型でしか
//!   ペイロードを作らない。
//! - 項目の表（[`EventCatalogEntry::payload_fields`]）。`bin/export-ts.rs` が `EVENT_CATALOG` として web へ
//!   書き出し（値の欄の参照の候補）、AI の説明文（chat の `workflow_tool::guide`）も同じ表から作る。
//!
//! 型と表は単体テスト（`payload_fields_match_the_payload_types`）で突き合わせる。どちらかだけを変えると
//! テストが落ちる。

use serde::Serialize;
use serde_json::{Map, Value};
use ts_rs::TS;

use super::EventSource;

/// ペイロードの 1 項目（後続が `$from trigger` の path・wait(event) の出力の path で読む）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct EventPayloadField {
    /// 項目名（path は `/<name>`）。
    pub name: &'static str,
    /// 型（TS 風の表記）。
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub ty: &'static str,
    /// 何が入るか（利用者向けの日本語）。
    pub description_ja: &'static str,
}

/// イベント source 1 種のエントリ。
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct EventCatalogEntry {
    /// イベント source（トリガ・wait(event) の `source`）。
    #[ts(type = "EventSource")]
    pub source: EventSource,
    /// 日本語の名前（「〜したとき」）。
    pub label_ja: &'static str,
    /// scope に書く束縛のキー（`folder` / `table`・予約語彙は空）。
    pub scope_key: &'static str,
    /// 現ステージで保存・実行できるか。
    pub available: bool,
    /// ペイロードの項目（予約語彙は空）。
    pub payload_fields: &'static [EventPayloadField],
}

const fn field(
    name: &'static str,
    ty: &'static str,
    description_ja: &'static str,
) -> EventPayloadField {
    EventPayloadField {
        name,
        ty,
        description_ja,
    }
}

/// ファイル保存（`storage.write`）のペイロードの項目（BUG-8）。
const STORAGE_WRITE_FIELDS: &[EventPayloadField] = &[
    field("file_id", "string", "保存されたファイルの ID"),
    field("version", "number", "保存された版の番号"),
    field(
        "op",
        "string",
        "\"create\"（新しいファイル）か \"update\"（新しい版）",
    ),
    field("kind", "string", "常に \"file\""),
    field("parent_id", "string | null", "保存先のフォルダの ID"),
    field("size", "number | null", "ファイルの大きさ（バイト）"),
    field(
        "actor",
        "string",
        "保存した人の ID（ワークフローが保存したときはワークフローの ID）",
    ),
    field("blob_sha256", "string | null", "内容のハッシュ"),
];

/// データ変更（`data.record.created` / `data.record.updated` / `data.transition`）のペイロードの項目（GAP-07）。
const DATA_CHANGE_FIELDS: &[EventPayloadField] = &[
    field(
        "source",
        "string",
        "できごとの種類（トリガの source と同じ）",
    ),
    field("table_id", "string", "テーブルの ID"),
    field("record_id", "string", "追加・更新された行の ID"),
    field("rev", "number", "変更後の行の版"),
    field(
        "change",
        "string",
        "\"create\"（追加）・\"update\"（更新）・\"transition\"（状態の変更）",
    ),
    field(
        "from",
        "string | null",
        "変更前の状態（状態が変わったときだけ）",
    ),
    field(
        "to",
        "string | null",
        "変更後の状態（状態が変わったときだけ）",
    ),
    field(
        "changed",
        "Array<string>",
        "変わった列の参照名（有効化した人に読める列だけ）",
    ),
    field(
        "changes",
        "{ [列の参照名]: { old: unknown, new: unknown } }",
        "変わった列ごとの変更前と変更後の値",
    ),
    field(
        "actor",
        "string",
        "変更した人の ID（ワークフローが書いたときは有効化した人）",
    ),
    field(
        "by_workflow",
        "boolean",
        "ワークフローの書込による変更か（人の操作だけで動かすなら false で絞る）",
    ),
    field("occurred_at", "string", "変更の日時（RFC 3339）"),
    field(
        "record",
        "{ id: string, data: { [列の参照名]: unknown }, rev: number, owner: string, … } | null",
        "今の行（有効化した人に見える値・大きすぎると null）",
    ),
    field(
        "truncated",
        "boolean",
        "大きすぎて record と changes を省いたか",
    ),
];

/// イベント source のカタログ全量（[`EventSource::ALL`] と同順・exhaustive）。
#[must_use]
pub fn event_catalog() -> Vec<EventCatalogEntry> {
    EventSource::ALL.iter().map(|s| entry(*s)).collect()
}

/// 1 つの source のペイロードの項目（予約語彙は空）。
#[must_use]
pub fn payload_fields(source: EventSource) -> &'static [EventPayloadField] {
    entry(source).payload_fields
}

fn entry(source: EventSource) -> EventCatalogEntry {
    let (label_ja, scope_key, payload_fields): (_, _, &'static [EventPayloadField]) = match source {
        EventSource::StorageWrite => ("ファイルが保存されたとき", "folder", STORAGE_WRITE_FIELDS),
        EventSource::DataRecordCreated => {
            ("テーブルに行が追加されたとき", "table", DATA_CHANGE_FIELDS)
        }
        EventSource::DataRecordUpdated => {
            ("テーブルの行が更新されたとき", "table", DATA_CHANGE_FIELDS)
        }
        EventSource::DataTransition => (
            "テーブルの行の状態が変わったとき",
            "table",
            DATA_CHANGE_FIELDS,
        ),
        EventSource::EventCustom => ("ほかのフローからの知らせ", "", &[]),
        EventSource::WebhookReceived => ("外部からの呼び出し（Webhook）", "", &[]),
    };
    EventCatalogEntry {
        source,
        label_ja,
        scope_key,
        available: source.available_stage_a(),
        payload_fields,
    }
}

/// ファイル保存（`storage.write`）のペイロード（`$from trigger` と wait(event) の出力）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StorageWritePayload {
    pub file_id: String,
    pub version: i64,
    pub op: String,
    pub kind: String,
    /// outbox にあった値をそのまま（無ければ null）。
    pub parent_id: Value,
    pub size: Value,
    pub actor: String,
    pub blob_sha256: Value,
}

/// データ変更のペイロード（`$from trigger` と wait(event) の出力）。値は委譲元（トリガ）・実行主体（wait）の
/// 見え方で作る（data の `change_event_view`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataChangePayload {
    pub source: String,
    pub table_id: String,
    pub record_id: String,
    pub rev: i64,
    pub change: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub changed: Vec<String>,
    pub changes: Map<String, Value>,
    pub actor: String,
    pub by_workflow: bool,
    pub occurred_at: String,
    pub record: Option<Value>,
    pub truncated: bool,
}

/// データ変更のペイロードの大きさの上限（超えたら `record` と `changes` を落とす）。
pub const MAX_DATA_PAYLOAD_BYTES: usize = 64 * 1024;

impl DataChangePayload {
    /// JSON にする。[`MAX_DATA_PAYLOAD_BYTES`] を超えたら `record` と `changes` を落として `truncated`。
    #[must_use]
    pub fn into_value(mut self) -> Value {
        let full = serde_json::to_value(&self).unwrap_or(Value::Null);
        if serde_json::to_vec(&full).map_or(0, |b| b.len()) <= MAX_DATA_PAYLOAD_BYTES {
            return full;
        }
        self.record = None;
        self.changes = Map::new();
        self.truncated = true;
        serde_json::to_value(&self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    fn names(fields: &[EventPayloadField]) -> Vec<String> {
        let mut n: Vec<String> = fields.iter().map(|f| f.name.to_string()).collect();
        n.sort();
        n
    }

    /// 型と表のどちらかだけを変えると落ちる（ペイロードの形の単一定義を保つ）。
    #[test]
    fn payload_fields_match_the_payload_types() {
        let storage = serde_json::to_value(StorageWritePayload {
            file_id: "f".into(),
            version: 1,
            op: "create".into(),
            kind: "file".into(),
            parent_id: Value::Null,
            size: json!(1),
            actor: "a".into(),
            blob_sha256: Value::Null,
        })
        .unwrap();
        assert_eq!(
            keys(&storage),
            names(payload_fields(EventSource::StorageWrite))
        );
        let data = DataChangePayload {
            source: "data.record.created".into(),
            table_id: "t".into(),
            record_id: "r".into(),
            rev: 1,
            change: "create".into(),
            from: None,
            to: None,
            changed: vec![],
            changes: Map::new(),
            actor: "a".into(),
            by_workflow: false,
            occurred_at: "2026-10-25T00:00:00Z".into(),
            record: None,
            truncated: false,
        }
        .into_value();
        for s in [
            EventSource::DataRecordCreated,
            EventSource::DataRecordUpdated,
            EventSource::DataTransition,
        ] {
            assert_eq!(keys(&data), names(payload_fields(s)), "{}", s.as_str());
        }
    }

    /// 使える source はすべて項目と scope のキーを持つ（予約語彙は持たない）。
    #[test]
    fn catalog_covers_every_source_and_available_ones_have_fields() {
        let cat = event_catalog();
        assert_eq!(cat.len(), EventSource::ALL.len());
        for e in &cat {
            assert_eq!(
                e.payload_fields.is_empty(),
                e.scope_key.is_empty(),
                "{}",
                e.source.as_str()
            );
            if e.available {
                assert!(!e.payload_fields.is_empty(), "{}", e.source.as_str());
            }
        }
        assert!(payload_fields(EventSource::EventCustom).is_empty());
    }

    #[test]
    fn oversized_data_payload_drops_record_and_changes() {
        let big = "x".repeat(MAX_DATA_PAYLOAD_BYTES);
        let mut changes = Map::new();
        changes.insert("memo".into(), json!({ "old": null, "new": big.clone() }));
        let v = DataChangePayload {
            source: "data.record.updated".into(),
            table_id: "t".into(),
            record_id: "r".into(),
            rev: 2,
            change: "update".into(),
            from: None,
            to: None,
            changed: vec!["memo".into()],
            changes,
            actor: "a".into(),
            by_workflow: false,
            occurred_at: "2026-10-25T00:00:00Z".into(),
            record: Some(json!({ "data": { "memo": big } })),
            truncated: false,
        }
        .into_value();
        assert_eq!(v["truncated"], true);
        assert_eq!(v["record"], Value::Null);
        assert_eq!(v["changes"], json!({}));
        // 変わった列の名前は残す（filter の /changed は使える）。
        assert_eq!(v["changed"], json!(["memo"]));
    }
}
