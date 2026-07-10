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

mod index;
mod model;
mod record;
mod record_list;
mod revision;
mod schema;
mod store;
mod validate;

pub use model::{
    ComputedDef, ComputedOp, DataRecord, DataTable, FieldDef, FieldPatch, FieldType, LookupDef,
    RecordRevision, TableSchema,
};
pub use record_list::{ListRecordsOptions, ListRecordsPage, RecordFilter, RecordSort};
pub use store::{DataStore, NewDataTable};
pub use validate::RefResolver;

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
