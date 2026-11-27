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
        GuiError::Artifact(artifact::ArtifactError::NotPublished { published }) => {
            app_version_conflict(published)
        }
        GuiError::Artifact(e) => ApiError::from(e),
        GuiError::Internal(m) => ApiError::Internal(m),
    }
}

/// ミニアプリの、使う人に出していない版を開こうとした・押した（D-16）。まだ公開していなければ `not_published`、
/// 開いた後に公開した版が変わったなら `app_version_changed`（画面は開き直す）。`published_version` は今の公開した版。
pub(crate) fn app_version_conflict(published: Option<i64>) -> ApiError {
    let (code, message) = match published {
        None => (
            "not_published",
            "このアプリはまだ公開されていません。作成した人が公開すると使えます。",
        ),
        Some(_) => (
            "app_version_changed",
            "公開しているバージョンが変わりました。画面を開き直してください。",
        ),
    };
    ApiError::ConflictJson(serde_json::json!({
        "status": 409,
        "title": "Conflict",
        "code": code,
        "message": message,
        "published_version": published,
    }))
}
