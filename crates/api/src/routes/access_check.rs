//! アプリとワークフローを使う人の権限の確かめ（GAP-37）。
//!
//! - `POST /mini-apps/{id}/access-check`・`POST /workflows/{id}/access-check`（作る人・editor）: 部品が要る権限の一覧と、
//!   共有の相手ごとに足りるか。相手ごとの結果は、共有を管理する持ち主だけが求められる（相手を省けば共有している相手）。
//! - `GET /mini-apps/{id}/access`（使う人・viewer）: 自分に足りない権限を部品ごとに返す（画面は押す前にボタンを止める）。
//!
//! 判定は OpenFGA の BatchCheck だけ（[`gui::AccessChecker`]）で、実際の読み書きの認可は変えない。見えない対象の名前は
//! 出さず、問う数に上限を置く。作る人の確かめは監査（`artifact.access_check`）に残す（誰が誰の権限を確かめたか）。

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use storage::audit::{AuditAction, AuditEntry, Decision};
use storage::ShareTarget;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::gui_errors::map_gui_err;
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// アプリの確かめの頼み。
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct MiniAppAccessCheckRequest {
    /// 確かめる版（省けば公開した版。まだ公開していなければ最新の版）。
    #[serde(default)]
    pub version: Option<i64>,
    /// 確かめる相手（人・部署・最大 20 件）。省けば共有している相手（持ち主だけ・ほかの人には相手ごとの結果を返さない）。
    /// 相手を書くのは持ち主だけができる。
    #[serde(default)]
    pub subjects: Option<Vec<ShareTarget>>,
}

/// ワークフローの確かめの頼み（確かめるのは最新の版・手動の実行と同じ）。
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct WorkflowAccessCheckRequest {
    /// 確かめる相手（人・部署・最大 20 件）。省けば共有している相手（持ち主だけ）。相手を書くのは持ち主だけができる。
    #[serde(default)]
    pub subjects: Option<Vec<ShareTarget>>,
}

/// 使う人の確かめのクエリ（version 省略時は公開した版。使う人は公開した版だけ）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct MyAccessQuery {
    pub version: Option<i64>,
}

fn checker(state: &AppState) -> gui::AccessChecker<'_> {
    gui::AccessChecker {
        authz: state.authz.as_ref(),
        artifacts: &state.artifacts,
        data: &state.data,
        storage: &state.storage,
    }
}

/// 作る人の確かめを組み立てる（相手の表示名を補い、監査に残す）。
async fn report(
    state: &AppState,
    ctx: &authz::AuthContext,
    id: Uuid,
    version: i64,
    parts: &[gui::PartNeeds],
    subjects: Option<(Vec<ShareTarget>, bool)>,
    trace: Option<&str>,
) -> Result<gui::AccessReport, ApiError> {
    let mut report = checker(state)
        .report(
            ctx,
            version,
            parts,
            subjects.as_ref().map(|(s, _)| s.as_slice()),
        )
        .await
        .map_err(map_gui_err)?;
    report.more_subjects = subjects.as_ref().is_some_and(|(_, more)| *more);
    if let Some(results) = report.subjects.as_mut() {
        let profiles =
            super::share_profiles::resolve(state, ctx, results.iter().map(|r| &r.subject)).await;
        for r in results.iter_mut() {
            r.display_name = profiles.get(&r.subject).display_name;
        }
    }
    let entry = AuditEntry {
        action: AuditAction::ArtifactAccessCheck,
        object_type: "artifact",
        object_id: &id.to_string(),
        decision: Decision::Allow,
        trace_id: trace,
        metadata: json!({
            "version": version,
            "subjects": report.subjects.as_ref().map(|s| s.iter().map(|r| &r.subject).collect::<Vec<_>>()),
            "needs": report.needs.len(),
            "truncated": report.truncated,
        }),
    };
    if let Err(e) = state.audit.record(ctx, entry).await {
        tracing::warn!(error = %e, "artifact.access_check の監査記録に失敗");
    }
    Ok(report)
}

