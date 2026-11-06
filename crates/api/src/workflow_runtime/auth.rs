//! `ExecCtx` → 実行主体 `AuthContext` の変換（`ports.rs` から分割・行数規約）。

use authz::{AuthContext, Principal, PrincipalKind, TenantId};
use workflow_engine::{ExecCtx, PortError};

/// `ExecCtx` から実行主体の `AuthContext` を組む（種別で subject を分ける）。
///
/// run の tenant_id も文字ポリシーを通す（違反は恒久エラー・再試行しない）。
pub(super) fn auth_ctx(ec: &ExecCtx) -> Result<AuthContext, PortError> {
    let tenant = tenant_of(ec)?;
    Ok(if ec.principal_kind == "workflow" {
        AuthContext::for_workflow(tenant, ec.org.clone(), &ec.principal)
    } else {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: ec.principal.clone(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(ec.tenant_id.clone()),
            },
            ec.org.clone(),
            tenant,
        )
    })
}

/// run の tenant_id を検証済みの [`TenantId`] にする。
pub(super) fn tenant_of(ec: &ExecCtx) -> Result<TenantId, PortError> {
    TenantId::parse(ec.tenant_id.as_str())
        .map_err(|e| PortError::forbidden(format!("run の tenant_id が不正です: {e}")))
}
