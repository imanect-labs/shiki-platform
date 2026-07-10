//! 有効化・同意・トリガ実体化 API（Task 10.4a 残・engine.md §10）。
//!
//! - GET registration — 有効化状態＋委譲一覧（再同意バナー用）
//! - GET consent-plan — 同意画面の提案 grants（IR 静的分析＋secret 名→id 解決）
//! - POST enable — 明示委譲付き有効化（scope 拡大で grants 不足なら 409 missing_scopes）
//! - POST disable — トリガ停止
//!
//! authz: 参照系は artifact viewer（`WorkflowStore` 経由の取得が担保）、enable/disable は
//! artifact **editor** を明示 check（閲覧者による有効化 = 権限行使の踏み台を防ぐ）。
//! 委譲範囲の検証は `DelegationStore` が fail-closed で行う。監査は 1 操作 1 行。

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use utoipa::ToSchema;
use uuid::Uuid;
use workflow_engine::{EnableError, GrantRequest, RegistrationService, WorkflowIr};

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::save::map_store_err;

/// 委譲 1 件の表示。
#[derive(Debug, Serialize, ToSchema)]
pub struct DelegationItem {
    pub delegator: String,
    pub scope: String,
    /// FGA オブジェクト参照（例: `folder:<tenant>|<id>`）。
    pub object_ref: String,
    pub relation: String,
    pub granted_at: String,
}

/// registration の現況。
#[derive(Debug, Serialize, ToSchema)]
pub struct RegistrationResponse {
    /// enabled / disabled / suspended_reconsent / none。
    pub status: String,
    pub enabled_version: Option<i64>,
    pub consented_scopes: Vec<String>,
    pub enabled_by: Option<String>,
    pub delegations: Vec<DelegationItem>,
}

/// 同意画面の提案 grant。
#[derive(Debug, Serialize, ToSchema)]
pub struct SuggestedGrantItem {
    pub scope: String,
    /// folder / file / secret / workflow。
    pub object_kind: String,
    /// 確定済み対象 id（リテラル参照・secret 名解決済みのとき）。
    pub object_id: Option<String>,
    /// 参照名（secret の name 等・表示用）。
    pub object_name: Option<String>,
    pub relation: String,
    /// 提案の根拠（`node:<id>` / `trigger:<index>`）。
    pub source: String,
    /// 対象をユーザーが選ぶ必要があるか。
    pub needs_user_pick: bool,
}

/// 同意計画レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct ConsentPlanResponse {
    /// 宣言スコープ（権限の天井・同意対象）。
    pub declared_scopes: Vec<String>,
    pub grants: Vec<SuggestedGrantItem>,
}

/// 委譲 1 件の付与指定。
#[derive(Debug, Deserialize, ToSchema)]
pub struct GrantItem {
    /// declared_scope の 1 要素（例: `storage.read`）。
    pub scope: String,
    /// 対象種別（folder / file / secret / workflow）。
    pub object_type: String,
    /// 対象 id。
    pub object_id: String,
    /// 付与 relation（viewer / editor / can_use）。
    pub relation: String,
}

/// 有効化リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct EnableRequest {
    /// 有効化する IR バージョン（enable は version 単位・ir.md §9）。
    pub version: i64,
    /// 明示委譲（省略時は軽量切替 = scope 拡大が無い場合のみ可）。
    #[serde(default)]
    pub grants: Vec<GrantItem>,
}

/// 有効化レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct EnableResponse {
    pub status: String,
    pub enabled_version: i64,
}

