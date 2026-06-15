//! `AuthContext` extractor（docs/design.md §4.1, architecture-invariants）。
//!
//! 認証主体から org を解決して [`AuthContext`] を組み立てる。データアクセスを行う
//! ハンドラはこの extractor 経由でしか `AuthContext` を得られないため、
//! 「AuthContext を経由しないデータアクセス」を構造的に書きにくくする継ぎ目になる。

use authz::{AuthContext, Principal};
use axum::{extract::FromRequestParts, http::request::Parts};

use crate::error::ApiError;

impl<S> FromRequestParts<S> for AuthContextExt
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let principal = parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or(ApiError::Unauthorized)?;
        let org = resolve_org(&principal);
        Ok(AuthContextExt(AuthContext::new(principal, org)))
    }
}

/// `AuthContext` を newtype で包んだ extractor。
pub struct AuthContextExt(pub AuthContext);

/// 認証主体から所属組織 ID を解決する。
///
/// Phase 0 はシングルテナント想定で、Keycloak group（例: `/acme` や `/acme/eng`）の
/// 先頭セグメントを org とみなす。group が無い場合は `default`。
/// 後続フェーズで専用 claim や DB ルックアップに差し替える。
fn resolve_org(principal: &Principal) -> String {
    principal
        .groups
        .iter()
        .filter_map(|g| g.trim_start_matches('/').split('/').next())
        .find(|seg| !seg.is_empty())
        .unwrap_or("default")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal_with_groups(groups: &[&str]) -> Principal {
        Principal {
            id: "u1".into(),
            email: None,
            groups: groups.iter().map(|s| s.to_string()).collect(),
            dept: None,
        }
    }

    #[test]
    fn org_from_group_path() {
        assert_eq!(resolve_org(&principal_with_groups(&["/acme/eng"])), "acme");
        assert_eq!(resolve_org(&principal_with_groups(&["acme"])), "acme");
        assert_eq!(resolve_org(&principal_with_groups(&[])), "default");
    }
}
