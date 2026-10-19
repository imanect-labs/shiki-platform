//! gui（skill・ミニアプリ）の保存/取得のエラー。

use artifact::ArtifactError;

use crate::validate::GuiValidationError;

/// skill・ミニアプリの保存/取得のエラー。
#[derive(Debug, thiserror::Error)]
pub enum GuiError {
    /// 検証エラー（全件）。
    #[error("検証に失敗しました（{} 件）", .0.len())]
    Validation(Vec<GuiValidationError>),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    /// 内部エラー（DB 等・利用者に詳細を見せない）。
    #[error("内部エラー: {0}")]
    Internal(String),
}

impl GuiError {
    /// 検証エラーの一覧（あれば）。
    pub fn validation_errors(&self) -> Option<&[GuiValidationError]> {
        match self {
            GuiError::Validation(v) => Some(v),
            GuiError::Artifact(_) | GuiError::Internal(_) => None,
        }
    }
}
