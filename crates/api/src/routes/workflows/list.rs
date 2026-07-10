//! ワークフロー一覧（所有∪共有・要約射影）とエディタレイアウト（Task 10.12/10.14）。
//!
//! 一覧の認可は artifact の list（所有）＋ FGA ListObjects（共有）＝一覧の単一チョークポイント。
//! 要約（display_name/トリガ種/enabled 状態）は認可済み id 集合への単一 SQL 射影で、
//! IR 本文・body 全体は運ばない（必要フィールドのみ）。

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::runs::require_workflow_viewer;

/// 一覧クエリ（artifacts 一覧と同じ keyset 契約）。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListWorkflowsQuery {
    /// keyset カーソル（前ページ末尾の updated_at）。
    pub before_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// keyset カーソル（前ページ末尾の id）。
    pub before_id: Option<Uuid>,
    /// 最大件数（既定 50・上限 100）。
    pub limit: Option<i64>,
}

/// 一覧 1 行の要約。
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkflowSummaryDto {
    pub id: Uuid,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub current_version: i64,
    /// トリガ種（schedule/event/interactive）。
    pub trigger_kinds: Vec<String>,
    /// enabled / disabled / suspended_reconsent / none。
    pub enabled_status: String,
    pub enabled_version: Option<i64>,
    pub updated_at: String,
}

/// 一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkflowListResponse {
    pub items: Vec<WorkflowSummaryDto>,
}

/// ワークフロー一覧（所有∪共有・更新日降順）。
#[utoipa::path(
    get,
    path = "/workflows",
    params(ListWorkflowsQuery),
    responses(
        (status = 200, description = "一覧", body = WorkflowListResponse),
        (status = 401, description = "未認証"),
    ),
    tag = "workflows"
)]
pub async fn list_workflows(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<ListWorkflowsQuery>,
) -> Result<Json<WorkflowListResponse>, ApiError> {
    let before = match (q.before_updated_at, q.before_id) {
        (Some(at), Some(id)) => Some((at, id)),
        _ => None,
    };
    let limit = q.limit.unwrap_or(50);
    // 所有分（keyset）＋初回ページのみ共有分を合流（GET /artifacts と同じ契約）。
    let mut arts = state
        .artifacts
        .list_mine(&ctx, Some(artifact::ArtifactKind::Workflow), before, limit)
        .await?;
    if before.is_none() {
        let shared = state
            .artifacts
            .list_shared_with_me(&ctx, Some(artifact::ArtifactKind::Workflow), limit)
            .await?;
        arts.extend(shared);
        arts.sort_by_key(|a| std::cmp::Reverse((a.updated_at, a.id)));
        arts.dedup_by_key(|a| a.id);
    }
    let ids: Vec<Uuid> = arts.iter().map(|a| a.id).collect();
    let items = state
        .workflow_summaries
        .list(&ctx.tenant_id, &ids)
        .await
        .map_err(|e| ApiError::Internal(format!("一覧要約: {e}")))?;
    Ok(Json(WorkflowListResponse {
        items: items
            .into_iter()
            .map(|s| WorkflowSummaryDto {
                id: s.id,
                name: s.name,
                display_name: s.display_name,
                description: s.description,
                current_version: s.current_version,
                trigger_kinds: s.trigger_kinds,
                enabled_status: s.enabled_status,
                enabled_version: s.enabled_version,
                updated_at: s.updated_at.to_rfc3339(),
            })
            .collect(),
    }))
}

/// レイアウト本文（React Flow のノード座標・IR 外・非バージョン）。
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LayoutBody {
    /// レイアウト JSON（`{ "positions": { "<node_id>": { "x": .., "y": .. } }, ... }`）。
    #[schema(value_type = Object)]
    pub layout: serde_json::Value,
}

/// エディタレイアウトを取得する（未保存は `{}`・dagre 自動配置にフォールバック）。
#[utoipa::path(
    get,
    path = "/workflows/{id}/layout",
    params(("id" = Uuid, Path, description = "ワークフロー ID")),
    responses(
        (status = 200, description = "レイアウト", body = LayoutBody),
        (status = 404, description = "存在しない/権限なし"),
    ),
    tag = "workflows"
)]
pub async fn get_workflow_layout(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<LayoutBody>, ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    let layout = state
        .workflow_layout
        .get(&ctx.tenant_id, id)
        .await
        .map_err(|e| ApiError::Internal(format!("layout: {e}")))?;
    Ok(Json(LayoutBody { layout }))
}

/// エディタレイアウトを保存する（editor・256KB 上限）。
#[utoipa::path(
    put,
    path = "/workflows/{id}/layout",
    params(("id" = Uuid, Path, description = "ワークフロー ID")),
    request_body = LayoutBody,
    responses(
        (status = 200, description = "保存した"),
        (status = 400, description = "レイアウトが大きすぎる"),
        (status = 404, description = "存在しない/editor でない"),
    ),
    tag = "workflows"
)]
pub async fn put_workflow_layout(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(body): Json<LayoutBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    // 書込は editor（座標だけとはいえ他人の編集画面を荒らさせない）。
    let ok = state
        .authz
        .check(
            &ctx.subject(),
            authz::Relation::Editor,
            &ctx.ns().artifact(&id.to_string()),
            authz::Consistency::MinimizeLatency,
        )
        .await?;
    if !ok {
        return Err(ApiError::NotFound);
    }
    state
        .workflow_layout
        .put(&ctx.tenant_id, id, &body.layout)
        .await
        .map_err(|e| match e {
            workflow_engine::LayoutError::TooLarge => {
                ApiError::BadRequest("レイアウトが大きすぎます（256KB 上限）".into())
            }
            workflow_engine::LayoutError::Db(err) => ApiError::Internal(format!("layout: {err}")),
        })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
