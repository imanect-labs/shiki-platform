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
//! - **フィールド識別子 3 層（Task 15.1）**: 外部契約は不変の参照名 `key` で受けて返し、
//!   保存形（JSONB）は不変の内部 ID 由来の `"f{id}"` に閉じる（[`field_key`]）。表示名
//!   （`display_name`・日本語可）の変更はスキーマ 1 行の書き換えで完結する。
//! - **内部整数キーは外へ出さない（Task 15.1・PIT-75）**: `tenant_int` / `table_int` /
//!   `record_int` は DTO・監査・エラー文言に載せない。keyset カーソルは AEAD で暗号化する
//!   （[`cursor`]）。
//! - **接続プールは `PoolRouter` から引く（Task 15.1）**: 本 crate は接続プールを保持しない
//!   （シャード安全の禁止事項は docs/data/shard-safety.md・CI で検査）。
//! - **pivot 索引テーブル（Task 15.2）**: 索引は共有の索引テーブルへの行として持ち、物理本数は
//!   テナント数・テーブル数・列数に依存しない。書込は同一 Tx で変更フィールドの索引行だけを
//!   書き換える（[`index`]）。旧方式の式インデックスは Task 15.3 で撤去し、ランタイム DDL はゼロ。
//! - **クエリコンパイラ v2（Task 15.3）**: 条件木・複数ソート・keyset・自前オプティマイザ O1
//!   （上限付き件数探索・4 戦略・予算付き切替・`Guard(P)`・事後検査）。[`query`]

mod cursor;
mod derived;
mod field_key;
mod fsm;
mod fsm_store;
mod index;
mod key_migration;
mod mask;
mod model;
mod numeric;
pub mod policy;
// クエリ層の件数・コストモデル（µs の概算）は非負・有界の数値を f64 / u64 へ写す（DB の件数は 0 以上）。
#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
mod query;
mod record;
mod record_list;
mod record_share;
mod revision;
mod schema;
mod store;
mod table_list;
mod tenant_settings;
mod transition;
mod validate;
mod view;

pub use cursor::{CursorKey, CURSOR_KEY_LEN};
pub use fsm::{FsmBody, FsmRef, FsmTransition};
pub use fsm_store::FsmStore;
pub use index::{
    sort_precision, IndexBackfillStatus, IndexBackfillStep, IndexBackfillWorker,
    IndexParityMismatch, IndexParityReport, IndexState, SortPrecision, BACKFILL_CHUNK,
    DEFAULT_MAX_MULTI_VALUES, INDEX_BACKFILL_QUEUE, MAX_INDEX_KEY_BYTES,
};
pub use key_migration::{
    FieldKeyMigrationWorker, KeyMigrationProgress, FIELD_KEY_MIGRATION_QUEUE, MIGRATION_BATCH,
};
pub use model::{
    ComputedDef, ComputedOp, DataRecord, DataTable, ExcessScale, FieldDef, FieldId, FieldPatch,
    FieldPolicy, FieldType, LookupDef, NumericSpec, RecordRevision, RoundingMode, TableSchema,
    FIRST_USER_FIELD_ID, MAX_FIELD_ID, SYSTEM_FIELD_CREATED_AT, SYSTEM_FIELD_OWNER,
    SYSTEM_FIELD_UPDATED_AT,
};
pub use policy::{CmpOp, PolicyExpr, PolicyOperand, RowPolicy};
pub use query::ir::{
    AggregateGroup, CondCmp, CondOp, Condition, CountResult, DataQuery, Metric, MetricSpec,
    QueryResult, SortKey,
};
pub use query::planner::exec::ForcedStrategy;
pub use query::planner::feedback::{QueryExecStats, TableScanMetrics};
pub use query::run::{
    DebugParam, DebugStatement, QueryDebug, QueryTuning, ReadPath, DEFAULT_COUNT_CAP,
    DEFAULT_READ_TIMEOUT_MS,
};
pub use record_list::{ListRecordsOptions, ListRecordsPage, RecordFilter, RecordSort};
pub use record_share::RecordShareRole;
pub use schema::validate_table_schema_public;
pub use store::{DataStore, NewDataTable};
pub use tenant_settings::{DataTenantSettings, MAX_COUNT_CAP};
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