/// enable/disable は artifact **editor** 必須。viewer 以下は存在秘匿の 404。
async fn require_editor(
    state: &AppState,
    ctx: &authz::AuthContext,
    workflow_id: Uuid,
) -> Result<(), ApiError> {
    let obj = ctx.ns().artifact(&workflow_id.to_string());
    let ok = state
        .authz
        .check(
            &ctx.subject(),
            authz::Relation::Editor,
            &obj,
            authz::Consistency::HigherConsistency,
        )
        .await?;
    if ok {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// 有効化状態と委譲一覧を返す。
#[utoipa::path(
    get,
    path = "/workflows/{id}/registration",
    params(("id" = Uuid, Path, description = "ワークフロー id")),
    responses(
        (status = 200, description = "現況", body = RegistrationResponse),
        (status = 404, description = "存在しない/権限なし"),
    ),
    tag = "workflows"
)]
pub async fn get_registration(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<RegistrationResponse>, ApiError> {
    // viewer ゲート（存在秘匿は IR 取得と同じ経路）。
    state
        .workflows
        .get_latest(&ctx, id, trace.as_deref())
        .await
        .map_err(map_store_err)?;
    let view = state
        .workflow_registration
        .view(&ctx.tenant_id, id)
        .await
        .map_err(map_enable_err)?;
    Ok(Json(RegistrationResponse {
        status: view.status,
        enabled_version: view.enabled_version,
        consented_scopes: view.consented_scopes,
        enabled_by: view.enabled_by,
        delegations: view
            .delegations
            .into_iter()
            .map(|d| DelegationItem {
                delegator: d.delegator,
                scope: d.scope,
                object_ref: d.object_ref,
                relation: d.relation,
                granted_at: d.granted_at.to_rfc3339(),
            })
            .collect(),
    }))
}

/// 指定バージョンの同意計画（提案 grants）を返す。
#[utoipa::path(
    get,
    path = "/workflows/{id}/versions/{version}/consent-plan",
    params(
        ("id" = Uuid, Path, description = "ワークフロー id"),
        ("version" = i64, Path, description = "IR バージョン")
    ),
    responses(
        (status = 200, description = "同意計画", body = ConsentPlanResponse),
        (status = 404, description = "存在しない/権限なし"),
    ),
    tag = "workflows"
)]
pub async fn consent_plan(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<ConsentPlanResponse>, ApiError> {
    let ir = fetch_ir(&state, &ctx, id, version, trace.as_deref()).await?;
    let mut grants: Vec<SuggestedGrantItem> = RegistrationService::consent_plan(&ir)
        .into_iter()
        .map(|g| SuggestedGrantItem {
            scope: g.scope,
            object_kind: g.object_kind,
            object_id: g.object_id,
            object_name: g.object_name,
            relation: g.relation,
            source: g.source,
            needs_user_pick: g.needs_user_pick,
        })
        .collect();
    // secret 参照名 → id 解決（有効化者が見える secret のみ・見えない名前は選択要に落とす）。
    if let Some(secrets) = state.secrets.as_deref() {
        if grants.iter().any(|g| g.object_kind == "secret") {
            let mine = secrets.list_mine(&ctx).await?;
            for g in grants.iter_mut().filter(|g| g.object_kind == "secret") {
                if let Some(name) = &g.object_name {
                    g.object_id = mine
                        .iter()
                        .find(|m| &m.name == name)
                        .map(|m| m.id.to_string());
                    g.needs_user_pick = g.object_id.is_none();
                }
            }
        }
    }
    Ok(Json(ConsentPlanResponse {
        declared_scopes: ir.declared_scopes.clone(),
        grants,
    }))
}

/// 有効化（明示委譲・単一論理操作・fail-closed）。
#[utoipa::path(
    post,
    path = "/workflows/{id}/enable",
    params(("id" = Uuid, Path, description = "ワークフロー id")),
    request_body = EnableRequest,
    responses(
        (status = 200, description = "有効化済み", body = EnableResponse),
        (status = 403, description = "有効化者の権限範囲外の委譲が含まれる"),
        (status = 404, description = "存在しない/editor でない"),
        (status = 409, description = "scope 拡大に grants が不足（missing_scopes 付き）"),
    ),
    tag = "workflows"
)]
pub async fn enable_workflow(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<EnableRequest>,
) -> Result<Json<EnableResponse>, ApiError> {
    require_editor(&state, &ctx, id).await?;
    let ir = fetch_ir(&state, &ctx, id, req.version, trace.as_deref()).await?;
    let grants = req
        .grants
        .iter()
        .map(|g| to_grant_request(&ctx, g))
        .collect::<Result<Vec<_>, _>>()?;
    state
        .workflow_registration
        .enable(&ctx, id, req.version, &ir, &grants)
        .await
        .map_err(map_enable_err)?;
    state
        .audit
        .record(
            &ctx,
            AuditEntry {
                action: "workflow.enable",
                object_type: "workflow",
                object_id: &id.to_string(),
                decision: Decision::Allow,
                trace_id: trace.as_deref(),
                metadata: json!({
                    "version": req.version,
                    "grants": req.grants.len(),
                    "light_switch": req.grants.is_empty(),
                }),
            },
        )
        .await?;
    Ok(Json(EnableResponse {
        status: "enabled".into(),
        enabled_version: req.version,
    }))
}

