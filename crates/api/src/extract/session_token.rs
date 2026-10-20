//! BFF セッションが保持する IdP の access token を取り出す extractor。
//!
//! ユーザー本人の権限で IdP の API（Keycloak Account API）を呼ぶハンドラだけが使う。
//! トークンはサーバ内で使うだけで、レスポンスやログへは出さない（`Debug` も伏せる）。

use axum::{extract::FromRequestParts, http::request::Parts};

use crate::error::ApiError;

/// セッションミドルウェアが request extension に載せる access token。
#[derive(Clone)]
pub struct SessionAccessToken(pub String);

impl std::fmt::Debug for SessionAccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionAccessToken(***)")
    }
}

impl<S> FromRequestParts<S> for SessionAccessToken
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<SessionAccessToken>()
            .cloned()
            .ok_or(ApiError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn empty_parts() -> Parts {
        Request::builder().body(()).unwrap().into_parts().0
    }

    #[tokio::test]
    async fn extracts_token_from_extension() {
        let mut parts = empty_parts();
        parts.extensions.insert(SessionAccessToken("tok".into()));
        let got = SessionAccessToken::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(got.0, "tok");
    }

    #[tokio::test]
    async fn missing_token_is_unauthorized() {
        let mut parts = empty_parts();
        let result = SessionAccessToken::from_request_parts(&mut parts, &()).await;
        assert!(matches!(result, Err(ApiError::Unauthorized)));
    }

    #[test]
    fn debug_hides_token() {
        assert_eq!(
            format!("{:?}", SessionAccessToken("secret".into())),
            "SessionAccessToken(***)"
        );
    }
}
