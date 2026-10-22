//! スキーマ・レコード・リビジョンのドメイン型（DTO 兼用・utoipa で OpenAPI へ流す）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// フィールド型（閉じた集合・Task 9.2）。
///
/// `lookup` / `computed` は**書込不可の派生フィールド**で、読み出し解決は
/// 参照先テーブルの行ポリシー透過適用（Task 9.3・PIT-20）と同時に解禁する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Number,
    /// `YYYY-MM-DD`（保存形式そのまま・辞書順＝日付順）。
    Date,
    /// RFC3339 を受理し **UTC の固定幅 ISO-8601 文字列に正規化して保存**する
    /// （辞書順＝時刻順になり、text 式インデックスで範囲検索できる）。
    DateTime,
    Select,
    MultiSelect,
    /// ユーザー参照（principal id・directory で存在検証）。
    UserRef,
    /// ロール（部署）参照（role id・directory で存在検証）。
    RoleRef,
    /// ファイル参照（StorageService 経由で存在＋呼出ユーザーの可読を検証）。
    FileRef,
    /// 他テーブルのレコード参照（`ref_table` 必須・同一テナント内で存在検証）。
    RecordRef,
    /// record 参照を辿った参照先フィールドの射影（書込不可・解決は Task 9.3）。
    Lookup,
    /// 他フィールドからの計算値（書込不可・評価は Task 9.4 のクエリ層）。
    Computed,
}

/// lookup 定義: `via_field`（自テーブルの record_ref フィールド）を辿り、
/// 参照先テーブルの `target_field` を射影する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LookupDef {
    pub via_field: String,
    pub target_field: String,
}

/// 計算フィールドの演算（閉じた集合。自由式は持たない＝インジェクション面を作らない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComputedOp {
    /// number フィールドの合計。
    Sum,
    /// text フィールドの連結。
    Concat,
}

/// 計算フィールド定義（対象は自テーブルの宣言済みフィールドのみ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComputedDef {
    pub op: ComputedOp,
    pub fields: Vec<String>,
}

/// フィールドの内部 ID（テーブル内で採番・不変・再利用しない・data-platform §4）。
///
/// JSONB キー（`"f{id}"`）と索引テーブルの `field_id`（`int2`）に使う。PostgreSQL に unsigned は
/// 無いため上限は [`MAX_FIELD_ID`]。`0..16` はシステム擬似列に予約（[`FIRST_USER_FIELD_ID`]）。
pub type FieldId = u16;

/// `FieldId` の上限（`int2` の正の最大値。採番時とスキーマ検証で強制・data-platform §3.2）。
pub const MAX_FIELD_ID: FieldId = 32_767;
/// ユーザー定義フィールドの最初の ID（`0..16` はシステム擬似列に予約）。
pub const FIRST_USER_FIELD_ID: FieldId = 16;
/// システム擬似列: レコード作成者（owner）。
pub const SYSTEM_FIELD_OWNER: FieldId = 0;
/// システム擬似列: 作成時刻。
pub const SYSTEM_FIELD_CREATED_AT: FieldId = 1;
/// システム擬似列: 更新時刻。
pub const SYSTEM_FIELD_UPDATED_AT: FieldId = 2;

/// フィールド定義（識別子 3 層・data-platform §4）。
///
/// | 層 | 役割 | 変更 |
/// |---|---|---|
/// | `id` | JSONB キー（`"f{id}"`）・索引の `field_id` | 不可 |
/// | `key` | API / shiki script / workflow / SDK の参照名 | 不可 |
/// | `display_name` | 画面表示（日本語可） | 自由（データは 1 行も動かない） |
///
/// 外部契約（レコードの `data`・フィルタ・ソート・集計・row_policy・field_policy・lookup・
/// computed・status_field）は **`key` で受けて `key` で返す**。`"f{id}"` への変換は
/// `crates/data` 内（保存・SQL 生成）に閉じる。
///
/// 入力の後方互換（§4.3）: v1 の `name` は `key` の別名として受ける。`id` を省略したフィールドは
/// 既存フィールドの `key` と照合して ID を引き継ぎ、無ければ新規採番する。`display_name` を
/// 省略したら `key` を使う。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[allow(clippy::struct_excessive_bools)] // 宣言フラグ群（required/unique/indexed/searchable）は API 契約そのもの。
pub struct FieldDef {
    /// 内部 ID（サーバ採番・不変）。入力で省略（0）したら採番・照合する。
    #[serde(default)]
    pub id: FieldId,
    /// 参照名（`^[a-z][a-z0-9_]{0,63}$`・**不変**。v1 の `name` も別名として受理する）。
    #[serde(alias = "name")]
    pub key: String,
    /// 表示名（自由文・日本語可・いつでも変更可）。省略時は `key`。
    #[serde(default)]
    pub display_name: String,
    /// 既定の表示順（ビューが上書き可能）。全フィールド省略時は宣言順。
    #[serde(default)]
    pub order: i32,
    /// 説明（列ヘッダのツールチップ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    /// 必須（create 時に存在・null 不可。update でも除去不可）。
    #[serde(default)]
    pub required: bool,
    /// テーブル内一意（unique 式インデックスで競合レースなく強制・null は重複可）。
    #[serde(default)]
    pub unique: bool,
    /// フィルタ/ソート対象（式インデックスを生成する）。
    #[serde(default)]
    pub indexed: bool,
    /// 部分一致検索の対象（Task 15.2 の `data_index_search` へ載せる・text 型のみ）。
    #[serde(default)]
    pub searchable: bool,
    /// select / multi_select の選択肢（閉じた集合）。
    #[serde(default)]
    pub options: Vec<String>,
    /// record_ref の参照先テーブル。
    #[serde(default)]
    pub ref_table: Option<Uuid>,
    /// lookup 定義（field_type=lookup のとき必須）。
    #[serde(default)]
    pub lookup: Option<LookupDef>,
    /// 計算定義（field_type=computed のとき必須）。
    #[serde(default)]
    pub computed: Option<ComputedDef>,
}

