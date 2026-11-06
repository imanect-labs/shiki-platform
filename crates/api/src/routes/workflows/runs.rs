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
    /// 試すきっかけの内容（`$from trigger` の源・項目のまとまり）。スケジュールやファイル・テーブルの変更を
    /// きっかけにするワークフローを手動で試すときに渡す（D-93）。省略すると、きっかけの内容は入力と同じ。
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub trigger: Option<serde_json::Value>,
    /// 試す予定の日時（`$from run` の `scheduled_at` の源・「日付を計算」の基準の既定）。スケジュールをきっかけに
    /// するワークフローを、その予定の時刻に動いたものとして手動で試すときに渡す（GAP-88）。時差つきの日時
    /// （RFC 3339）か、時差の無い日時（`2026-11-09T09:00`・日本時間で読む）。省略すると、予定の日時は実行を始めた
    /// 日時と同じ。実行履歴では、手動の実行で指定した予定の日時として出る。
    #[serde(default)]
    #[schema(value_type = Option<String>, example = "2026-11-09T09:00")]
    pub scheduled_at: Option<String>,
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
        (status = 400, description = "入力が input_schema に合わない（invalid_input・項目ごとの errors 付き）か、きっかけの内容が項目のまとまりでない（invalid_input・field = trigger）か、予定の日時が日時として読めない（invalid_input・field = scheduled_at）か、実行できない設定がある（not_ready・errors 付き）", body = crate::error_body::InvalidRunInputBody),
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
    // きっかけの内容は項目のまとまりだけを受ける（ペイロードはどのきっかけでも項目のまとまり・D-93）。
    if req.trigger.as_ref().is_some_and(|t| !t.is_object()) {
        return Err(ApiError::UnprocessableJson(crate::error_body::to_body(
            &crate::error_body::BadRequestBody::new(
                data::InvalidCode::InvalidInput,
                "きっかけの内容は、項目のまとまり（{ } で囲んだ JSON）で書いてください。",
                Some("trigger"),
            ),
        )));
    }
    // 試す予定の日時は、実行情報で言える日時だけを受ける（読めない値を黙って捨てて、始めた日時で動かさない・GAP-88）。
    let scheduled_at = match req.scheduled_at.as_deref() {
        None => None,
        Some(raw) => Some(
            workflow_engine::run::parse_trial_scheduled_at(raw).ok_or_else(|| {
                ApiError::UnprocessableJson(crate::error_body::to_body(
                    &crate::error_body::BadRequestBody::new(
                        data::InvalidCode::InvalidInput,
                        workflow_engine::run::planned::TRIAL_SCHEDULED_AT_INVALID,
                        Some("scheduled_at"),
                    ),
                ))
            })?,
        ),
    };
    let trial = workflow_engine::RunTrial {
        trigger: req.trigger.as_ref(),
        scheduled_at,
        rerun_of: None,
    };
    // start_interactive 内で workflows.get_latest の OpenFGA 認可を通る。IR 取得失敗（権限なし/不在）は
    // 存在秘匿のため 404 に写す（500 にしない）。
    let run_id = launcher
        .start_interactive_trial(&ctx, id, &req.input, trial)
        .await
        .map_err(|e| match e {
            workflow_engine::run::LauncherError::Ir(_) => ApiError::NotFound,
            // 入力の誤りは項目ごとの理由を返す（本文を捨てて「API 400」にしない・BUG-109）。
            workflow_engine::run::LauncherError::Input(issues) => invalid_input(&issues),
            // 必須の文字の欄が空のまま（保存は通る・実行の前に止める・D-43）。理由は欄に写せる形で返す。
            workflow_engine::run::LauncherError::NotReady(errors) => not_ready(&errors),
            other => ApiError::Internal(format!("run 起動: {other}")),
        })?;
    Ok((StatusCode::ACCEPTED, Json(StartRunResponse { run_id })))
}

/// 入力が入力項目の形に合わない 400（`invalid_input`・項目ごとの理由・BUG-109）。
pub(super) fn invalid_input(issues: &[workflow_engine::ir::input_schema::InputIssue]) -> ApiError {
    ApiError::UnprocessableJson(crate::error_body::to_body(
        &crate::error_body::InvalidRunInputBody::new(issues),
    ))
}

