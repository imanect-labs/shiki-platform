//! run の起動（対話トリガ）・実行履歴 read（一覧/詳細/step/イベント・Task 10.2/10.14）。
//!
//! 履歴の正 = `workflow_run`/`step_execution`/`run_event`（engine.md §11.3）。
//! 一覧は必要列のみ・keyset ページング。step の output 本体は詳細エンドポイントで遅延取得する
//! （記録時レダクト済み・閲覧は workflow の artifact viewer でゲート）。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
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

/// 対話トリガの run 起動リクエスト（起動入力）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct StartRunRequest {
    /// run 起動入力（`$from input` の源）。
    #[serde(default)]
    #[schema(value_type = Object)]
    pub input: serde_json::Value,
}

/// run 起動レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct StartRunResponse {
    /// 起動した run の id（起動されなかった場合は null）。
    pub run_id: Option<Uuid>,
}

/// ワークフローを対話トリガで起動する（実行主体＝起動ユーザーの権限）。
#[utoipa::path(
    post,
    path = "/workflows/{id}/runs",
    params(("id" = Uuid, Path, description = "ワークフロー ID")),
    request_body = StartRunRequest,
    responses(
        (status = 202, description = "起動した", body = StartRunResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    security(("session" = [])),
)]
pub async fn start_workflow_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
    Json(req): Json<StartRunRequest>,
) -> Result<(StatusCode, Json<StartRunResponse>), ApiError> {
    let launcher = state
        .workflow_launcher
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("workflow 実行時が無効です".into()))?;
    // start_interactive 内で workflows.get_latest の OpenFGA 認可を通る。IR 取得失敗（権限なし/不在）は
    // 存在秘匿のため 404 に写す（500 にしない）。
    let run_id = launcher
        .start_interactive(&ctx, id, &req.input)
        .await
        .map_err(|e| match e {
            workflow_engine::run::LauncherError::Ir(_) => ApiError::NotFound,
            other => ApiError::Internal(format!("run 起動: {other}")),
        })?;
    Ok((StatusCode::ACCEPTED, Json(StartRunResponse { run_id })))
}

/// ワークフロー閲覧権限＋kind を検証する（履歴系ハンドラ共通・存在秘匿）。
pub(super) async fn require_workflow_viewer(
    state: &AppState,
    ctx: &authz::AuthContext,
    id: Uuid,
    trace: Option<&str>,
) -> Result<(), ApiError> {
    let meta = state.artifacts.get(ctx, id, trace).await?;
    if meta.kind != artifact::ArtifactKind::Workflow {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub(super) fn runs_or_503(state: &AppState) -> Result<&workflow_engine::RunStore, ApiError> {
    state
        .workflow_runs
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("workflow 実行時が無効です".into()))
}

/// run 一覧クエリ（faceted filter・keyset）。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListRunsQuery {
    /// 絞り込む status（カンマ区切り・例 `failed,running`）。
    pub status: Option<String>,
    /// 絞り込むトリガ種（カンマ区切り・例 `schedule,event`）。
    pub trigger_kind: Option<String>,
    /// created_at の下限（RFC3339）。
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    /// created_at の上限（RFC3339）。
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    /// keyset カーソル（前ページ末尾の created_at）。
    pub before_created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// keyset カーソル（前ページ末尾の run_id）。
    pub before_run_id: Option<Uuid>,
    /// 最大件数（既定 50・上限 100）。
    pub limit: Option<i64>,
}

