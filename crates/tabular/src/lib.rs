//! CSV クエリ/パッチサービス（Phase 11-pre Task 11P.7・design §4.8.2）。
//!
//! CSV は StorageService 上のファイルが真実（authz はファイル単位 ReBAC・data_table には
//! 乗せない）。本クレートを CSV クエリ/パッチの**単一チョークポイント**とし、DuckDB 実行は
//! **非特権別プロセス**（`shiki-tabular-runner` バイナリ）に隔離する（PIT-39）。
//!
//! # セキュリティ不変条件（PIT-39）
//! - SQL は**読み取り専用**（[`sql_guard`]・単一 SELECT/WITH のみ・DDL/DML/ATTACH/PRAGMA/
//!   LOAD/INSTALL/COPY を拒否）。
//! - 外部アクセスは**ランナー側で無効化**（`enable_external_access=false`・`lock_configuration=
//!   true`・extension autoload/autoinstall 無効）。入力はサービスが渡した検証済みパスのみ。
//! - メモリ/時間/結果サイズの**クォータをプロセス境界で強制**（超過はプロセスごと kill）。
//! - 失敗は常に fail-closed。
//!
//! **DuckDB（重い C++）は隔離ランナーだけがリンクする**。api（lib 利用者）は DuckDB をリンクしない＝敵対的 CSV を
//! api プロセスに一切食わせない。ランナーとの JSON プロトコルの型・Excel の入力の上限・SQL の検証は、ランナーと
//! 共有する小さなクレート（`shiki-tabular-protocol`）にあり、ここで再エクスポートする。ランナーはこの lib ではなく
//! そのクレートだけに依存する（ストレージ・認可などをランナーに組まない・GAP-95）。

pub mod error;
pub mod input;
pub mod patch;
pub use tabular_protocol::protocol;
pub mod runner;
pub mod service;
pub mod sql_guard;
pub mod tables;

pub use error::TabularError;
pub use patch::{apply_patches, apply_patches_with, PatchOp, PatchResult};
pub use protocol::{
    CsvDialect, InputFormat, RunnerOp, RunnerRequest, RunnerResponse, RunnerTable, SheetInfo,
};
pub use runner::RunnerConfig;
pub use service::{PatchApplied, Quotas, SavedCsv, TabularService};
pub use sql_guard::validate_read_only;
pub use tables::{
    is_valid_table_name, validate_tables, TableRef, MAX_EXTRA_TABLES, PRIMARY_TABLE,
    TABLE_NAME_PATTERN,
};