/// 実行できない設定（必須の文字の欄が空・D-43）の 400。有効化の 409 `not_ready` と同じ形で、理由を
/// 検証エラーの形（node_id・path つき）で全件返す（画面が欄に写せる）。
pub(super) fn not_ready(errors: &[workflow_engine::ValidationError]) -> ApiError {
    ApiError::UnprocessableJson(serde_json::json!({
        "code": "not_ready",
        "message": "実行できない設定があります",
        "errors": errors,
    }))
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

/// 履歴の読取口が返す待っているもの（`approval`・`event`・`timer`）を API の型へ（知らない値は null）。
fn waiting_for(raw: Option<&str>) -> Option<workflow_engine::WaitingFor> {
    raw.and_then(workflow_engine::WaitingFor::parse)
}

/// 履歴の読取口が返す予定の日時の由来を API の型へ（知らない値は null・GAP-89）。
fn scheduled_from(raw: Option<&str>) -> Option<workflow_engine::ScheduledAtSource> {
    raw.and_then(workflow_engine::ScheduledAtSource::parse)
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
    /// 待機中（`running` で、承認・変更・時刻を待つブロックで止まっている）なら待っているもの
    /// （engine.md §3.1 の導出値・実行中と分けて出す・BUG-119）。待機中でなければ null。
    pub waiting_for: Option<workflow_engine::WaitingFor>,
    pub trigger_kind: String,
    pub version: i64,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// 予定の日時（`$from run` の `scheduled_at` の源・GAP-89）。スケジュールで動いた実行はその回の予定の時刻
    /// （止まっていた後の発火や、多重度で待たされた実行でも変わらない）、手動の実行で指定した日時、「別のワークフローを
    /// 実行」で始めた実行が引き継いだ日時のどれか。ほかの実行（実行情報では始めた日時と同じ値）は null。
    pub scheduled_at: Option<String>,
    /// 予定の日時の由来（予定の日時が null なら null）。
    pub scheduled_at_source: Option<workflow_engine::ScheduledAtSource>,
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
                waiting_for: waiting_for(r.waiting_for.as_deref()),
                status: r.status,
                trigger_kind: r.trigger_kind,
                version: r.version,
                created_at: r.created_at.to_rfc3339(),
                started_at: r.started_at.map(|t| t.to_rfc3339()),
                finished_at: r.finished_at.map(|t| t.to_rfc3339()),
                scheduled_at: r.scheduled_at.map(|t| t.to_rfc3339()),
                scheduled_at_source: scheduled_from(r.scheduled_from.as_deref()),
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
    /// 待っている step なら待っているもの（承認・変更・時刻・run の `waiting_for` と同じ決め方・BUG-119）。
    pub waiting_for: Option<workflow_engine::WaitingFor>,
}

/// run 詳細レスポンス（履歴 UI のヘッダ＋タイムライン素材）。
#[derive(Debug, Serialize, ToSchema)]
pub struct RunDetailResponse {
    pub run_id: Uuid,
    pub status: String,
    /// 待機中なら待っているもの（[`RunListItemDto::waiting_for`] と同じ）。
    pub waiting_for: Option<workflow_engine::WaitingFor>,
    pub trigger_kind: String,
    pub version: i64,
    /// run 入力。
    #[schema(value_type = Object)]
    pub input: serde_json::Value,
    /// 手動の実行で試したきっかけの内容（実行のダイアログで書いた値か、「同じ入力でもう一度実行」が元の実行から
    /// 引き継いだ値・`$from trigger` の源・D-93・GAP-96）。試していない実行は null（きっかけの内容は入力と同じ）。
    /// 見られる人は実行の詳細と同じ（ワークフローを見られる人）。
    #[schema(value_type = Option<Object>)]
    pub trigger_payload: Option<serde_json::Value>,
    /// run 単位の理由の code（`run_timeout`・`approval_timeout` など・古い画面の互換のために残す）。画面は
    /// `failure` を出す。
    pub fail_reason: Option<String>,
    /// 失敗した run の理由（利用者向けの分類と文言・参照番号・失敗していなければ null）。文言は Rust の単一定義
    /// （`run::failure`）から組む（web に対応表を持たない・BUG-113）。
    pub failure: Option<workflow_engine::RunFailure>,
    /// 「失敗したところから再開」を勧めるかと理由（失敗していなければ null・BUG-132）。
    pub resume: Option<workflow_engine::ResumeHint>,
    /// OTel trace id（監査↔OTel↔Langfuse 相関・Task 10.14 DoD）。
    pub trace_id: Option<String>,
    pub cancel_requested: bool,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// 予定の日時（[`RunListItemDto::scheduled_at`] と同じ）。
    pub scheduled_at: Option<String>,
    /// 予定の日時の由来（[`RunListItemDto::scheduled_at_source`] と同じ）。
    pub scheduled_at_source: Option<workflow_engine::ScheduledAtSource>,
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
        waiting_for: waiting_for(d.waiting_for.as_deref()),
        status: d.status,
        trigger_kind: d.trigger_kind,
        version: d.version,
        input: d.input,
        trigger_payload: d.trigger_payload,
        fail_reason: d.fail_reason,
        failure: d.failure,
        resume: d.resume,
        trace_id: d.trace_id,
        cancel_requested: d.cancel_requested,
        created_at: d.created_at.to_rfc3339(),
        started_at: d.started_at.map(|t| t.to_rfc3339()),
        finished_at: d.finished_at.map(|t| t.to_rfc3339()),
        scheduled_at: d.scheduled_at.map(|t| t.to_rfc3339()),
        scheduled_at_source: scheduled_from(d.scheduled_from.as_deref()),
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
                waiting_for: waiting_for(s.waiting_for.as_deref()),
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
