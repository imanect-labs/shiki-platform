//! `tenant_id` の解決規則の単一定義（SAAS.1 / #808）。
//!
//! 内部 API（`crates/api` の `resolve_tenant_id`）と公開 API ゲートウェイ
//! （`crates/app-gateway` の二重ゲート）は**同じ規則**で `tenant_id` を決める。
//! 規則が二箇所に分かれると、片方だけ single モードで claim を採用する等の食い違いが
//! 生まれ、同じユーザーが入口によって別テナントへ解決される（越境の温床）。
//!
//! - [`TenantSource::Fixed`]（`auth.tenancy=single`・オンプレ/cell）: 設定の固定値を使い、
//!   claim `tenant` は**無視**する。固定値が空なら設定ミスとして拒否する。
//! - [`TenantSource::Claim`]（`auth.tenancy=multi`・SaaS）: claim `tenant` を**必須**にする。
//!   欠落・空白なら fail-closed で拒否し、固定値へフォールバックしない。
//!
//! どちらの経路でも、結果は [`validate_tenant_id`] の文字ポリシーを通す。

use crate::ident::{validate_tenant_id, IdentViolation};

/// `tenant_id` の取得元（テナンシーモードの写し）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantSource {
    /// single: 設定の固定値（claim は無視する）。
    Fixed(String),
    /// multi: claim `tenant` 必須（欠落は fail-closed）。
    Claim,
}

/// `tenant_id` を解決できなかった理由。呼び出し側が各層のエラー型（401 等）へ写す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantResolveError {
    /// multi モードで claim `tenant` が欠落・空白。
    MissingClaim,
    /// single モードで固定値が空（設定ミス）。
    EmptyFixed,
    /// 解決した値が識別子ポリシーに違反する。
    Invalid(IdentViolation),
}

impl std::fmt::Display for TenantResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TenantResolveError::MissingClaim => f.write_str("tenant クレームがありません"),
            TenantResolveError::EmptyFixed => f.write_str("固定の tenant_id が空です"),
            TenantResolveError::Invalid(v) => write!(f, "tenant_id が不正です: {v}"),
        }
    }
}

impl std::error::Error for TenantResolveError {}

impl TenantSource {
    /// claim `tenant` の値（あれば）から `tenant_id` を解決する。前後の空白は落とす。
    pub fn resolve(&self, claim: Option<&str>) -> Result<String, TenantResolveError> {
        let tenant_id = match self {
            TenantSource::Fixed(fixed) => {
                non_empty(Some(fixed)).ok_or(TenantResolveError::EmptyFixed)?
            }
            TenantSource::Claim => non_empty(claim).ok_or(TenantResolveError::MissingClaim)?,
        };
        validate_tenant_id(tenant_id).map_err(TenantResolveError::Invalid)?;
        Ok(tenant_id.to_string())
    }
}

/// trim して空でなければ返す。
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_ignores_claim() {
        // single では claim がどのテナントを名乗っても固定値に解決する。
        let src = TenantSource::Fixed("cell-onprem".into());
        assert_eq!(src.resolve(Some("other")).unwrap(), "cell-onprem");
        assert_eq!(src.resolve(None).unwrap(), "cell-onprem");
    }

    #[test]
    fn fixed_empty_is_rejected() {
        for blank in ["", "   "] {
            assert_eq!(
                TenantSource::Fixed(blank.into()).resolve(Some("acme")),
                Err(TenantResolveError::EmptyFixed),
                "空の固定値 {blank:?} は claim へフォールバックせず拒否されること"
            );
        }
    }

    #[test]
    fn claim_is_required() {
        let src = TenantSource::Claim;
        assert_eq!(src.resolve(Some(" acme ")).unwrap(), "acme");
        assert_eq!(src.resolve(None), Err(TenantResolveError::MissingClaim));
        assert_eq!(
            src.resolve(Some("  ")),
            Err(TenantResolveError::MissingClaim)
        );
    }

    #[test]
    fn both_sources_apply_ident_policy() {
        assert!(matches!(
            TenantSource::Claim.resolve(Some("ac|me")),
            Err(TenantResolveError::Invalid(_))
        ));
        assert!(matches!(
            TenantSource::Fixed("a/b".into()).resolve(None),
            Err(TenantResolveError::Invalid(_))
        ));
    }

    #[test]
    fn error_display_is_descriptive() {
        assert!(TenantResolveError::MissingClaim
            .to_string()
            .contains("tenant"));
        assert!(TenantResolveError::EmptyFixed.to_string().contains("空"));
        assert!(TenantResolveError::Invalid(IdentViolation::Whitespace)
            .to_string()
            .contains("空白"));
    }
}
