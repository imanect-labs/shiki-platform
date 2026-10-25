//! `ExecCtx` → 実行主体 `AuthContext` の変換（`ports.rs` から分割・行数規約）。

use authz::{AuthContext, Principal, PrincipalKind};
use workflow_engine::ExecCtx;

/// `ExecCtx` から実行主体の `AuthContext` を組む（種別で subject を分ける）。
pub(super) fn auth_ctx(ec: &ExecCtx) -> AuthContext {
    principal_ctx(&ec.tenant_id, &ec.org, &ec.principal, &ec.principal_kind)
}

/// run の実行主体（`workflow_run.principal` / `principal_kind`）の `AuthContext` を組む。
///
/// ノードの実行（[`auth_ctx`]）と、wait(event) を起こす前の閲覧の確認（relay）が同じ組み方を使う。
pub(super) fn principal_ctx(
    tenant_id: &str,
    org: &str,
    principal: &str,
    principal_kind: &str,
) -> AuthContext {
    if principal_kind == "workflow" {
        AuthContext::for_workflow(tenant_id.to_string(), org.to_string(), principal)
    } else {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: principal.to_string(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(tenant_id.to_string()),
            },
            org.to_string(),
            tenant_id.to_string(),
        )
    }
}
