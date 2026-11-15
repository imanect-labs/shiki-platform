//! 管理操作の単一経路。永続キュー・Keycloak定義・OpenFGA階層の再試行可能な同期。
use crate::{error::ApiError, keycloak_admin::KeycloakAdmin, AppState};
pub mod connection;
pub mod dn;
pub mod import;
pub mod plan;
pub mod preview;

use authz::{AuthContext, Consistency, Principal, PrincipalKind, Relation, TenantId};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use storage::directory::admin::{DirectoryChangeSet, DirectoryOperation};
use uuid::Uuid;

pub async fn submit(
    state: &AppState,
    ctx: &AuthContext,
    request: &DirectoryChangeSet,
    trace: Option<&str>,
) -> Result<DirectoryOperation, ApiError> {
    KeycloakAdmin::from_config(&state.http, &state.config.auth)
        .map_err(|_| ApiError::ServiceUnavailable("Keycloak管理接続が未設定です".into()))?;
    let store = state.directory.admin();
    let hash = storage::directory::admin::request_hash(request)?;
    if let Some(operation) = store
        .existing_request(ctx, request.idempotency_key, &hash)
        .await?
    {
        return Ok(operation);
    }
    let _lease = store.lease(ctx).await?.ok_or(ApiError::Conflict)?;
    if let Some(operation) = store
        .existing_request(ctx, request.idempotency_key, &hash)
        .await?
    {
        return Ok(operation);
    }
    let prepared = store
        .validate(ctx, request)
        .await
        .map_err(|error| public_error(error.into()))?;
    Ok(store.enqueue(ctx, &prepared, &hash, trace).await?)
}

/// 空の変更セットは内部的にAD同期を表す。通常applyはvalidateで空入力を拒否する。
pub async fn submit_sync(
    state: &AppState,
    ctx: &AuthContext,
    key: Uuid,
    trace: Option<&str>,
) -> Result<DirectoryOperation, ApiError> {
    KeycloakAdmin::from_config(&state.http, &state.config.auth)
        .map_err(connection::keycloak_error)?;
    let request = DirectoryChangeSet {
        idempotency_key: key,
        roles: vec![],
        mappings: vec![],
    };
    let hash = storage::directory::admin::request_hash(&request)?;
    let store = state.directory.admin();
    if let Some(operation) = store.existing_request(ctx, key, &hash).await? {
        return Ok(operation);
    }
    let _lease = store.lease(ctx).await?.ok_or(ApiError::Conflict)?;
    if let Some(operation) = store.existing_request(ctx, key, &hash).await? {
        return Ok(operation);
    }
    Ok(store.enqueue(ctx, &request, &hash, trace).await?)
}
/// pendingを実行し、所有セッションのないrunningは確認待ちにする。並行実行は最大4組織。
pub async fn run(state: AppState) {
    if KeycloakAdmin::from_config(&state.http, &state.config.auth).is_err() {
        return;
    }
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        interval.tick().await;
        let pending = match state.directory.admin().pending_operations().await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::error!(%error, "directory operation queue unavailable");
                continue;
            }
        };
        for job in pending {
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                break;
            };
            let Ok(tenant) = TenantId::parse(job.tenant_id) else {
                continue;
            };
            let ctx = AuthContext::new(
                Principal {
                    kind: PrincipalKind::User,
                    id: job.actor_id,
                    email: None,
                    groups: vec![],
                    roles: vec![],
                    tenant_id: Some(tenant.to_string()),
                },
                job.org,
                tenant,
            );
            let state = state.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = process(&state, &ctx, job.id).await {
                    tracing::error!(operation_id = %job.id, %error, "directory operation failed");
                }
            });
        }
    }
}

pub async fn process(state: &AppState, ctx: &AuthContext, id: Uuid) -> Result<(), ApiError> {
    let store = state.directory.admin();
    let Some(lease) = store.lease(ctx).await? else {
        return Ok(());
    };
    let operation = store.operation(ctx, id).await?;
    if operation.state == "running" {
        // 所有セッションを失った実行は自動継承しない。外部の受付済み書込みを確認してから手動再開する。
        lease.verify().await?;
        store
            .fail_owned_operation(ctx, id, operation.attempts, "execution_interrupted")
            .await?;
        return Ok(());
    }
    let ownership = AtomicI32::new(0);
    let uncertain = AtomicBool::new(false);
    match lease
        .while_live(process_steps(
            state, ctx, id, &lease, &ownership, &uncertain,
        ))
        .await
    {
        Ok(result) => {
            if let Err(error) = &result {
                let live = lease.verify().await.is_ok();
                let code = if live && !uncertain.load(Ordering::SeqCst) {
                    match error {
                        ApiError::Forbidden => "authorization_revoked",
                        ApiError::Conflict => "version_conflict",
                        ApiError::NotFound => "reference_missing",
                        _ => "synchronization_failed",
                    }
                } else {
                    "execution_interrupted"
                };
                let attempt = ownership.load(Ordering::SeqCst);
                if attempt > 0 {
                    store.fail_owned_operation(ctx, id, attempt, code).await?;
                }
            }
            result
        }
        Err(error) => {
            let attempt = ownership.load(Ordering::SeqCst);
            if attempt > 0 {
                store
                    .fail_owned_operation(ctx, id, attempt, "execution_interrupted")
                    .await?;
            }
            Err(error.into())
        }
    }
}

