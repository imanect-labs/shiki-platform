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
//! - **RAG 統合スパイク（Task 15.8）**: テーブル単位のオプトイン（`TableSchema.rag`）でレコード 1 件 =
//!   1 チャンク。post-filter は候補 id を可視集合に絞るクエリ 1 本（[`DataStore::rag_visible_records`]・
//!   OpenFGA のタプルを増やさない）。投影にマスク対象列は入れられず、`field_policy` の変更は索引の世代を変えて
//!   破棄・再構築させる。増分索引は outbox の別 consumer（[`RagIndexWorker`]・docs/data/rag-integration.md）

mod bulk;
mod cursor;
mod derived;
mod field_key;
mod formula;
mod fsm;
mod fsm_rules;
mod fsm_store;
mod index;
mod job_lease;
mod key_migration;
mod link_merge;
mod link_values;
mod mask;
mod model;
mod numeric;
pub mod policy;
mod rag;
// クエリ層の件数・コストモデル（µs の概算）は非負・有界の数値を f64 / u64 へ写す（DB の件数は 0 以上）。
mod claim;
#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
mod query;
mod record;
mod record_effects;
mod record_list;
mod record_share;
mod record_update;
mod ref_id;
mod revision;
mod schema;
mod schema_cache;
mod schema_data_check;
mod sql;
mod stats;
mod store;
mod stream;
mod table_list;
mod table_load;
mod table_redact;
mod tenant_settings;
mod transition;
mod validate;
mod view;
mod view_spec;
mod view_spec_de;
mod view_spec_redact;
mod viewer_context;

/// クエリ条件の 2 値論理の共通テスト表（JSON・query-optimizer.md §3.1）。
///
/// data の評価器と workflow の評価器（`control/eval.rs`）の両方のテストがこの 1 つの表を読み、
/// 同じ結果を返すことを固定する（Task 15.7）。実行時には使わない。
#[doc(hidden)]
pub const CONDITION_SEMANTICS_TABLE: &str = include_str!("query/semantics_table.json");

