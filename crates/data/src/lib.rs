//! 構造化データサービス（Phase 9 Task 9.2 / 9.5・design §4.10）。
//!
//! kintone 型の業務アプリを支える「管理されたテーブル」。ランタイム DDL を打たず、
//! 全テーブルの行を共有テーブル `data_record` に JSONB で格納する。
//!
//! 不変条件:
//! - **単一チョークポイント**: `data_table` / `data_record` への SQL は本 crate に閉じる。
//!   全公開メソッドは `&AuthContext` を取り、内部で OpenFGA check ＋監査を行う。
//! - **第1層 ReBAC**: テーブル＝OpenFGA `data_table` 型（owner/editor/viewer）。
//!   「テーブル＝ReBAC（少数）／行＝クエリ時述語（多数・Task 9.3）」の役割分担で
//!   全行をタプルにしない（タプル爆発防止）。
//! - **書込はサーバ検証**: 型・必須・unique・参照整合を書込時に強制（[`validate`]）。
//! - **追記型リビジョン**: 全書込がフィールド単位差分の changelog を同一 Tx で残し、
//!   `rev` の楽観ロックで同時更新を 409 にする（Task 9.5）。
//! - **lookup / 計算フィールドの読み出し解決は Task 9.3 まで封印**: 参照先テーブルの
//!   行ポリシー透過適用（PIT-20）なしに参照解決を出荷しない。定義の保存と検証のみ行う。

mod derived;
mod fsm;
mod fsm_store;
mod index;
mod mask;
mod model;
pub mod policy;
mod query;
mod record;
mod record_list;
mod record_share;
mod revision;
mod schema;
mod store;
mod table_list;
mod transition;
mod validate;
mod view;

pub use fsm::{FsmBody, FsmRef, FsmTransition};
pub use fsm_store::FsmStore;
pub use model::{
    ComputedDef, ComputedOp, DataRecord, DataTable, FieldDef, FieldPatch, FieldPolicy, FieldType,
    LookupDef, RecordRevision, TableSchema,
};
pub use policy::{CmpOp, PolicyExpr, PolicyOperand, RowPolicy};
pub use query::declarative::{
    Aggregate, AggregateGroup, DataQuery, Metric, QueryFilter, QueryResult, QuerySort,
};
pub use record_list::{ListRecordsOptions, ListRecordsPage, RecordFilter, RecordSort};
pub use record_share::RecordShareRole;
pub use schema::validate_table_schema_public;
pub use store::{DataStore, NewDataTable};
pub use transition::TRANSITION_EVENT_TYPE;
pub use validate::RefResolver;
pub use view::{DataViewBody, DataViewStore};

/// 集計スモールセル抑制の既定 K（PIT-17・design-caveats）。
///
/// K 未満のグループ/全体集計は値を返さず suppressed 通知に置き換える。反復差分攻撃の
/// 完全防御（差分プライバシー）は非目標で、集計クエリ自体の監査記録で検知可能性を残す。
pub const DEFAULT_AGGREGATE_MIN_ROWS: i64 = 5;

/// 構造化データ操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("対象が見つかりません")]
    NotFound,
    #[error("権限がありません")]
    Forbidden,
    #[error("不正な入力: {0}")]
    Invalid(String),
    /// 名前重複・unique 制約違反・楽観ロック（rev）不一致。
    #[error("競合しています: {0}")]
    Conflict(String),
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_db(e: sqlx::Error) -> DataError {
    DataError::Internal(format!("db: {e}"))
}
