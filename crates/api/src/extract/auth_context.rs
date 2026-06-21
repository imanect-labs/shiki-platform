//! `AuthContext` extractor（docs/design.md §4.1, architecture-invariants）。
//!
//! 認証主体から org を解決して [`AuthContext`] を組み立てる。データアクセスを行う
//! ハンドラはこの extractor 経由でしか `AuthContext` を得られないため、
//! 「AuthContext を経由しないデータアクセス」を構造的に書きにくくする継ぎ目になる。

use authz::{AuthContext, Principal};
use axum::{extract::FromRequestParts, http::request::Parts};

use crate::{
    config::{AuthConfig, Tenancy},
    error::ApiError,
};

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
/// テナンシーモードで戦略を分岐する（human 決定: 案C 既定 ＋ 案A 継ぎ目,
/// docs/roadmap/phase-0.md Task 0.5）:
/// - `single`（案C・オンプレ/cell）: 設定 `auth.tenant_id` の固定値を使う。defaults で
///   `"default"` が効くため無設定でも後方互換（シングルテナント既定で無変更動作）。
///   固定値が空なら設定ミスとして拒否。
/// - `multi`（案A・SaaS）: claim `tenant` 由来の [`Principal::tenant_id`] を**必須**にする。
///   欠落・空白なら **fail-closed で拒否**（固定値へフォールバックして無関係なテナントへ
///   黙って融合させない）。
///
/// いずれのモードでも解決不能なら `tenant_id` 無しの `AuthContext` を構築させず拒否する。
pub(crate) fn resolve_tenant_id(
    principal: &Principal,
    auth: &AuthConfig,
) -> Result<String, ApiError> {
    match auth.tenancy {
        Tenancy::Multi => non_empty(principal.tenant_id.as_deref()).ok_or_else(|| {
            tracing::warn!("multi-tenant モードで claim `tenant` が欠落（fail-closed で拒否）");
            ApiError::Unauthorized
        }),
        Tenancy::Single => non_empty(auth.tenant_id.as_deref()).ok_or_else(|| {
            tracing::error!("single-tenant モードで auth.tenant_id が空（設定ミス）");
            ApiError::Unauthorized
        }),
    }
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

    fn auth_config(tenancy: Tenancy, tenant_id: Option<&str>) -> AuthConfig {
        AuthConfig {
            issuer: "http://localhost/realms/shiki".into(),
            internal_base_url: None,
            jwks_uri: None,
            audience: "shiki-api".into(),
            jwks_ttl_secs: 300,
            client_id: "shiki-web".into(),
            client_secret: None,
            redirect_uri: "http://localhost:3000/auth/callback".into(),
            post_logout_redirect_uri: "http://localhost:3000/".into(),
            scopes: "openid profile".into(),
            tenancy,
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
    fn single_tenant_uses_fixed_config() {
        // 案C: オンプレ/cell は設定の固定値を使う（claim は無視）。
        let mut principal = principal_with_groups(&[]);
        principal.tenant_id = Some("ignored-claim".into());
        let tenant = resolve_tenant_id(
            &principal,
            &auth_config(Tenancy::Single, Some("cell-onprem")),
        )
        .unwrap();
        assert_eq!(tenant, "cell-onprem");
    }

    #[test]
    fn single_tenant_empty_config_is_rejected() {
        // 固定値が空（設定ミス）なら拒否＝tenant_id 無しの AuthContext を作らせない。
        let principal = principal_with_groups(&[]);
        assert!(resolve_tenant_id(&principal, &auth_config(Tenancy::Single, None)).is_err());
        assert!(resolve_tenant_id(&principal, &auth_config(Tenancy::Single, Some("  "))).is_err());
    }

    #[test]
    fn multi_tenant_requires_claim() {
        // 案A: SaaS は claim 由来の tenant_id を使う（設定固定値は無視）。
        let mut principal = principal_with_groups(&[]);
        principal.tenant_id = Some("acme-saas".into());
        let tenant =
            resolve_tenant_id(&principal, &auth_config(Tenancy::Multi, Some("default"))).unwrap();
        assert_eq!(tenant, "acme-saas");
    }

    #[test]
    fn multi_tenant_missing_claim_fails_closed() {
        // SaaS で claim 欠落・空白なら fail-closed で拒否（固定値へ融合させない）。
        let principal = principal_with_groups(&[]);
        assert!(
            resolve_tenant_id(&principal, &auth_config(Tenancy::Multi, Some("default"))).is_err()
        );
        let mut blank = principal_with_groups(&[]);
        blank.tenant_id = Some("   ".into());
        assert!(resolve_tenant_id(&blank, &auth_config(Tenancy::Multi, Some("default"))).is_err());
    }
}
