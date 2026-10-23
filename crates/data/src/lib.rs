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
//! - **リンク・lookup・ロールアップ・数式（Task 15.4）**: 参照先テーブルの 4 階層 authz を
//!   **閲覧者本人の権限で**透過適用して読取時に計算する（PIT-20）。書込時の材料化は全閲覧者で同値と
//!   証明できた列（`Materialization::Invariant`）だけで、前提が崩れたら同一 Tx で破棄する（PIT-71）。
//!   数式は閉じた AST ＋ Rust インタプリタ（`numeric` 精度）。[`derived`]・[`formula`]
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
//! - **認可材料のリクエストスコープ memo（Task 15.9）**: 同じリクエストの OpenFGA の問いを覚えて
//!   投げ直さず、読取の入口でテーブル・参照先・名指しロールの check を BatchCheck 1 往復に束ねる。
//!   メモは `DataStore` の借用で作るのでリクエストの外へ持ち出せない（PIT-18）。`data_table.schema` は
//!   `(tenant_id, table_id, schema_version)` のプロセス内キャッシュ（認可材料は載せない）。
//! - **一括書込パス（Task 15.11）**: ステージング表へ COPY → 1 件の書込と同じ検証器 → チャンク Tx で
//!   1 件の create と同じ入口（[`record_effects`]）から本体・索引・revision・材料化を書く。jobq のチャンク
//!   ジョブで冪等・中断再開可。大きな取込は値索引を `building` に保持して遅延構築する（[`bulk`]）
//! - **SQL 互換インターフェース（Task 15.10）**: SQL を前段言語として受け、閉じた部分集合（allowlist）だけを
//!   `DataQuery` IR へ変換する。**呼び出し側の SQL テキストは 1 文字もデータベースへ届かない**（[`sql`]）。
//!   JOIN は宣言済みリンク列の等値だけ・参照先の行述語を必ず適用し、証明できる結合は半結合へ書き換える。
//!   同一形状のクエリはバイト同一の SQL テキストになり、プランキャッシュに認可材料を載せない。
//! - **クエリコンパイラ v2（Task 15.3）**: 条件木・複数ソート・keyset・自前オプティマイザ O1
//!   （上限付き件数探索・4 戦略・予算付き切替・`Guard(P)`・事後検査）。[`query`]

mod bulk;
mod cursor;
mod derived;
mod field_key;
mod formula;
mod fsm;
mod fsm_store;
mod index;
mod key_migration;
mod link_values;
mod mask;
mod model;
mod numeric;
pub mod policy;
// クエリ層の件数・コストモデル（µs の概算）は非負・有界の数値を f64 / u64 へ写す（DB の件数は 0 以上）。
#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
mod query;
mod record;
mod record_effects;
mod record_list;
mod record_share;
mod revision;
mod schema;
mod schema_cache;
mod sql;
mod stats;
mod store;
mod stream;
mod table_list;
mod table_load;
mod tenant_settings;
mod transition;
mod validate;
mod view;