async fn process_steps(
    state: &AppState,
    ctx: &AuthContext,
    id: Uuid,
    lease: &storage::directory::admin::DirectoryLease,
    ownership: &AtomicI32,
    uncertain: &AtomicBool,
) -> Result<(), ApiError> {
    let store = state.directory.admin();
    let Some((request, processed, attempt)) = store.start_owned_operation(ctx, id).await? else {
        return Ok(());
    };
    ownership.store(attempt, Ordering::SeqCst);
    if !authz::is_org_admin(state.authz.as_ref(), ctx, Consistency::HigherConsistency).await? {
        return Err(ApiError::Forbidden);
    }
    tracing::info!(operation_id = %id, "directory operation started");
    if request.roles.is_empty() && request.mappings.is_empty() {
        connection::sync(state, ctx, id, lease, uncertain).await?;
        return Ok(());
    }
    store
        .reserve_new_roles(ctx, request.roles.get(processed..).unwrap_or(&[]), id)
        .await?;
    let keycloak = KeycloakAdmin::from_config(&state.http, &state.config.auth)
        .map_err(|_| ApiError::ServiceUnavailable("Keycloak管理接続が未設定です".into()))?;
    let role_container = if request.roles.is_empty() {
        None
    } else {
        lease.verify().await?;
        uncertain.store(true, Ordering::SeqCst);
        let container = keycloak
            .directory_role_container(ctx.tenant_id(), ctx.org())
            .await
            .map_err(|error| {
                tracing::error!(%error, "Keycloak role container unavailable");
                ApiError::ServiceUnavailable("管理ロールの保存先を確認してください".into())
            })?;
        uncertain.store(false, Ordering::SeqCst);
        Some(container)
    };
    let remaining: Vec<_> = request
        .roles
        .iter()
        .skip(processed)
        .map(|r| r.id.clone())
        .collect();
    let current_roles: std::collections::BTreeMap<_, _> = store
        .role_definitions(ctx, &remaining)
        .await?
        .into_iter()
        .map(|role| (role.id.clone(), role))
        .collect();
    for (index, edit) in request.roles.iter().enumerate().skip(processed) {
        let current = current_roles.get(&edit.id);
        if current.map_or(0, |role| role.version) != edit.expected_version {
            return Err(ApiError::Conflict);
        }
        lease.verify().await?;
        uncertain.store(true, Ordering::SeqCst);
        let keycloak_id = keycloak
            .save_directory_role(
                role_container
                    .as_deref()
                    .ok_or_else(|| ApiError::Internal("role container missing".into()))?,
                edit,
            )
            .await
            .map_err(|error| {
                tracing::error!(%error, "Keycloak role definition failed");
                ApiError::ServiceUnavailable("Keycloakへの反映に失敗しました".into())
            })?;
        let previous = current.and_then(|role| role.parent_id.as_deref());
        if previous != edit.parent_id.as_deref() {
            let subject = ctx.ns().role_member(&edit.id);
            if let Some(parent) = previous {
                lease.verify().await?;
                state
                    .authz
                    .delete_tuple(&subject, Relation::Member, &ctx.ns().role(parent))
                    .await?;
            }
            if let Some(parent) = &edit.parent_id {
                lease.verify().await?;
                state
                    .authz
                    .write_tuple(&subject, Relation::Member, &ctx.ns().role(parent))
                    .await?;
            }
        }
        lease.verify().await?;
        store
            .save_role(
                ctx,
                edit,
                &keycloak_id,
                id,
                i32::try_from(index)
                    .map_err(|_| ApiError::BadRequest("変更件数が多すぎます".into()))?,
            )
            .await?;
        uncertain.store(false, Ordering::SeqCst);
    }
    let offset = request.roles.len();
    for (index, mapping) in request
        .mappings
        .iter()
        .enumerate()
        .skip(processed.saturating_sub(offset))
    {
        lease.verify().await?;
        store
            .save_mapping(
                ctx,
                mapping,
                id,
                i32::try_from(offset + index)
                    .map_err(|_| ApiError::BadRequest("変更件数が多すぎます".into()))?,
            )
            .await?;
    }
    if let Err(error) = state.data.notify_authz_changed(ctx, None).await {
        tracing::warn!(%error, "directory authz change notification failed");
    }
    Ok(())
}

/// 入力検証のメッセージだけを利用者へ返す。外部応答やDBエラーは含めない。
pub fn input_error(message: String) -> ApiError {
    let body = crate::error_body::BadRequestBody {
        message,
        ..crate::error_body::BadRequestBody::new(data::InvalidCode::InvalidInput, "", None)
    };
    ApiError::UnprocessableJson(crate::error_body::to_body(&body))
}
pub fn public_error(error: ApiError) -> ApiError {
    match error {
        ApiError::BadRequest(message) => input_error(message),
        other => other,
    }
}
