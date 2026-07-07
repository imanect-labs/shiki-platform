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
    #[error("対象が見つかりません")]
    NotFound,
    #[error("競合しています")]
    Conflict,
    #[error("不正なリクエスト: {0}")]
    BadRequest(String),
    /// 機能が無効/未準備（RAG 無効設定など）。理由はログのみ（クライアントへ漏らさない）。
    #[error("利用できません: {0}")]
    ServiceUnavailable(String),
    #[error("内部エラー: {0}")]
    Internal(String),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden => StatusCode::FORBIDDEN,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::Conflict => StatusCode::CONFLICT,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
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
        // 503 の原因（RAG worker/Qdrant 障害等）も追跡できるようログへ残す。
        if let ApiError::ServiceUnavailable(ref detail) = self {
            tracing::warn!(error = %detail, "サービス利用不可（503）");
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

impl From<storage::StorageError> for ApiError {
    fn from(err: storage::StorageError) -> Self {
        use storage::StorageError as SE;
        match err {
            SE::Forbidden => ApiError::Forbidden,
            SE::NotFound => ApiError::NotFound,
            SE::Conflict => ApiError::Conflict,
            // Invalid と整合性エラー（宣言ハッシュ不一致・staging 未完了等）はどちらも
            // クライアント起因の 400。
            SE::Invalid(msg) | SE::Integrity(msg) => ApiError::BadRequest(msg),
            SE::ObjectStore(e) => ApiError::Internal(format!("object_store: {e}")),
            SE::Db(e) => ApiError::Internal(format!("db: {e}")),
            SE::Authz(e) => ApiError::Internal(format!("authz: {e}")),
        }
    }
}

impl From<rag::RagError> for ApiError {
    fn from(err: rag::RagError) -> Self {
        use rag::RagError as RE;
        match err {
            // worker/Qdrant への到達失敗など一時障害は 503（クライアントは再試行できる）。
            RE::Http(_) | RE::Worker(_) | RE::Vector(_) => {
                ApiError::ServiceUnavailable(format!("rag: {err}"))
            }
            other => ApiError::Internal(format!("rag: {other}")),
        }
    }
}

impl From<chat::ChatError> for ApiError {
    fn from(err: chat::ChatError) -> Self {
        use chat::ChatError as CE;
        match err {
            CE::NotFound => ApiError::NotFound,
            CE::Forbidden => ApiError::Forbidden,
            CE::Invalid(msg) => ApiError::BadRequest(msg),
            CE::Unavailable(msg) => ApiError::ServiceUnavailable(format!("chat: {msg}")),
            CE::Internal(msg) => ApiError::Internal(format!("chat: {msg}")),
        }
    }
}

impl From<artifact::ArtifactError> for ApiError {
    fn from(err: artifact::ArtifactError) -> Self {
        use artifact::ArtifactError as AE;
        match err {
            AE::NotFound => ApiError::NotFound,
            AE::Forbidden => ApiError::Forbidden,
            AE::Invalid(msg) => ApiError::BadRequest(msg),
            AE::Conflict(_) => ApiError::Conflict,
            AE::Internal(msg) => ApiError::Internal(format!("artifact: {msg}")),
        }
    }
}

impl From<llm_gateway::LlmError> for ApiError {
    fn from(err: llm_gateway::LlmError) -> Self {
        use llm_gateway::LlmError as LE;
        match err {
            LE::Unavailable(msg) => ApiError::ServiceUnavailable(format!("llm: {msg}")),
            LE::BadRequest(msg) => ApiError::BadRequest(msg),
            LE::Config(msg) | LE::Internal(msg) => ApiError::Internal(format!("llm: {msg}")),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn status_maps_each_variant() {
        assert_eq!(ApiError::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(ApiError::Forbidden.status(), StatusCode::FORBIDDEN);
        assert_eq!(ApiError::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(ApiError::Conflict.status(), StatusCode::CONFLICT);
        assert_eq!(
            ApiError::BadRequest("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::Internal("boom".into()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn from_storage_error_maps_status() {
        use storage::StorageError as SE;
        assert!(matches!(ApiError::from(SE::Forbidden), ApiError::Forbidden));
        assert!(matches!(ApiError::from(SE::NotFound), ApiError::NotFound));
        assert!(matches!(ApiError::from(SE::Conflict), ApiError::Conflict));
        assert!(matches!(
            ApiError::from(SE::Invalid("bad".into())),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            ApiError::from(SE::Integrity("mismatch".into())),
            ApiError::BadRequest(_)
        ));
    }

    /// レスポンスボディを ProblemDetails 風 JSON として読み出すヘルパ。
    async fn body_json(err: ApiError) -> (StatusCode, serde_json::Value) {
        let resp = err.into_response();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, value)
    }

    #[tokio::test]
    async fn unauthorized_into_response() {
        let (status, body) = body_json(ApiError::Unauthorized).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["status"], 401);
        assert_eq!(body["title"], "Unauthorized");
    }

    #[tokio::test]
    async fn forbidden_into_response() {
        let (status, body) = body_json(ApiError::Forbidden).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["status"], 403);
        assert_eq!(body["title"], "Forbidden");
    }

    #[tokio::test]
    async fn internal_into_response_hides_detail() {
        // 内部エラーの詳細はボディに漏らさない（status/title のみ）。
        let (status, body) = body_json(ApiError::Internal("secret detail".into())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["status"], 500);
        assert_eq!(body["title"], "Internal Server Error");
        assert!(!body.to_string().contains("secret detail"));
    }

    #[test]
    fn from_authz_error_is_internal() {
        let api: ApiError = authz::AuthzError::InvalidModel("fga down".into()).into();
        assert!(matches!(api, ApiError::Internal(_)));
    }

    #[test]
    fn from_session_error_is_internal() {
        let api: ApiError = crate::session::SessionError::Backend("redis down".into()).into();
        assert!(matches!(api, ApiError::Internal(_)));
    }

    #[test]
    fn from_oidc_4xx_is_unauthorized() {
        // token エンドポイント 4xx（invalid_grant 等）は認証失敗にマップする。
        let err = crate::oidc::OidcError::Status {
            status: 400,
            body: "invalid_grant".into(),
        };
        assert!(matches!(ApiError::from(err), ApiError::Unauthorized));
    }

    #[test]
    fn from_oidc_5xx_is_internal() {
        // 5xx は一過性障害として内部エラー扱い。
        let err = crate::oidc::OidcError::Status {
            status: 503,
            body: "down".into(),
        };
        assert!(matches!(ApiError::from(err), ApiError::Internal(_)));
    }

    #[test]
    fn from_oidc_transport_is_internal() {
        // 到達失敗（transport）も内部エラー扱い。
        let err = crate::oidc::OidcError::Transport("connection refused".into());
        assert!(matches!(ApiError::from(err), ApiError::Internal(_)));
    }

    #[test]
    fn error_display_is_safe_messages() {
        // Display 文言（ログ/内部用）が想定どおりであること。
        assert_eq!(ApiError::Unauthorized.to_string(), "認証が必要です");
        assert_eq!(ApiError::Forbidden.to_string(), "権限がありません");
        assert_eq!(ApiError::Internal("x".into()).to_string(), "内部エラー: x");
    }
}