pub use bulk::{
    BulkImportOptions, BulkImportRow, BulkImportStatus, BulkImportStep, BulkImportWorker,
    BulkIndexMode, BulkRowError, BulkUpsertRow, BULK_CHUNK, BULK_IMPORT_QUEUE, LAZY_INDEX_MIN_ROWS,
    MAX_BULK_CSV_BYTES, MAX_BULK_ROWS, MAX_BULK_ROW_BYTES, MAX_BULK_ROW_KEY_BYTES, ROW_KEY_COLUMN,
};
pub use cursor::{CursorKey, CURSOR_KEY_LEN};
pub use derived::links::{LINK_TRAVERSAL_SQL, MAX_LINK_VALUES_PER_RECORD};
pub use derived::types::{
    LinkCardinality, LinkDef, Materialization, ResultType, RollupDef, RollupOp,
};
pub use formula::{FormulaCmpOp, FormulaExpr, SwitchCase, MAX_FORMULA_DEPTH};
pub use fsm::{FsmBody, FsmRef, FsmTransition};
pub use fsm_rules::AllowedTransitions;
pub use fsm_store::FsmStore;
#[doc(hidden)]
pub use index::text_index_delete_sql;
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
    HavingCond, JoinSpec, LinkCond, Metric, MetricSpec, QueryResult, SortKey, DATA_QUERY_KEYS,
};
pub use query::limits::{
    QueryLimits, MAX_AST_DEPTH, MAX_AST_NODES, MAX_COMPILE_MS, MAX_COMPILE_WORK, MAX_JOIN_DEPTH,
    MAX_LITERALS, MAX_SQL_BYTES,
};
#[doc(hidden)]
pub use query::optimizer_ops::extract_table_shape;
pub use query::optimizer_ops::{
    OptimizerCandidateKind, OptimizerIndexCandidate, OptimizerStatus, TableOptimizerReport,
};
pub use query::planner::cost::{CalibrationReport, UnitCosts};
pub use query::planner::exec::ForcedStrategy;
pub use query::planner::feedback::{ExecSample, QueryExecStats, TableScanMetrics};
pub use query::planner::plan_cache::PlanCacheStats;
pub use query::planner::shape::{ColumnShape, LinkShape, ShapeType, TableShape};
pub use query::reject::{QueryRejection, RejectCode, RejectLimit, SqlLocation};
pub use query::run::{
    DebugParam, DebugStatement, QueryDebug, QueryTuning, ReadPath, DEFAULT_COUNT_CAP,
    DEFAULT_READ_TIMEOUT_MS,
};
pub use rag::{
    RagIndexWorker, RagProjection, RagRecordHit, RagVisibleRecords, RecordChunk, RecordChunkSink,
    MAX_RAG_BODY_FIELDS, MAX_RAG_CHUNK_CHARS, MAX_RAG_POST_FILTER_IDS, RAG_INDEX_BATCH,
    RAG_INDEX_CONSUMER,
};
pub use record_list::{ListRecordsOptions, ListRecordsPage, RecordFilter, RecordSort};
pub use record_share::RecordShareRole;
pub use schema::validate_table_schema_public;
pub use sql::render::{render_sql, RENDER_PSEUDO};
pub use sql::{CompiledSql, SqlCell, SqlCellItem, SqlQueryResult, SqlRequest};
pub use stats::analyze::AnalyzeOutcome;
pub use stats::{StatsWorker, STATS_ANALYZE_QUEUE, STATS_CONSUMER, VALUE_COUNT_MAX_DISTINCT};
pub use store::{DataStore, NewDataTable};
pub use stream::{
    AwarenessEvent, RecordRemovedEvent, RecordStream, RecordStreamRelayWorker, RecordUpsertedEvent,
    RecordsBulkAppliedEvent, SchemaChangedEvent, StreamConfig, StreamEvent, StreamKeepaliveEvent,
    StreamMessage, StreamReadyEvent, StreamResetEvent, StreamResetReason, LEDGER_CONSUMER,
    RECORD_CHANGED_EVENT,
};
pub use tenant_settings::{DataTenantSettings, MAX_COUNT_CAP, MAX_INDEXED_FIELDS_CEILING};
pub use transition::TRANSITION_EVENT_TYPE;
pub use validate::RefResolver;
pub use view::{
    effective_sort, DataViewBody, DataViewPage, DataViewRun, DataViewStore, DataViewSummary,
};
pub use view_spec::{
    CalendarView, ColumnConfig, ConditionalFormat, FormatColor, GalleryView, GridView, GroupConfig,
    KanbanView, RowHeight, ViewSpec, DEFAULT_MAX_VIEW_LOOKUP_COLUMNS,
    MAX_VIEW_LOOKUP_COLUMNS_CEILING,
};
pub use viewer_context::TableViewerContext;

/// [`DataError::FieldUnavailable`] の文言（存在しない列と読めない列で同一・列名を含めない）。
pub const FIELD_UNAVAILABLE_MESSAGE: &str = "指定した列は存在しないか、参照できません";

/// 集計スモールセル抑制の既定 K（PIT-17・design-caveats）。
///
/// K 未満のグループ/全体集計は値を返さず suppressed 通知に置き換える。反復差分攻撃の
/// 完全防御（差分プライバシー）は非目標で、集計クエリ自体の監査記録で検知可能性を残す。
pub const DEFAULT_AGGREGATE_MIN_ROWS: i64 = 5;

/// 入力の誤り（400）の構造化コード（data-platform §5.6・検証 C-P2-4）。
///
/// HTTP・ゲートウェイ・ワークフロー・チャットのエラー形が同じ語彙を使う（`snake_case`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvalidCode {
    /// カーソルが不正・期限切れ・鍵の入替えで読めない（先頭から読み直す）。
    CursorInvalid,
    /// v1 の `offset`（0 以外）は使えない（カーソルで続きを読む）。
    OffsetNotSupported,
    /// 索引の無い列で絞り込み・並べ替えようとした。
    FieldNotIndexed,
    /// 並べ替え・絞り込み・集計に使えない列（閲覧者ごとの派生列・対称列・多値など）。
    FieldNotSortable,
    /// 比較値・入力値の型が列の型と合わない。
    ValueTypeMismatch,
    /// 値が範囲外（数値の min / max・小数桁・日付の範囲）。
    ValueOutOfRange,
    /// 値が長すぎる（文字数の上限）。
    ValueTooLong,
    /// 必須の列に値が無い。
    RequiredMissing,
    /// 選択肢に無い値・重複した選択。
    InvalidOption,
    /// 値の数が多すぎる（多値・リンク・条件の数）。
    TooManyValues,
    /// スキーマ・ビュー・ポリシーの定義が不正（定義を書いた本人へ理由を返す）。
    SchemaInvalid,
    /// 入力の形の誤り（NUL 文字・未知のキー・`limit` の範囲・形の合わない要求など）。
    InvalidInput,
    /// スナップショット読取で使えない指定（並べ替え・集計・結合など）。
    SnapshotUnsupported,
    /// 状態遷移（FSM）の定義・遷移の誤り。
    FsmInvalid,
    /// スキーマの改訂が既存のデータと矛盾する（値の無い行がある列を必須にする・使われている選択肢を外す）。
    SchemaConflictsWithData,
}

