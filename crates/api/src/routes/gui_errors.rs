//! gui 層（skill・ミニアプリ）のエラーを HTTP へ写す（検証エラーは構造化して返す）。

use gui::{GuiError, GuiValidationError};
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::ApiError;

/// 検証エラーレスポンス（全件・LLM/エディタが位置つきで表示）。
#[derive(Debug, Serialize, ToSchema)]
pub struct GuiValidationErrorResponse {
    pub errors: Vec<GuiValidationError>,
}

/// gui 層のエラーを HTTP へ写す（検証エラーは構造化 422）。
pub(crate) fn map_gui_err(err: GuiError) -> ApiError {
    match err {
        GuiError::Validation(errors) => {
            let payload = serde_json::to_value(GuiValidationErrorResponse { errors })
                .unwrap_or_else(|_| serde_json::json!({ "errors": [] }));
            ApiError::UnprocessableJson(payload)
        }
        GuiError::Artifact(e) => ApiError::from(e),
        GuiError::Internal(m) => ApiError::Internal(m),
    }
}