impl FieldDef {
    /// 最小のフィールド定義（`id` 未採番・`display_name` = `key`）。
    pub fn new(key: impl Into<String>, field_type: FieldType) -> Self {
        let key = key.into();
        FieldDef {
            id: 0,
            display_name: key.clone(),
            key,
            order: 0,
            description: None,
            field_type,
            required: false,
            unique: false,
            indexed: false,
            searchable: false,
            options: Vec::new(),
            ref_table: None,
            lookup: None,
            computed: None,
        }
    }

    /// 保存形（`data_record.data`）の JSONB キー `"f{id}"`。
    pub(crate) fn json_key(&self) -> String {
        crate::field_key::json_key(self.id)
    }
}

/// フィールドマスク（Task 9.4・PIT-19）。
///
/// `readable_by` を満たさない実行主体には、対象フィールドを**応答から除去**し、かつ
/// **filter/sort/group_by/metrics の対象からも除外**する（「表示を隠す」と「検索に
/// 使わせない」を同時に強制。ソート順・絞り込み・集計値からの推測を塞ぐ）。
/// 式はロール/ユーザーレベル（has_role/public とその any/all 合成）のみ
/// （行の値に依存する式は不可＝リクエスト単位で一度だけ評価できる形に限定）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FieldPolicy {
    pub field: String,
    pub readable_by: crate::policy::PolicyExpr,
}

/// テーブルスキーマ（`data_table.schema` JSONB の正本型）。
///
/// Task 9.4 で `field_policy` / `aggregate_min_rows`、9.10 で `fsm_ref` を
/// 同じ構造体に追記する（serde default で後方互換）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TableSchema {
    pub fields: Vec<FieldDef>,
    /// FSM（Task 9.10）が状態として扱う select フィールド名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_field: Option<String>,
    /// 行レベル認可の宣言（Task 9.3）。未定義はテーブル viewer 全員が全行可視
    /// （行制限はオプトイン。テーブル自体の ReBAC は常に第1層として効く）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_policy: Option<crate::policy::RowPolicy>,
    /// フィールドマスク（Task 9.4・PIT-19）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_policy: Vec<FieldPolicy>,
    /// 集計スモールセル抑制の最小件数 K（Task 9.4・PIT-17）。未指定は既定
    /// [`crate::DEFAULT_AGGREGATE_MIN_ROWS`]。既定未満へ下げる変更は監査に残る。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_min_rows: Option<i64>,
    /// FSM 参照（Task 9.10・status 遷移ガード）。定義があると status フィールドの直接更新は
    /// 禁止され、遷移 API（`transition`）経由でのみ status を変えられる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsm_ref: Option<crate::fsm::FsmRef>,
}

impl TableSchema {
    /// フィールド定義を参照名（`key`）で引く。
    pub fn field(&self, key: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// フィールド定義を内部 ID で引く。
    pub fn field_by_id(&self, id: FieldId) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.id == id)
    }
}

/// テーブルのメタデータ。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataTable {
    pub id: Uuid,
    pub name: String,
    /// 所有ミニアプリ（Task 9.13 のプロビジョンが設定・null はスタンドアロン）。
    pub app_id: Option<Uuid>,
    pub schema: TableSchema,
    pub schema_version: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 内部整数キー（`tenant_int` / `table_int`）。**外へ出さない**（serde・OpenAPI から除外）。
    #[serde(skip)]
    pub(crate) keys: TableKeys,
}

