//! `ExecCtx` → 実行主体 `AuthContext` の変換（`ports.rs` から分割・500 行規約）。

use authz::{AuthContext, Principal, PrincipalKind};
use workflow_engine::ExecCtx;

/// `ExecCtx` から実行主体の `AuthContext` を組む（種別で subject を分ける）。
pub(super) fn auth_ctx(ec: &ExecCtx) -> AuthContext {
    if ec.principal_kind == "workflow" {
        AuthContext::for_workflow(ec.tenant_id.clone(), ec.org.clone(), &ec.principal)
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
            ec.tenant_id.clone(),
        )
    }
}
