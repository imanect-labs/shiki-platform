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