/// run 一覧の 1 行。
#[derive(Debug, Serialize, ToSchema)]
pub struct RunListItemDto {
    pub run_id: Uuid,
    pub status: String,
    pub trigger_kind: String,
    pub version: i64,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// run 一覧レスポンス（次ページは末尾行の (created_at, run_id) をカーソルに）。
#[derive(Debug, Serialize, ToSchema)]
pub struct RunListResponse {
    pub items: Vec<RunListItemDto>,
}

/// run 一覧を取得する（作成日降順・フィルタ付き・Task 10.14）。
#[utoipa::path(
    get,
    path = "/workflows/{id}/runs",
    params(("id" = Uuid, Path, description = "ワークフロー ID"), ListRunsQuery),
    responses(
        (status = 200, description = "run 一覧", body = RunListResponse),
        (status = 404, description = "存在しない/権限なし"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    tag = "workflows"
)]
pub async fn list_workflow_runs(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<ListRunsQuery>,
) -> Result<Json<RunListResponse>, ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    let runs = runs_or_503(&state)?;
    let split = |v: &Option<String>| -> Vec<String> {
        v.as_deref()
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let filter = workflow_engine::RunListFilter {
        statuses: split(&q.status),
        trigger_kinds: split(&q.trigger_kind),
        from: q.from,
        to: q.to,
    };
    let before = match (q.before_created_at, q.before_run_id) {
        (Some(at), Some(rid)) => Some((at, rid)),
        _ => None,
    };
    let items = runs
        .list_runs(&ctx.tenant_id, id, &filter, before, q.limit.unwrap_or(50))
        .await
        .map_err(|e| ApiError::Internal(format!("run 一覧: {e}")))?;
    Ok(Json(RunListResponse {
        items: items
            .into_iter()
            .map(|r| RunListItemDto {
                run_id: r.run_id,
                status: r.status,
                trigger_kind: r.trigger_kind,
                version: r.version,
                created_at: r.created_at.to_rfc3339(),
                started_at: r.started_at.map(|t| t.to_rfc3339()),
                finished_at: r.finished_at.map(|t| t.to_rfc3339()),
            })
            .collect(),
    }))
}

/// step 概要（run 詳細に同梱・output 本体は steps エンドポイントで遅延取得）。
#[derive(Debug, Serialize, ToSchema)]
pub struct StepOverviewDto {
    pub step_path: String,
    pub node_id: String,
    pub status: String,
    pub attempt: i32,
    pub taken_ports: Vec<String>,
    pub has_output: bool,
    /// エラー詳細（記録時レダクト済み・null = なし）。
    #[schema(value_type = Object)]
    pub error: serde_json::Value,
    pub next_retry_at: Option<String>,
    pub wake_at: Option<String>,
    pub updated_at: String,
    /// AI ノードの Langfuse trace 突合（engine.md §11.2）。
    pub langfuse_trace_id: Option<String>,
}

/// run 詳細レスポンス（履歴 UI のヘッダ＋タイムライン素材）。
#[derive(Debug, Serialize, ToSchema)]
pub struct RunDetailResponse {
    pub run_id: Uuid,
    pub status: String,
    pub trigger_kind: String,
    pub version: i64,
    /// run 入力。
    #[schema(value_type = Object)]
    pub input: serde_json::Value,
    pub fail_reason: Option<String>,
    /// OTel trace id（監査↔OTel↔Langfuse 相関・Task 10.14 DoD）。
    pub trace_id: Option<String>,
    pub cancel_requested: bool,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub steps: Vec<StepOverviewDto>,
}

/// run の詳細（step 概要込み）を取得する。
#[utoipa::path(
    get,
    path = "/workflows/{id}/runs/{run_id}",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("run_id" = Uuid, Path, description = "run ID")
    ),
    responses(
        (status = 200, description = "run 詳細", body = RunDetailResponse),
        (status = 404, description = "存在しない/権限なし"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    tag = "workflows"
)]
pub async fn get_workflow_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<RunDetailResponse>, ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    let runs = runs_or_503(&state)?;
    // run は workflow_id 束縛で引く（別ワークフローの run_id を渡しても存在秘匿）。
    let d = runs
        .run_detail(&ctx.tenant_id, id, run_id)
        .await
        .map_err(|e| ApiError::Internal(format!("run 詳細: {e}")))?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(RunDetailResponse {
        run_id: d.run_id,
        status: d.status,
        trigger_kind: d.trigger_kind,
        version: d.version,
        input: d.input,
        fail_reason: d.fail_reason,
        trace_id: d.trace_id,
        cancel_requested: d.cancel_requested,
        created_at: d.created_at.to_rfc3339(),
        started_at: d.started_at.map(|t| t.to_rfc3339()),
        finished_at: d.finished_at.map(|t| t.to_rfc3339()),
        steps: d
            .steps
            .into_iter()
            .map(|s| StepOverviewDto {
                step_path: s.step_path,
                node_id: s.node_id,
                status: s.status,
                attempt: s.attempt,
                taken_ports: s.taken_ports,
                has_output: s.has_output,
                error: s.error.map_or(serde_json::Value::Null, |j| j.0),
                next_retry_at: s.next_retry_at.map(|t| t.to_rfc3339()),
                wake_at: s.wake_at.map(|t| t.to_rfc3339()),
                updated_at: s.updated_at.to_rfc3339(),
                langfuse_trace_id: s.langfuse_trace_id,
            })
            .collect(),
    }))
}

