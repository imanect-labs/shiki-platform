//! 構造化データ（data）ノードの params 契約（Task 15.7・data-platform §11）。
//!
//! - **クエリは data の [`DataQuery`] をそのまま使う**（変換層・面ごとの独自フィルタ表現を作らない）。
//!   条件木の語彙は workflow IR の `Condition` と同じで、意味論も同じ 2 値論理（query-optimizer.md §3.1・
//!   `control/eval.rs` と共通の表で固定）。比較値（`cmp.value`・`in` の要素）には `$from` / `$template` を
//!   書け、実行直前に解決してから data へ渡す（値の差し込みであり、条件の表現は変えない）。
//! - 認可はチョークポイント（data の DataStore）側。engine は scope 天井（`data.read` / `data.write`）だけを
//!   担保する（二重ゲート）。schedule / event の run は委譲元ユーザーの権限で行が絞られる（ポート実装）。
//! - 書込（作成・更新・削除・遷移・一括 upsert）は EngineDedup（effect_journal の冪等キーで高々 1 回・PIT-31）。
//! - status は `data.transition` でしか動かせない（FSM の不変条件・`data.record.update` で status を書くと 422）。

use data::DataQuery;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ir::expr::ValueExpr;

/// `data.bulk_upsert` の 1 ノードあたりの行数上限（同期で取込を最後まで進めるため・step 出力 256KB と釣り合う）。
pub const MAX_WORKFLOW_BULK_ROWS: usize = 5_000;

/// `data.query` — テーブルを検索・集計する（viewer・行述語とフィールドマスクは実行主体の権限で適用・pure）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataQueryParams {
    /// 対象テーブル id（UUID に解決される値）。
    pub table: ValueExpr,
    /// クエリ（data の `DataQuery`・OpenAPI の `DataQuery` と同一型）。省略時は既定（更新の新しい順に 50 件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(
        type = "import(\"./api\").components[\"schemas\"][\"DataQuery\"]",
        optional
    )]
    pub query: Option<DataQuery>,
}

/// `data.record.create` — レコードを 1 件作る（editor・EngineDedup）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataRecordCreateParams {
    /// 対象テーブル id。
    pub table: ValueExpr,
    /// レコードの値（参照名 key → 値のオブジェクトに解決される値）。
    pub data: ValueExpr,
}

/// `data.record.update` — レコードを merge patch で更新する（editor ＋書込述語・EngineDedup）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataRecordUpdateParams {
    /// 対象テーブル id。
    pub table: ValueExpr,
    /// 対象レコード id。
    pub record: ValueExpr,
    /// 変更するフィールドだけのオブジェクト（`null` はフィールド除去）。
    pub patch: ValueExpr,
    /// 楽観ロックの rev（省略時は実行時点の rev を読んでから書く）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_rev: Option<ValueExpr>,
}

/// `data.record.delete` — レコードを削除する（editor ＋書込述語・EngineDedup）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataRecordDeleteParams {
    /// 対象テーブル id。
    pub table: ValueExpr,
    /// 対象レコード id。
    pub record: ValueExpr,
    /// 楽観ロックの rev（省略時は実行時点の rev を読んでから消す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_rev: Option<ValueExpr>,
}

/// `data.transition` — FSM の遷移 API を叩いて status を進める（ガード・actor 述語は data 側で再評価・EngineDedup）。
///
/// FSM の遷移は「現在の status → `to`」で定義される（data の `FsmBody`）。遷移名ではなく行き先の
/// status を指定する（定義外の遷移は 422）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataTransitionParams {
    /// 対象テーブル id。
    pub table: ValueExpr,
    /// 対象レコード id。
    pub record: ValueExpr,
    /// 遷移先の status。
    pub to: ValueExpr,
    /// 楽観ロックの rev（省略時は実行時点の rev を読んでから遷移する）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_rev: Option<ValueExpr>,
}

/// `data.bulk_upsert` — 行をまとめて作成・更新する（data の一括書込パス・EngineDedup）。
///
/// `rows` は `{ "id"?: uuid, "rev"?: number, "key"?: string, "data": {...} }` の配列に解決される値。
/// `id` のある行は既存レコードの更新（`data` は merge patch）、無い行は新規作成。更新行は 1 件の
/// 更新と同じ入口（行述語つきロック・書込述語・rev・マスク列・検証器・outbox）を通り、誤りはその行だけを
/// 拒否する。出力は件数と拒否行（行番号・行キー・理由）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataBulkUpsertParams {
    /// 対象テーブル id。
    pub table: ValueExpr,
    /// 行の配列（最大 5,000 行）。
    pub rows: ValueExpr,
    /// 不合格の行が 1 つでもあれば何も適用しない（既定は合格した行だけを適用する）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub abort_on_invalid: Option<bool>,
}
