//! ワークフロー一覧（所有∪共有・要約射影）とエディタレイアウト（Task 10.12/10.14）。
//!
//! 一覧の認可は artifact の list（所有）＋ FGA ListObjects（共有）＝一覧の単一チョークポイント。自分のものと共有された
//! ものを合わせて続きの位置を返すのは `GET /artifacts` と同じ入口（[`list_page`]）。
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
use crate::routes::artifact_list::{before_of, list_page, ListPageRequest, OwnedNext};

/// 一覧クエリ（`GET /artifacts` と同じ契約・[`list_page`]）。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListWorkflowsQuery {
    /// 自分のものの続き（前の応答の `owned_next.before_updated_at`）。
    pub before_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 自分のものの続き（前の応答の `owned_next.before_id`・`before_updated_at` と対で使う）。
    pub before_id: Option<Uuid>,
    /// 最大件数（既定 50・上限 100）。
    pub limit: Option<i64>,
    /// 共有された分の続き（前の応答の `shared_next_cursor`）。渡すと、共有された分の続きだけを返す（自分のものは
    /// 返さない・`before_*` と同時には使えない・BUG-185）。
    pub shared_cursor: Option<String>,
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
    /// 自動実行の起動を見送り続けているか（`GET /workflows/{id}/registration` の `launch_skip` があるか・GAP-67）。
    /// 理由と直し方は登録の API が返す。
    pub launch_skipped: bool,
    pub updated_at: String,
}

/// 一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkflowListResponse {
    pub items: Vec<WorkflowSummaryDto>,
    /// 自分のものに続きがあるときの位置（`before_updated_at`・`before_id` に渡す・無ければ `null`・BUG-185）。
    pub owned_next: Option<OwnedNext>,
    /// 共有された分に続きがあるときの位置（`shared_cursor` に渡す・無ければ `null`）。共有された分が多い人では、
    /// 共有された分が `limit` より少ないか 0 件でも続きがあることがある（BUG-162・BUG-185）。
    pub shared_next_cursor: Option<String>,
}

/// ワークフロー一覧（所有∪共有・更新日降順）。
///
/// 自分のものの続きは `owned_next` を `before_*` に、共有された分の続きは `shared_next_cursor` を `shared_cursor` に
/// 渡して読む（`GET /artifacts?kind=workflow` と同じ契約・同じ位置）。
#[utoipa::path(
    get,
    path = "/workflows",
    params(ListWorkflowsQuery),
    responses(
        (status = 200, description = "一覧", body = WorkflowListResponse),
        (status = 400, description = "shared_cursor が読めない（cursor_invalid）・before_* と同時に渡した"),
        (status = 401, description = "未認証"),
    ),
    tag = "workflows"
)]
pub async fn list_workflows(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<ListWorkflowsQuery>,
) -> Result<Json<WorkflowListResponse>, ApiError> {
    let page = list_page(
        &state.artifacts,
        &state.data,
        &ctx,
        ListPageRequest {
            kind: Some(artifact::ArtifactKind::Workflow),
            before: before_of(q.before_updated_at, q.before_id),
            limit: q.limit.unwrap_or(50),
            shared_cursor: q.shared_cursor.as_deref(),
        },
    )
    .await?;
    let ids: Vec<Uuid> = page.items.iter().map(|a| a.id).collect();
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
                launch_skipped: s.launch_skipped,
                updated_at: s.updated_at.to_rfc3339(),
            })
            .collect(),
        owned_next: page.owned_next,
        shared_next_cursor: page.shared_next_cursor,
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
