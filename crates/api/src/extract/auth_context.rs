//! `AuthContext` extractor（docs/design.md §4.1, architecture-invariants）。
//!
//! 認証主体から org を解決して [`AuthContext`] を組み立てる。データアクセスを行う
//! ハンドラはこの extractor 経由でしか `AuthContext` を得られないため、
//! 「AuthContext を経由しないデータアクセス」を構造的に書きにくくする継ぎ目になる。

use authz::{AuthContext, Principal};
use axum::{extract::FromRequestParts, http::request::Parts};

use crate::{config::AuthConfig, error::ApiError};

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
        // tenant_id は state を持つ認証 middleware（`require_auth`）が解決して extension に
        // 載せる。extractor は state 非依存（`FromRequestParts<S>`）を保つためここでは読むだけ。
        let tenant_id = parts
            .extensions
            .get::<TenantId>()
            .map(|t| t.0.clone())
            .ok_or(ApiError::Unauthorized)?;
        let org = resolve_org(&principal);
        Ok(AuthContextExt(AuthContext::new(principal, org, tenant_id)))
    }
}

/// `AuthContext` を newtype で包んだ extractor。
pub struct AuthContextExt(pub AuthContext);

/// 解決済み `tenant_id` を request extension で運ぶ newtype。
///
/// 認証 middleware が [`resolve_tenant_id`] の結果を載せ、extractor が取り出す。
/// `tenant_id` を撒く継ぎ目を `AuthContext` 構築の一点に集約するための運搬器。
#[derive(Debug, Clone)]
pub(crate) struct TenantId(pub String);

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

/// 認証主体と設定から `tenant_id` を解決する（取得元の継ぎ目を一点に集約）。
///
/// 解決順（human 決定: 案C 既定 ＋ 案A 継ぎ目, docs/roadmap/phase-0.md Task 0.5）:
/// 1. 案A — claim `tenant` 由来の [`Principal::tenant_id`]（SaaS マルチテナント）。
/// 2. 案C — 設定 `auth.tenant_id` の固定値（オンプレ/cell シングルテナント。既定 `"default"`）。
/// 3. いずれも無ければ拒否（`tenant_id` 無しで `AuthContext` を構築させない）。
///
/// オンプレは defaults で `auth.tenant_id = "default"` のため無設定でも 2 で解決し、
/// 後方互換（シングルテナント既定で無変更動作）を保つ。
pub(crate) fn resolve_tenant_id(
    principal: &Principal,
    auth: &AuthConfig,
) -> Result<String, ApiError> {
    if let Some(tenant) = non_empty(principal.tenant_id.as_deref()) {
        return Ok(tenant);
    }
    if let Some(tenant) = non_empty(auth.tenant_id.as_deref()) {
        return Ok(tenant);
    }
    tracing::debug!("tenant_id を解決できません（claim・設定とも欠落）");
    Err(ApiError::Unauthorized)
}

/// trim して空でなければ所有 `String` を返す。
fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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
            tenant_id: None,
        }
    }

    fn auth_config(tenant_id: Option<&str>) -> AuthConfig {
        AuthConfig {
            issuer: "http://localhost/realms/shiki".into(),
            jwks_uri: None,
            audience: "shiki-api".into(),
            jwks_ttl_secs: 300,
            tenant_id: tenant_id.map(str::to_string),
        }
    }

    #[test]
    fn org_from_group_path() {
        assert_eq!(resolve_org(&principal_with_groups(&["/acme/eng"])), "acme");
        assert_eq!(resolve_org(&principal_with_groups(&["acme"])), "acme");
        assert_eq!(resolve_org(&principal_with_groups(&[])), "default");
    }

    #[test]
    fn tenant_id_prefers_claim() {
        // 案A: claim 由来の tenant_id があれば設定固定値より優先。
        let mut principal = principal_with_groups(&[]);
        principal.tenant_id = Some("acme-saas".into());
        let tenant = resolve_tenant_id(&principal, &auth_config(Some("default"))).unwrap();
        assert_eq!(tenant, "acme-saas");
    }

    #[test]
    fn tenant_id_falls_back_to_config() {
        // 案C: claim が無ければ設定の固定値（オンプレ/cell）。
        let principal = principal_with_groups(&[]);
        let tenant = resolve_tenant_id(&principal, &auth_config(Some("cell-onprem"))).unwrap();
        assert_eq!(tenant, "cell-onprem");
    }

    #[test]
    fn tenant_id_missing_is_rejected() {
        // claim・設定とも欠落（または空）なら拒否＝tenant_id 無しの AuthContext を作らせない。
        let principal = principal_with_groups(&[]);
        assert!(resolve_tenant_id(&principal, &auth_config(None)).is_err());
        assert!(resolve_tenant_id(&principal, &auth_config(Some("  "))).is_err());
    }
}