/// step 詳細クエリ（step_path は `[`/`.` を含むため query param）。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct StepDetailQuery {
    /// 対象 step の step_path（例 `map[2].work`）。
    pub path: String,
}

/// step 詳細レスポンス（入出力プレビュー・記録時レダクト済み）。
#[derive(Debug, Serialize, ToSchema)]
pub struct StepDetailResponse {
    pub step_path: String,
    pub node_id: String,
    pub status: String,
    pub attempt: i32,
    pub taken_ports: Vec<String>,
    #[schema(value_type = Object)]
    pub output: serde_json::Value,
    #[schema(value_type = Object)]
    pub error: serde_json::Value,
    pub langfuse_trace_id: Option<String>,
}

/// step の詳細（output/error 本体）を取得する（遅延取得・Task 10.14）。
#[utoipa::path(
    get,
    path = "/workflows/{id}/runs/{run_id}/steps",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("run_id" = Uuid, Path, description = "run ID"),
        StepDetailQuery
    ),
    responses(
        (status = 200, description = "step 詳細", body = StepDetailResponse),
        (status = 404, description = "存在しない/権限なし"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    tag = "workflows"
)]
pub async fn get_workflow_step(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<StepDetailQuery>,
) -> Result<Json<StepDetailResponse>, ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    let runs = runs_or_503(&state)?;
    let d = runs
        .step_detail(&ctx.tenant_id, id, run_id, &q.path)
        .await
        .map_err(|e| ApiError::Internal(format!("step 詳細: {e}")))?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(StepDetailResponse {
        step_path: d.step_path,
        node_id: d.node_id,
        status: d.status,
        attempt: d.attempt,
        taken_ports: d.taken_ports,
        output: d.output,
        error: d.error,
        langfuse_trace_id: d.langfuse_trace_id,
    }))
}

/// run_event クエリ。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct RunEventsQuery {
    /// この seq より後を返す（省略時は先頭から）。
    pub after_seq: Option<i64>,
    /// 最大件数（既定 500・上限 1000）。
    pub limit: Option<i64>,
}

/// run_event の 1 行。
#[derive(Debug, Serialize, ToSchema)]
pub struct RunEventDto {
    pub seq: i64,
    pub kind: String,
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    pub created_at: String,
}

/// run_event 一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct RunEventsResponse {
    pub items: Vec<RunEventDto>,
}

/// run のイベント列（タイムライン・SSE リプレイと同源）を取得する。
#[utoipa::path(
    get,
    path = "/workflows/{id}/runs/{run_id}/events",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("run_id" = Uuid, Path, description = "run ID"),
        RunEventsQuery
    ),
    responses(
        (status = 200, description = "イベント列", body = RunEventsResponse),
        (status = 404, description = "存在しない/権限なし"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    tag = "workflows"
)]
pub async fn list_workflow_run_events(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<RunEventsQuery>,
) -> Result<Json<RunEventsResponse>, ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    let runs = runs_or_503(&state)?;
    let items = runs
        .list_events(
            &ctx.tenant_id,
            id,
            run_id,
            q.after_seq.unwrap_or(0),
            q.limit.unwrap_or(500),
        )
        .await
        .map_err(|e| ApiError::Internal(format!("run イベント: {e}")))?;
    Ok(Json(RunEventsResponse {
        items: items
            .into_iter()
            .map(|e| RunEventDto {
                seq: e.seq,
                kind: e.kind,
                payload: e.payload.0,
                created_at: e.created_at.to_rfc3339(),
            })
            .collect(),
    }))
}
