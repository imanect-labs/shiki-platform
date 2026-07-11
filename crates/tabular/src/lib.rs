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
//! **DuckDB（重い C++）は `runner` feature でのみリンクされる**。api（lib 利用者）は default
//! features で使い DuckDB をリンクしない＝敵対的 CSV を api プロセスに一切食わせない。

pub mod error;
pub mod patch;
pub mod protocol;
pub mod runner;
pub mod service;
pub mod sql_guard;

pub use error::TabularError;
pub use patch::{apply_patches, PatchOp, PatchResult};
pub use protocol::{RunnerOp, RunnerRequest, RunnerResponse};
pub use runner::RunnerConfig;
pub use service::{PatchApplied, Quotas, SavedCsv, TabularService};
pub use sql_guard::validate_read_only;
