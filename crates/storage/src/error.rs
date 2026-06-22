//! StorageService のエラー型。API 層で HTTP ステータスへマップする。

use crate::object_store::ObjectStoreError;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// 認可 check に失敗（403）。
    #[error("権限がありません")]
    Forbidden,
    /// 対象ノード/アップロードが存在しない（404）。
    #[error("対象が見つかりません")]
    NotFound,
    /// 同一フォルダ内の名前衝突など（409）。
    #[error("名前が競合しています")]
    Conflict,
    /// 入力が不正（400）。
    #[error("不正な引数: {0}")]
    Invalid(String),
    /// content-addressing の整合性検証に失敗（宣言ハッシュ/サイズ不一致等）。
    #[error("整合性チェックに失敗: {0}")]
    Integrity(String),
    #[error("オブジェクトストア: {0}")]
    ObjectStore(#[from] ObjectStoreError),
    #[error("データベース: {0}")]
    Db(sqlx::Error),
    #[error("認可: {0}")]
    Authz(#[from] authz::AuthzError),
}

impl From<sqlx::Error> for StorageError {
    fn from(err: sqlx::Error) -> Self {
        // 一意制約違反は名前衝突（同一フォルダ内の重複名）として 409 に倒す。
        if let sqlx::Error::Database(ref db_err) = err {
            if db_err.is_unique_violation() {
                return StorageError::Conflict;
            }
        }
        StorageError::Db(err)
    }
}
