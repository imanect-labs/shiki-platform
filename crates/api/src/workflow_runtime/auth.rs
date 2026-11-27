//! `ExecCtx` → 実行主体 `AuthContext` の変換（`ports.rs` から分割・行数規約）。

use authz::{AuthContext, Principal, PrincipalKind, TenantId};
use workflow_engine::{ExecCtx, PortError};

/// `ExecCtx` から実行主体の `AuthContext` を組む（種別で subject を分ける）。
///
/// run の tenant_id も文字ポリシーを通す（違反は恒久エラー・再試行しない）。
///
/// ノードの実行の文脈には run の印（`origin_run`）を付ける。書込はそれを outbox の非公開部に載せ、relay が
/// 因果の鎖と `by_workflow` をそこから引く（書込の trace_id から run を推しはからない・BUG-44）。
pub(super) fn auth_ctx(ec: &ExecCtx) -> Result<AuthContext, PortError> {
    Ok(principal_ctx(&ec.tenant_id, &ec.org, &ec.principal, &ec.principal_kind)?.in_run(ec.run_id))
}

/// run の実行主体（`workflow_run.principal` / `principal_kind`）の `AuthContext` を組む。
///
/// ノードの実行（[`auth_ctx`]）と、wait(event) を起こす前の閲覧の確認（relay）が同じ組み方を使う。
/// tenant_id が文字ポリシーに反するときは恒久エラー（fail-closed・#1032）。
pub(super) fn principal_ctx(
    tenant_id: &str,
    org: &str,
    principal: &str,
    principal_kind: &str,
) -> Result<AuthContext, PortError> {
    let tenant = parse_tenant(tenant_id)?;
    Ok(if principal_kind == "workflow" {
        AuthContext::for_workflow(tenant, org.to_string(), principal)
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
            tenant,
        )
    })
}

/// run の tenant_id を検証済みの [`TenantId`] にする。
pub(super) fn tenant_of(ec: &ExecCtx) -> Result<TenantId, PortError> {
    parse_tenant(&ec.tenant_id)
}

fn parse_tenant(tenant_id: &str) -> Result<TenantId, PortError> {
    TenantId::parse(tenant_id)
        .map_err(|e| PortError::forbidden(format!("run の tenant_id が不正です: {e}")))
}