/// アプリを使う人の権限を確かめる（作る人・editor）。部品が要る権限と、共有の相手ごとに足りるか。
#[utoipa::path(
    post,
    path = "/mini-apps/{id}/access-check",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    request_body = MiniAppAccessCheckRequest,
    responses(
        (status = 200, description = "部品が要る権限と、相手ごとの結果（持ち主だけ）", body = gui::AccessReport),
        (status = 400, description = "相手の指定の誤り・上限を超えた", body = super::gui_errors::GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "editor でない・持ち主でないのに相手を書いた"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn check_mini_app_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<MiniAppAccessCheckRequest>,
) -> Result<Json<gui::AccessReport>, ApiError> {
    let trace = trace.as_deref();
    let checker = checker(&state);
    checker
        .require_maker(&ctx, id, artifact::ArtifactKind::MiniApp, trace)
        .await
        .map_err(map_gui_err)?;
    let subjects = checker
        .subjects_for(&ctx, id, req.subjects, trace)
        .await
        .map_err(map_gui_err)?;
    let (version, parts) = state
        .mini_apps
        .part_needs(&ctx, id, req.version, "access_check", trace)
        .await
        .map_err(map_gui_err)?;
    let report = report(&state, &ctx, id, version, &parts, subjects, trace).await?;
    Ok(Json(report))
}

/// ワークフローを使う人の権限を確かめる（作る人・editor）。手動の実行で要る権限と、共有の相手ごとに足りるか。
#[utoipa::path(
    post,
    path = "/workflows/{id}/access-check",
    params(("id" = Uuid, Path, description = "ワークフロー ID")),
    request_body = WorkflowAccessCheckRequest,
    responses(
        (status = 200, description = "要る権限と、相手ごとの結果（持ち主だけ）", body = gui::AccessReport),
        (status = 400, description = "相手の指定の誤り・上限を超えた", body = super::gui_errors::GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "editor でない・持ち主でないのに相手を書いた"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn check_workflow_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<WorkflowAccessCheckRequest>,
) -> Result<Json<gui::AccessReport>, ApiError> {
    let trace = trace.as_deref();
    let checker = checker(&state);
    checker
        .require_maker(&ctx, id, artifact::ArtifactKind::Workflow, trace)
        .await
        .map_err(map_gui_err)?;
    let subjects = checker
        .subjects_for(&ctx, id, req.subjects, trace)
        .await
        .map_err(map_gui_err)?;
    let (version, ir) = state
        .workflows
        .get_latest(&ctx, id, trace)
        .await
        .map_err(super::workflows::save::map_store_err)?;
    let parts = [gui::workflow_needs(&ir)];
    let report = report(&state, &ctx, id, version, &parts, subjects, trace).await?;
    Ok(Json(report))
}

/// アプリを使う人自身に足りない権限（使う人・viewer）。足りない・範囲が狭い・使えない部品だけを返す。
#[utoipa::path(
    get,
    path = "/mini-apps/{id}/access",
    params(
        ("id" = Uuid, Path, description = "ミニアプリ ID"),
        MyAccessQuery,
    ),
    responses(
        (status = 200, description = "自分に足りない権限（部品ごと）", body = gui::MyAppAccess),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "使う人に出していない版（code=not_published・app_version_changed）"),
    ),
    security(("session" = [])),
)]
pub async fn my_mini_app_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<MyAccessQuery>,
) -> Result<Json<gui::MyAppAccess>, ApiError> {
    let trace = trace.as_deref();
    let (version, parts) = state
        .mini_apps
        .part_needs(&ctx, id, q.version, "my_access", trace)
        .await
        .map_err(map_gui_err)?;
    let mine = checker(&state)
        .mine(&ctx, version, &parts)
        .await
        .map_err(map_gui_err)?;
    Ok(Json(mine))
}
