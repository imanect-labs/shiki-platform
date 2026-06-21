//! API エラー型と HTTP レスポンスへの変換（ProblemDetails 風）。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("認証が必要です")]
    Unauthorized,
    #[error("権限がありません")]
    Forbidden,
    #[error("内部エラー: {0}")]
    Internal(String),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden => StatusCode::FORBIDDEN,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        // 内部エラーは詳細をクライアントに漏らさず、ログにのみ残す。
        if let ApiError::Internal(ref detail) = self {
            tracing::error!(error = %detail, "内部エラー");
        }
        let body = Json(json!({
            "status": status.as_u16(),
            "title": status.canonical_reason().unwrap_or("Error"),
        }));
        (status, body).into_response()
    }
}

impl From<authz::AuthzError> for ApiError {
    fn from(err: authz::AuthzError) -> Self {
        ApiError::Internal(format!("authz: {err}"))
    }
}

impl From<crate::session::SessionError> for ApiError {
    fn from(err: crate::session::SessionError) -> Self {
        ApiError::Internal(format!("session: {err}"))
    }
}

impl From<crate::oidc::OidcError> for ApiError {
    fn from(err: crate::oidc::OidcError) -> Self {
        match err {
            // token エンドポイントの 4xx（invalid_grant / 失効 refresh 等）は認証失敗扱い。
            crate::oidc::OidcError::Status { status, .. } if (400..500).contains(&status) => {
                tracing::debug!(%status, "OIDC token エンドポイントが 4xx（認証失敗）");
                ApiError::Unauthorized
            }
            other => ApiError::Internal(format!("oidc: {other}")),
        }
    }
}