impl InvalidCode {
    /// コードの文字列（`snake_case`）。
    pub fn as_str(self) -> &'static str {
        match self {
            InvalidCode::CursorInvalid => "cursor_invalid",
            InvalidCode::OffsetNotSupported => "offset_not_supported",
            InvalidCode::FieldNotIndexed => "field_not_indexed",
            InvalidCode::FieldNotSortable => "field_not_sortable",
            InvalidCode::ValueTypeMismatch => "value_type_mismatch",
            InvalidCode::ValueOutOfRange => "value_out_of_range",
            InvalidCode::ValueTooLong => "value_too_long",
            InvalidCode::RequiredMissing => "required_missing",
            InvalidCode::InvalidOption => "invalid_option",
            InvalidCode::TooManyValues => "too_many_values",
            InvalidCode::SchemaInvalid => "schema_invalid",
            InvalidCode::InvalidInput => "invalid_input",
            InvalidCode::SnapshotUnsupported => "snapshot_unsupported",
            InvalidCode::FsmInvalid => "fsm_invalid",
            InvalidCode::SchemaConflictsWithData => "schema_conflicts_with_data",
        }
    }

    /// 既定の文言（値・列名・内部 ID を含まない）。
    pub fn default_message(self) -> &'static str {
        match self {
            InvalidCode::CursorInvalid => {
                "続きを読む位置（カーソル）が無効です。最初から読み直してください"
            }
            InvalidCode::OffsetNotSupported => {
                "offset は使えません。next_cursor で続きを読んでください"
            }
            InvalidCode::FieldNotIndexed => {
                "この列では絞り込み・並べ替えできません（索引がありません）"
            }
            InvalidCode::FieldNotSortable => "この列は並べ替え・絞り込み・集計に使えません",
            InvalidCode::ValueTypeMismatch => "値の型が列の型と合いません",
            InvalidCode::ValueOutOfRange => "値が範囲外です",
            InvalidCode::ValueTooLong => "値が長すぎます",
            InvalidCode::RequiredMissing => "必須の列に値がありません",
            InvalidCode::InvalidOption => "選択肢にない値です",
            InvalidCode::TooManyValues => "値の数が多すぎます",
            InvalidCode::SchemaInvalid => "定義が正しくありません",
            InvalidCode::InvalidInput => "入力が正しくありません",
            InvalidCode::SnapshotUnsupported => "スナップショット読取では使えない指定です",
            InvalidCode::FsmInvalid => "状態遷移の定義または遷移が正しくありません",
            InvalidCode::SchemaConflictsWithData => {
                "既存のデータと矛盾するためスキーマを変更できません"
            }
        }
    }
}

/// 入力の誤り（400）の構造化した理由。
///
/// **文言には値・閲覧者が読めない列名・内部 ID・`record_int`・SQL 断片を入れられない設計にする**:
/// 文言を作る入口は [`InputError::new`]（コードの既定文言）・[`InputError::field`]（＋列の参照名）・
/// [`InputError::detail`] だけ。`detail` は**要求の書き手が自分で書いた名前**（クエリ・スキーマ・FSM の定義の
/// 参照名）と宣言済みの上限だけから作る（値を埋め込まない）。列の参照名は、書き手が要求で名指しした列か
/// 書き手が読める列に限る（書込の検証は名指ししていない列の名前を外す・[`InputError::scrub_field`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError {
    code: InvalidCode,
    field: Option<String>,
    message: std::borrow::Cow<'static, str>,
}

