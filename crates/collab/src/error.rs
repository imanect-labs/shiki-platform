//! collab クレートのエラー型。

use thiserror::Error;

/// 共同編集サブシステムのエラー。
#[derive(Debug, Error)]
pub enum CollabError {
    /// 対象ノードに必要な relation（viewer/editor）が無い（fail-closed）。
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// 対象ノードが存在しない・ファイルでない。
    #[error("not found: {0}")]
    NotFound(String),
    /// DB 永続化の失敗。
    #[error("db error: {0}")]
    Db(#[from] sqlx::Error),
    /// Yjs update のデコード/適用失敗（敵対的入力は拒否して接続を切る）。
    #[error("invalid update: {0}")]
    InvalidUpdate(String),
    /// ストレージ（ノードメタ）参照の失敗。
    #[error("storage error: {0}")]
    Storage(#[from] storage::StorageError),
    /// 認可チェックの失敗（OpenFGA 到達不能等・fail-closed で拒否に倒す）。
    #[error("authz error: {0}")]
    Authz(#[from] authz::AuthzError),
}
