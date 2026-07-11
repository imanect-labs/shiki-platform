//! tabular クレートのエラー型。

use thiserror::Error;

/// CSV クエリ/パッチのエラー。
#[derive(Debug, Error)]
pub enum TabularError {
    /// ファイルへの必要 relation（viewer/editor/作成権限）が無い（fail-closed）。
    #[error("forbidden")]
    Forbidden,
    /// 対象ファイルが存在しない・CSV でない。
    #[error("not found: {0}")]
    NotFound(String),
    /// SQL が読み取り専用制約に違反（DDL/DML/ATTACH/PRAGMA/複数文/外部参照等）。
    #[error("read-only 制約違反: {0}")]
    SqlRejected(String),
    /// クォータ超過（メモリ/時間/結果サイズ）で隔離プロセスが打ち切られた。
    #[error("クォータ超過: {0}")]
    QuotaExceeded(String),
    /// 楽観ロック失敗（base rev が現在の版と不一致）。リロードが必要。
    #[error("競合: base_rev={base}, current={current}")]
    RevConflict { base: i64, current: i64 },
    /// パッチ入力が不正（範囲外の行/列・型不一致等）。
    #[error("不正なパッチ: {0}")]
    InvalidPatch(String),
    /// 隔離ランナーの実行失敗（デコード/プロセス異常）。
    #[error("runner error: {0}")]
    Runner(String),
    /// ユーザー SQL が DuckDB 実行で失敗（未知の列・型不一致・構文以外の意味エラー等）。
    /// 構文/RO 検証は通ったがクエリ自体が誤り＝**利用者起因**なので 400 で理由を返す
    /// （プロセス異常＝500 の `Runner` とは区別する）。
    #[error("query failed: {0}")]
    QueryFailed(String),
    /// ストレージ（CSV 取得/保存）の失敗。
    #[error("storage error: {0}")]
    Storage(#[from] storage::StorageError),
    /// 認可チェックの失敗（fail-closed）。
    #[error("authz error: {0}")]
    Authz(#[from] authz::AuthzError),
    /// I/O・内部エラー。
    #[error("internal: {0}")]
    Internal(String),
}