impl InputError {
    /// コードの既定文言で作る。
    pub fn new(code: InvalidCode) -> Self {
        InputError {
            code,
            field: None,
            message: std::borrow::Cow::Borrowed(code.default_message()),
        }
    }

    /// コードの既定文言＋列の参照名（UI が列の表示名を添えて出す）。
    pub fn field(code: InvalidCode, key: &str) -> Self {
        InputError {
            field: Some(key.to_string()),
            ..InputError::new(code)
        }
    }

    /// 理由の文言つき（要求の書き手が自分で書いた名前と宣言済みの上限だけから作ること・型の注記を参照）。
    pub(crate) fn detail(code: InvalidCode, key: Option<&str>, message: impl Into<String>) -> Self {
        InputError {
            code,
            field: key.map(str::to_string),
            message: std::borrow::Cow::Owned(message.into()),
        }
    }

    pub fn code(&self) -> InvalidCode {
        self.code
    }

    /// 対象の列の参照名（あれば）。
    pub fn field_key(&self) -> Option<&str> {
        self.field.as_deref()
    }

    /// 利用者に見せてよい文言。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 文言の中の列の参照名（`'key'`）を表示名（`「表示名」`）に置き換える（利用者向けの文言・検証 2-A P3）。
    /// 対象の列（`field`）が `key` のときだけ。表示名は列の参照名と同じく、書き手が名指しした・読める列の
    /// もの（読めない列の名前は [`InputError::scrub_field`] が文言ごと既定に戻す）。
    pub(crate) fn relabel(mut self, key: &str, display: &str) -> Self {
        if self.field.as_deref() == Some(key) {
            let label = if display.trim().is_empty() {
                key
            } else {
                display
            };
            let m = self
                .message
                .replace(&format!("'{key}'"), &format!("「{label}」"));
            self.message = std::borrow::Cow::Owned(m);
        }
        self
    }

    /// 列名を外して既定文言に戻す（書き手が名指ししていない・読めない列の名前を返さない）。
    pub(crate) fn scrub_field(self, allowed: impl Fn(&str) -> bool) -> Self {
        match &self.field {
            Some(k) if !allowed(k) => InputError::new(self.code),
            _ => self,
        }
    }
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// 構造化データ操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("対象が見つかりません")]
    NotFound,
    #[error("権限がありません")]
    Forbidden,
    /// 存在しない列・閲覧者に読めない（マスクされた）列の指定（403・`code: field_unavailable`）。
    ///
    /// **両者を同じ応答にする**（同じ status・code・文言・列名を含めない）。区別すると REST・取込・チャット・
    /// ワークフロー・スクリプトが列の存在オラクルになる（data-platform §5.2・§16.1・PIT-74 ⑤）。
    #[error("{}", FIELD_UNAVAILABLE_MESSAGE)]
    FieldUnavailable,
    /// 入力の誤り（理由は**ログにだけ**残し、利用者へは汎用の文言 `invalid_input` を返す）。
    /// 利用者へ理由を返す誤りは [`DataError::InvalidInput`]（構造化・安全な文言）。
    #[error("不正な入力: {0}")]
    Invalid(String),
    /// 入力の誤り（400・構造化したコードと利用者に見せてよい文言・[`InputError`]）。
    #[error("不正な入力: {0}")]
    InvalidInput(Box<InputError>),
    /// テーブル名の重複（409・`code: name_taken`）。
    #[error("競合しています: 同じ名前のテーブルが既にあります")]
    NameTaken,
    /// 読取が statement timeout で打ち切られた（503・`code: read_timeout`・再試行可）。
    #[error("読取が時間内に終わりませんでした")]
    ReadTimeout,
    /// 名前重複・取込 ID の再利用・並行のスキーマ切替など（409・`code: conflict`）。
    /// rev 不一致・unique 違反・再計算中はクライアントが出し分けられるよう個別の変種にする。
    #[error("競合しています: {0}")]
    Conflict(String),
    /// 楽観ロック（rev）の不一致（409・`code: rev_conflict`）。`current` は今の rev
    /// （行述語を通った＝閲覧できる行だけで起きる。不可視行は 404）。
    #[error("競合しています: rev が一致しません（現在 {current}）")]
    RevConflict { current: i64 },
    /// unique 宣言・一対一 / 一対多のリンクの参照先の一意の違反（409・`code: unique_violation`）。
    /// 重複した値・相手の行は返さない。`field` は違反した列（書き手が読める列のときだけ・
    /// [`scrub_input_field`]）。
    #[error("競合しています: unique 制約に違反しています")]
    UniqueViolation { field: Option<ConflictField> },
    /// 材料化した派生列が参照先の変更を受けて再計算中（409・`code: derived_recomputing`・再試行可）。
    /// 完了までその列で絞り込み・並べ替えできない（data-platform §7.4）。
    #[error(
        "フィールド '{field}' は参照先の変更を受けて再計算中です（完了までフィルタ/ソートに使えません）"
    )]
    DerivedRecomputing { field: String },
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