/// 無効化（トリガ停止・委譲タプルは温存＝再有効化で再同意不要。run 開始は status で fail-closed）。
#[utoipa::path(
    post,
    path = "/workflows/{id}/disable",
    params(("id" = Uuid, Path, description = "ワークフロー id")),
    responses(
        (status = 200, description = "無効化済み"),
        (status = 404, description = "存在しない/editor でない"),
    ),
    tag = "workflows"
)]
pub async fn disable_workflow(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_editor(&state, &ctx, id).await?;
    state
        .workflow_registration
        .disable(&ctx.tenant_id, id)
        .await
        .map_err(map_enable_err)?;
    state
        .audit
        .record(
            &ctx,
            AuditEntry {
                action: "workflow.disable",
                object_type: "workflow",
                object_id: &id.to_string(),
                decision: Decision::Allow,
                trace_id: trace.as_deref(),
                metadata: json!({}),
            },
        )
        .await?;
    Ok(Json(json!({ "status": "disabled" })))
}

/// 指定バージョンの IR を取得しパースする（artifact viewer ゲート込み・kind 不一致は 404 秘匿）。
async fn fetch_ir(
    state: &AppState,
    ctx: &authz::AuthContext,
    id: Uuid,
    version: i64,
    trace: Option<&str>,
) -> Result<WorkflowIr, ApiError> {
    state
        .workflows
        .get_latest(ctx, id, trace)
        .await
        .map_err(map_store_err)?;
    let body = state.artifacts.get_version(ctx, id, version, trace).await?;
    WorkflowIr::from_json(&body.body).map_err(|_| ApiError::NotFound)
}

/// API の grant 指定を FGA オブジェクトへ写像する（未知の種別/関係は 400）。
fn to_grant_request(ctx: &authz::AuthContext, g: &GrantItem) -> Result<GrantRequest, ApiError> {
    let ns = ctx.ns();
    let object = match g.object_type.as_str() {
        "folder" => ns.folder(&g.object_id),
        "file" => ns.file(&g.object_id),
        "secret" => ns.secret(&g.object_id),
        "workflow" => ns.artifact(&g.object_id),
        other => return Err(ApiError::BadRequest(format!("未知の object_type: {other}"))),
    };
    let relation = authz::Relation::parse(&g.relation)
        .ok_or_else(|| ApiError::BadRequest(format!("未知の relation: {}", g.relation)))?;
    Ok(GrantRequest {
        scope: g.scope.clone(),
        object,
        relation,
    })
}

/// エンジンのエラーを HTTP へ写像する。
fn map_enable_err(e: EnableError) -> ApiError {
    match e {
        EnableError::ScopeExpansion { missing } => ApiError::ConflictJson(json!({
            "code": "scope_expansion",
            "message": "scope が拡大しています（再同意が必要）",
            "missing_scopes": missing,
        })),
        EnableError::Delegation(workflow_engine::DelegationError::OutOfScope(d)) => {
            tracing::warn!(detail = %d, "権限範囲外の委譲要求を拒否");
            ApiError::Forbidden
        }
        EnableError::Delegation(err) => ApiError::Internal(format!("delegation: {err}")),
        EnableError::Internal(m) => ApiError::Internal(m),
    }
}