pub use bulk::{
    BulkImportOptions, BulkImportRow, BulkImportStatus, BulkImportStep, BulkImportWorker,
    BulkIndexMode, BulkRowError, BULK_CHUNK, BULK_IMPORT_QUEUE, LAZY_INDEX_MIN_ROWS, MAX_BULK_ROWS,
    MAX_BULK_ROW_BYTES, MAX_BULK_ROW_KEY_BYTES,
};
pub use cursor::{CursorKey, CURSOR_KEY_LEN};
pub use derived::links::{LINK_TRAVERSAL_SQL, MAX_LINK_VALUES_PER_RECORD};
pub use derived::types::{
    LinkCardinality, LinkDef, Materialization, ResultType, RollupDef, RollupOp,
};
pub use formula::{FormulaCmpOp, FormulaExpr, SwitchCase, MAX_FORMULA_DEPTH};
pub use fsm::{FsmBody, FsmRef, FsmTransition};
pub use fsm_store::FsmStore;
pub use index::{
    sort_precision, FieldIndexStatus, IndexBackfillStatus, IndexBackfillStep, IndexBackfillWorker,
    IndexParityMismatch, IndexParityReport, IndexPromotion, IndexState, IndexStatus, SortPrecision,
    TableIndexOverview, BACKFILL_CHUNK, DEFAULT_MAX_INDEXED_FIELDS, DEFAULT_MAX_MULTI_VALUES,
    INDEX_BACKFILL_QUEUE, MAX_INDEX_KEY_BYTES, SYNC_INDEX_MAX_ROWS,
};
pub use key_migration::{
    FieldKeyMigrationWorker, KeyMigrationProgress, FIELD_KEY_MIGRATION_QUEUE, MIGRATION_BATCH,
};
pub use link_values::{LinkValueMigrationStatus, LinkValueStep, LINK_VALUE_CHUNK};
pub use model::{
    ComputedDef, ComputedOp, DataRecord, DataTable, ExcessScale, FieldDef, FieldId, FieldPatch,
    FieldPolicy, FieldType, LookupDef, NumericSpec, RecordRevision, RoundingMode, TableSchema,
    FIRST_USER_FIELD_ID, MAX_FIELD_ID, SYSTEM_FIELD_CREATED_AT, SYSTEM_FIELD_OWNER,
    SYSTEM_FIELD_UPDATED_AT,
};
pub use policy::{CmpOp, PolicyExpr, PolicyOperand, RowPolicy};
pub use query::admission::{QuerySuggestion, DEFAULT_READ_BUDGET_MS, DEFAULT_TENANT_READ_TOKENS};
pub use query::ir::{
    AggregateGroup, CondCmp, CondOp, Condition, CountResult, DataQuery, GroupSortKey, HavingCmp,
    HavingCond, JoinSpec, LinkCond, Metric, MetricSpec, QueryResult, SortKey,
};
pub use query::limits::{
    QueryLimits, MAX_AST_DEPTH, MAX_AST_NODES, MAX_COMPILE_MS, MAX_COMPILE_WORK, MAX_JOIN_DEPTH,
    MAX_LITERALS, MAX_SQL_BYTES,
};
pub use query::optimizer_ops::{
    OptimizerCandidateKind, OptimizerIndexCandidate, TableOptimizerReport,
};
pub use query::planner::cost::{CalibrationReport, UnitCosts};
pub use query::planner::exec::ForcedStrategy;
pub use query::planner::feedback::{QueryExecStats, TableScanMetrics};
pub use query::planner::plan_cache::PlanCacheStats;
pub use query::planner::shape::{ColumnShape, LinkShape, ShapeType, TableShape};
pub use query::reject::{QueryRejection, RejectCode, RejectLimit, SqlLocation};
pub use query::run::{
    DebugParam, DebugStatement, QueryDebug, QueryTuning, ReadPath, DEFAULT_COUNT_CAP,
    DEFAULT_READ_TIMEOUT_MS,
};
pub use record_list::{ListRecordsOptions, ListRecordsPage, RecordFilter, RecordSort};
pub use record_share::RecordShareRole;
pub use schema::validate_table_schema_public;
pub use sql::render::{render_sql, RENDER_PSEUDO};
pub use sql::{CompiledSql, SqlQueryResult, SqlRequest};
pub use stats::analyze::AnalyzeOutcome;
pub use stats::{StatsWorker, STATS_ANALYZE_QUEUE, STATS_CONSUMER, VALUE_COUNT_MAX_DISTINCT};
pub use store::{DataStore, NewDataTable};
pub use stream::{
    AwarenessEvent, RecordRemovedEvent, RecordStream, RecordStreamRelayWorker, RecordUpsertedEvent,
    SchemaChangedEvent, StreamConfig, StreamEvent, StreamMessage, StreamReadyEvent,
    StreamResetEvent, StreamResetReason, LEDGER_CONSUMER, RECORD_CHANGED_EVENT,
};
pub use tenant_settings::{DataTenantSettings, MAX_COUNT_CAP, MAX_INDEXED_FIELDS_CEILING};
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
    /// スナップショット読取の基準位置が保持期間を過ぎた（410・最初から取り直す・data-platform §3.3）。
    #[error("保持期間を過ぎました: {0}")]
    Gone(String),
    /// 列の索引を準備中（バックフィル中）。完了までその列で絞り込み・並べ替えできない（409・§10）。
    /// `progress` は 0〜1（不明なら `None`）。
    #[error("フィールド '{field}' の索引は準備中です")]
    IndexPreparing {
        field: String,
        progress: Option<f64>,
    },
    /// 列の索引を自動で作るとテーブルの索引列数の上限を超える（409・明示の承認が要る・PIT-72）。
    #[error(
        "フィールド '{field}' の索引を作るには承認が要ります（索引列 {indexed} / 上限 {cap}）"
    )]
    IndexApprovalRequired {
        field: String,
        indexed: usize,
        cap: usize,
    },
    /// 予測コストが読取予算を超えるので実行しなかった（受付制御・query-optimizer.md §5.4）。
    /// 提案は語彙だけで、**推定値・予測コスト・戦略名を含めない**（PIT-75）。
    #[error("クエリが重すぎるため実行しませんでした（絞り込みを足してください）")]
    QueryTooExpensive { suggestions: Vec<QuerySuggestion> },
    /// テナントの読取の同時実行の枠が空かなかった（data-platform §16.8・再試行可）。
    #[error("読取が混み合っています。しばらくしてから再試行してください")]
    ReadOverloaded,
    /// クエリの入口で拒否した（422・構造化）。SQL 前段面の部分集合外・名前解決不能・資源上限
    /// （data-platform §16.1）。存在しない列/テーブルと見えない列/テーブルは同一の内容になる。
    #[error("クエリを受け付けられません: {0}")]
    Rejected(Box<QueryRejection>),
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_db(e: sqlx::Error) -> DataError {
    DataError::Internal(format!("db: {e}"))
}