/// 409 `unique_violation` を起こした列（参照名と表示名・値は持たない）。
///
/// 書き手が読めない列は名指ししない（[`scrub_input_field`] が外す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictField {
    /// 列の参照名。
    pub key: String,
    /// 列の表示名（文言に使う）。
    pub display_name: String,
    /// 一対一・一対多のリンクの参照先の一意の違反か（偽なら unique 宣言の違反）。
    pub link: bool,
}

impl ConflictField {
    pub(crate) fn of(f: &model::FieldDef, link: bool) -> Self {
        let display_name = if f.display_name.trim().is_empty() {
            f.key.clone()
        } else {
            f.display_name.clone()
        };
        ConflictField {
            key: f.key.clone(),
            display_name,
            link,
        }
    }

    /// 利用者に見せる文言（列の表示名つき・値と相手の行は含めない）。
    pub fn message(&self) -> String {
        let name = &self.display_name;
        if self.link {
            format!("「{name}」のリンク先は既に他のレコードから参照されています（一対一・一対多のリンク）")
        } else {
            format!("「{name}」の値は既に他のレコードで使われています（重複できない列）")
        }
    }
}

impl DataError {
    /// 構造化した入力の誤り（400）を作る。
    pub fn input(code: InvalidCode) -> Self {
        DataError::InvalidInput(Box::new(InputError::new(code)))
    }

    /// 構造化した入力の誤り（400・列の参照名つき）を作る。
    pub fn input_field(code: InvalidCode, key: &str) -> Self {
        DataError::InvalidInput(Box::new(InputError::field(code, key)))
    }

    /// 構造化した入力の誤り（理由の文言つき・[`InputError::detail`] の制約に従うこと）。
    pub(crate) fn input_detail(
        code: InvalidCode,
        key: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        DataError::InvalidInput(Box::new(InputError::detail(code, key, message)))
    }

    /// 構造化した入力の誤り（固定の文言つき・列名なし）。crate 外の入口（HTTP の見出しの検査など）用。
    pub fn input_detail_public(code: InvalidCode, message: &'static str) -> Self {
        DataError::input_detail(code, None, message)
    }

    /// クエリの最上位の未知のキー（400 `invalid_input`・キーは要求の書き手が書いたもの）。
    pub fn unknown_query_key(key: &str) -> Self {
        DataError::input_detail(
            InvalidCode::InvalidInput,
            Some(key),
            format!("クエリのキー '{key}' は使えません（綴りを確かめてください）"),
        )
    }

    /// 入力の誤り（400・構造化の有無を問わない）か。
    pub fn is_invalid(&self) -> bool {
        matches!(self, DataError::Invalid(_) | DataError::InvalidInput(_))
    }

    /// 入力の誤りの文言（行のエラー文言・ツールの応答用。構造化した誤りは安全な文言）。
    pub fn invalid_message(&self) -> Option<String> {
        match self {
            DataError::Invalid(m) => Some(m.clone()),
            DataError::InvalidInput(e) => Some(e.message().to_string()),
            _ => None,
        }
    }

    /// 入力の誤りのコード（構造化していない誤りは `invalid_input`）。
    pub fn invalid_code(&self) -> Option<InvalidCode> {
        match self {
            DataError::Invalid(_) => Some(InvalidCode::InvalidInput),
            DataError::InvalidInput(e) => Some(e.code()),
            _ => None,
        }
    }

    /// 409 の競合（汎用・rev 不一致・unique 違反・再計算中・名前の重複）か。
    pub fn is_conflict(&self) -> bool {
        matches!(
            self,
            DataError::Conflict(_)
                | DataError::RevConflict { .. }
                | DataError::UniqueViolation { .. }
                | DataError::DerivedRecomputing { .. }
                | DataError::NameTaken
        )
    }