/// テーブルの内部整数キー（data-platform §3.2）。crate 外から読めない・直列化しない。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TableKeys {
    pub tenant_int: i32,
    pub table_int: i64,
}

/// レコード。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataRecord {
    pub id: Uuid,
    pub table_id: Uuid,
    pub data: serde_json::Value,
    /// 楽観ロック用リビジョン（更新ごとに +1・不一致は 409）。
    pub rev: i64,
    pub owner: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// フィールド単位の変更差分（リビジョン changelog の 1 要素・Task 9.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FieldPatch {
    pub field: String,
    /// 変更前の値（create では null）。
    pub old: serde_json::Value,
    /// 変更後の値（delete では null）。
    pub new: serde_json::Value,
}

/// レコードのリビジョン（追記型 changelog の 1 行）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecordRevision {
    pub record_id: Uuid,
    pub rev: i64,
    pub changed_by: String,
    /// create / update / delete / transition（transition は Task 9.10）。
    pub change_kind: String,
    pub patch: Vec<FieldPatch>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_type_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&FieldType::MultiSelect).unwrap(),
            "\"multi_select\""
        );
        assert_eq!(
            serde_json::to_string(&FieldType::UserRef).unwrap(),
            "\"user_ref\""
        );
        let t: FieldType = serde_json::from_str("\"record_ref\"").unwrap();
        assert_eq!(t, FieldType::RecordRef);
    }

    #[test]
    fn field_type_unknown_fails_closed() {
        // 閉じた集合の外はデシリアライズ失敗（fail-closed）。
        let r: Result<FieldType, _> = serde_json::from_str("\"raw_sql\"");
        assert!(r.is_err());
    }

    #[test]
    fn table_schema_defaults_are_backward_compatible() {
        // 既存行（fields のみ）が読めること（後続 PR のフィールド追記に備えた前提確認）。
        let schema: TableSchema = serde_json::from_str(
            r#"{ "fields": [{ "name": "title", "type": "text", "required": true }] }"#,
        )
        .unwrap();
        assert_eq!(schema.fields.len(), 1);
        // v1 の `name` は `key` の別名として読める（§4.3 の後方互換）。
        assert_eq!(schema.fields[0].key, "title");
        assert_eq!(schema.fields[0].id, 0, "id は未採番（サーバが採番する）");
        assert!(schema.fields[0].required);
        assert!(!schema.fields[0].unique);
        assert!(schema.status_field.is_none());
        assert!(schema.field("title").is_some());
        assert!(schema.field("missing").is_none());
    }

    #[test]
    fn field_def_serializes_three_layers_without_name() {
        let mut f = FieldDef::new("applicant", FieldType::UserRef);
        f.id = 17;
        f.display_name = "申請者".into();
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["id"], 17);
        assert_eq!(v["key"], "applicant");
        assert_eq!(v["display_name"], "申請者");
        assert!(v.get("name").is_none(), "v1 の name は出力しない");
        assert_eq!(f.json_key(), "f17");
    }

    #[test]
    fn data_table_never_serializes_internal_keys() {
        let ts = chrono::DateTime::from_timestamp(0, 0).unwrap();
        let t = DataTable {
            id: Uuid::nil(),
            name: "t".into(),
            app_id: None,
            schema: TableSchema {
                fields: vec![],
                status_field: None,
                row_policy: None,
                field_policy: vec![],
                aggregate_min_rows: None,
                fsm_ref: None,
            },
            schema_version: 1,
            created_by: "u".into(),
            created_at: ts,
            updated_at: ts,
            keys: TableKeys {
                tenant_int: 424_242,
                table_int: 7_777_777,
            },
        };
        let s = serde_json::to_string(&t).unwrap();
        assert!(!s.contains("tenant_int") && !s.contains("table_int"), "{s}");
        assert!(!s.contains("424242") && !s.contains("7777777"), "{s}");
    }

    #[test]
    fn openapi_schema_has_three_layer_fields_and_no_internal_keys() {
        use utoipa::PartialSchema;
        let field = serde_json::to_string(&<FieldDef as PartialSchema>::schema()).unwrap();
        for p in [
            "\"id\"",
            "\"key\"",
            "\"display_name\"",
            "\"order\"",
            "\"searchable\"",
        ] {
            assert!(field.contains(p), "FieldDef に {p} が無い: {field}");
        }
        for schema in [
            serde_json::to_string(&<DataTable as PartialSchema>::schema()).unwrap(),
            serde_json::to_string(&<DataRecord as PartialSchema>::schema()).unwrap(),
            serde_json::to_string(&<RecordRevision as PartialSchema>::schema()).unwrap(),
        ] {
            for k in ["record_int", "table_int", "tenant_int"] {
                assert!(
                    !schema.contains(k),
                    "{k} が API スキーマに出ている: {schema}"
                );
            }
        }
    }
}
