//! `Principal` extractor。認証ミドルウェアが extension に載せた値を取り出す。

use authz::Principal;
use axum::{extract::FromRequestParts, http::request::Parts};

use crate::error::ApiError;

pub struct AuthPrincipal(pub Principal);

impl<S> FromRequestParts<S> for AuthPrincipal
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .cloned()
            .map(AuthPrincipal)
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

    fn sample_principal() -> Principal {
        Principal {
            kind: authz::PrincipalKind::User,
            id: "user-1".into(),
            email: Some("u@example.com".into()),
            groups: vec!["/acme".into()],
            roles: vec![],
            tenant_id: Some("acme".into()),
        }
    }

    #[tokio::test]
    async fn extracts_principal_from_extension() {
        // 認証ミドルウェアが載せた Principal を取り出せること。
        let mut parts = empty_parts();
        parts.extensions.insert(sample_principal());
        let extracted = AuthPrincipal::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(extracted.0.id, "user-1");
        assert_eq!(extracted.0.tenant_id.as_deref(), Some("acme"));
    }

    #[tokio::test]
    async fn missing_principal_is_unauthorized() {
        // extension が無い（未認証）なら 401 相当の拒否を返す。
        let mut parts = empty_parts();
        let result = AuthPrincipal::from_request_parts(&mut parts, &()).await;
        assert!(matches!(result, Err(ApiError::Unauthorized)));
    }
}