    /// 競合の理由の文言（「競合しています」の前置きなし・行のエラー文言やログ用）。競合でなければ `None`。
    pub fn conflict_detail(&self) -> Option<String> {
        match self {
            DataError::Conflict(m) => Some(m.clone()),
            DataError::RevConflict { current } => {
                Some(format!("rev が一致しません（現在 {current}）"))
            }
            DataError::UniqueViolation { field: Some(f) } => Some(f.message()),
            DataError::UniqueViolation { field: None } => {
                Some("unique 制約に違反しています".into())
            }
            DataError::DerivedRecomputing { .. } => Some(self.to_string()),
            DataError::NameTaken => Some("同じ名前のテーブルが既にあります".into()),
            _ => None,
        }
    }

    /// 409 の構造化コード（`conflict` / `rev_conflict` / `unique_violation` / `derived_recomputing`）。
    /// 競合でなければ `None`。HTTP・ゲートウェイ・ワークフローのエラー形が同じ語彙を使う。
    pub fn conflict_code(&self) -> Option<&'static str> {
        Some(match self {
            DataError::Conflict(_) => "conflict",
            DataError::RevConflict { .. } => "rev_conflict",
            DataError::UniqueViolation { .. } => "unique_violation",
            DataError::DerivedRecomputing { .. } => "derived_recomputing",
            DataError::NameTaken => "name_taken",
            _ => return None,
        })
    }
}

/// スキーマ・ビュー・ポリシーの定義の誤り（定義を書いた本人へ返す文言・[`InputError::detail`] の制約に従う）。
pub(crate) fn schema_invalid(message: impl Into<String>) -> DataError {
    DataError::input_detail(InvalidCode::SchemaInvalid, None, message)
}

/// 書込の検証の誤りから、書き手が名指ししていない・読めない列の名前を外す（[`InputError::scrub_field`]）。
pub(crate) fn scrub_input_field(e: DataError, allowed: impl Fn(&str) -> bool) -> DataError {
    match e {
        DataError::InvalidInput(ie) => DataError::InvalidInput(Box::new(ie.scrub_field(allowed))),
        DataError::UniqueViolation { field: Some(f) } if !allowed(&f.key) => {
            DataError::UniqueViolation { field: None }
        }
        other => other,
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_db(e: sqlx::Error) -> DataError {
    // statement timeout（57014 query_canceled）は再試行できる 503 にする（検証 C-P2-6・D-3）。
    // 理由（SQL・値）はログにだけ残す。
    if let sqlx::Error::Database(db) = &e {
        if db.code().as_deref() == Some("57014") {
            tracing::warn!(error = %e, "statement timeout で打ち切りました");
            return DataError::ReadTimeout;
        }
    }
    DataError::Internal(format!("db: {e}"))
}

#[cfg(test)]
mod conflict_field_tests {
    use super::*;

    fn violation(key: &str) -> DataError {
        DataError::UniqueViolation {
            field: Some(ConflictField {
                key: key.into(),
                display_name: format!("{key} の表示名"),
                link: false,
            }),
        }
    }

    #[test]
    fn unreadable_conflict_fields_are_not_named() {
        // 読めない列は名指ししない（列なしの 409 にする）。
        let e = scrub_input_field(violation("secret"), |k| k != "secret");
        assert!(
            matches!(e, DataError::UniqueViolation { field: None }),
            "{e:?}"
        );
        assert_eq!(
            e.conflict_detail().as_deref(),
            Some("unique 制約に違反しています")
        );
        // 読める列はそのまま（表示名つきの文言）。
        let e = scrub_input_field(violation("code"), |k| k != "secret");
        assert!(
            e.conflict_detail().unwrap().contains("code の表示名"),
            "{e:?}"
        );
        assert_eq!(e.conflict_code(), Some("unique_violation"));
    }

    #[test]
    fn conflict_field_falls_back_to_the_key_without_a_display_name() {
        let mut f = model::FieldDef::new("code", model::FieldType::Text);
        f.display_name = " ".into();
        let c = ConflictField::of(&f, true);
        assert_eq!(c.display_name, "code");
        assert!(
            c.message().contains("「code」のリンク先"),
            "{}",
            c.message()
        );
    }
}
