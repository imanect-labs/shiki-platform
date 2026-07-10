//! run のキャンセル・再実行・ライブイベント SSE（Task 10.14・engine.md §9.3/§11.4）。
//!
//! - cancel: v1 = step 境界検知（実行中 step の即時中断はしない・過約束しない）
//! - retry: `resume`（失敗 step から再開・checkpoint 再利用）/ `new`（同一入力・**元 version ピン**の新規 run）
//! - SSE: `run_event` を Last-Event-ID からリプレイ→1s 間隔の DB ポーリングで追記配信
//!   （DB=truth。run が terminal になったら `run.terminal` を送って閉じる）
//!
//! authz: cancel/retry は「run を見られる人」ではなく **artifact editor または run 起動者本人**
//! （閲覧者に他人の run を止めさせない）。SSE は viewer。

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::stream::Stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use workflow_engine::{CancelOutcome, ResumeOutcome};

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::runs::{require_workflow_viewer, runs_or_503};

/// cancel/retry の操作権限: artifact editor **または** run 起動者本人（interactive）。
async fn require_run_operator(
    state: &AppState,
    ctx: &authz::AuthContext,
    workflow_id: Uuid,
    run_id: Uuid,
) -> Result<(), ApiError> {
    let is_editor = state
        .authz
        .check(
            &ctx.subject(),
            authz::Relation::Editor,
            &ctx.ns().artifact(&workflow_id.to_string()),
            authz::Consistency::MinimizeLatency,
        )
        .await?;
    if is_editor {
        return Ok(());
    }
    let runs = runs_or_503(state)?;
    let principal = runs
        .run_principal(&ctx.tenant_id, workflow_id, run_id)
        .await
        .map_err(|e| ApiError::Internal(format!("run principal: {e}")))?
        .ok_or(ApiError::NotFound)?;
    let (p, kind, _trigger) = principal;
    if kind == "user" && p == ctx.principal.id {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// キャンセルのレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct CancelResponse {
    /// requested（受理）/ already_terminal。
    pub outcome: String,
}

/// run のキャンセルを要求する（step 境界検知・実行中 step は完走後に確定）。
#[utoipa::path(
    post,
    path = "/workflows/{id}/runs/{run_id}/cancel",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("run_id" = Uuid, Path, description = "run ID")
    ),
    responses(
        (status = 200, description = "受理（または既に terminal）", body = CancelResponse),
        (status = 404, description = "存在しない/権限なし"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    tag = "workflows"
)]
pub async fn cancel_workflow_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<CancelResponse>, ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    require_run_operator(&state, &ctx, id, run_id).await?;
    let runs = runs_or_503(&state)?;
    let outcome = runs
        .request_cancel(&ctx.tenant_id, id, run_id)
        .await
        .map_err(|e| ApiError::Internal(format!("cancel: {e}")))?;
    match outcome {
        CancelOutcome::Requested => Ok(Json(CancelResponse {
            outcome: "requested".into(),
        })),
        CancelOutcome::AlreadyTerminal(_) => Ok(Json(CancelResponse {
            outcome: "already_terminal".into(),
        })),
        CancelOutcome::NotFound => Err(ApiError::NotFound),
    }
}

/// 再実行モード。
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetryMode {
    /// 失敗 step から同一 run 内で再開（成功済み checkpoint は再利用）。
    Resume,
    /// 同一入力・元 version ピンで新規 run を作る（実行主体は呼び出し者）。
    New,
}

/// 再実行リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct RetryRequest {
    pub mode: RetryMode,
}

/// 再実行レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct RetryResponse {
    /// resume 時は元 run_id・new 時は新規 run_id。
    pub run_id: Option<Uuid>,
    pub mode: String,
}

/// run を再実行する（resume = 失敗 step から再開 / new = 同一入力の新規 run）。
#[utoipa::path(
    post,
    path = "/workflows/{id}/runs/{run_id}/retry",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("run_id" = Uuid, Path, description = "run ID")
    ),
    request_body = RetryRequest,
    responses(
        (status = 202, description = "再実行を受理", body = RetryResponse),
        (status = 404, description = "存在しない/権限なし"),
        (status = 409, description = "failed でない run の resume"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    tag = "workflows"
)]
pub async fn retry_workflow_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<RetryRequest>,
) -> Result<(axum::http::StatusCode, Json<RetryResponse>), ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    require_run_operator(&state, &ctx, id, run_id).await?;
    let runs = runs_or_503(&state)?;
    match req.mode {
        RetryMode::Resume => {
            // schedule/event run の再開は委譲の現在有効性を fail-closed で再確認する
            // （失権後に人手で動かし直す抜け道を作らない・engine.md §6.2）。
            let detail = runs
                .run_detail(&ctx.tenant_id, id, run_id)
                .await
                .map_err(|e| ApiError::Internal(format!("run: {e}")))?
                .ok_or(ApiError::NotFound)?;
            if detail.trigger_kind != "interactive" {
                // 検査対象は**この run のピン済み declared_scopes**（現 consented を渡すと
                // 再同意で縮小した scope の検知が自明式になり失われる・Codex P1）。
                let declared = runs
                    .run_declared_scopes(&ctx.tenant_id, id, run_id)
                    .await
                    .map_err(|e| ApiError::Internal(format!("run scopes: {e}")))?
                    .ok_or(ApiError::NotFound)?;
                let admission = state
                    .workflow_registration
                    .check_run_start(&ctx.tenant_id, id, &declared)
                    .await
                    .map_err(|e| ApiError::Internal(format!("delegation: {e}")))?;
                if !matches!(admission, workflow_engine::RunAdmission::Ok) {
                    return Err(ApiError::ConflictJson(serde_json::json!({
                        "code": "delegation_invalid",
                        "message": "委譲が無効のため再開できません（再同意が必要です）",
                    })));
                }
            }
            match runs
                .resume_failed(&ctx.tenant_id, id, run_id)
                .await
                .map_err(|e| ApiError::Internal(format!("resume: {e}")))?
            {
                ResumeOutcome::Resumed => Ok((
                    axum::http::StatusCode::ACCEPTED,
                    Json(RetryResponse {
                        run_id: Some(run_id),
                        mode: "resume".into(),
                    }),
                )),
                ResumeOutcome::NotFailed(s) => Err(ApiError::ConflictJson(serde_json::json!({
                    "code": "not_failed",
                    "message": format!("failed でない run（{s}）は再開できません"),
                }))),
                ResumeOutcome::NotFound => Err(ApiError::NotFound),
            }
        }
        RetryMode::New => {
            let launcher = state
                .workflow_launcher
                .as_ref()
                .ok_or_else(|| ApiError::ServiceUnavailable("workflow 実行時が無効です".into()))?;
            let detail = runs
                .run_detail(&ctx.tenant_id, id, run_id)
                .await
                .map_err(|e| ApiError::Internal(format!("run: {e}")))?
                .ok_or(ApiError::NotFound)?;
            // 元 version をピンし同一入力で新規 run（実行主体は呼び出し者＝正直な semantics）。
            let new_run = launcher
                .start_interactive_version(&ctx, id, detail.version, &detail.input)
                .await
                .map_err(|e| ApiError::Internal(format!("再実行: {e}")))?;
            Ok((
                axum::http::StatusCode::ACCEPTED,
                Json(RetryResponse {
                    run_id: new_run,
                    mode: "new".into(),
                }),
            ))
        }
    }
}

/// SSE クエリ（Last-Event-ID 相当の明示指定）。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct StreamQuery {
    /// この seq より後からリプレイ開始（省略時は 0 = 先頭から）。
    pub last_event_id: Option<i64>,
}

/// run_event のライブ配信（SSE・リプレイ→1s ポーリング追記・terminal で close）。
///
/// イベント形式: `id: <seq>` / `event: run_event` / `data: {kind, payload, created_at}`。
/// run が terminal になったら `event: run.terminal` を送って閉じる（クライアントは再接続不要）。
#[utoipa::path(
    get,
    path = "/workflows/{id}/runs/{run_id}/events/stream",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("run_id" = Uuid, Path, description = "run ID"),
        StreamQuery
    ),
    responses(
        (status = 200, description = "SSE ストリーム"),
        (status = 404, description = "存在しない/権限なし"),
    ),
    tag = "workflows"
)]
pub async fn stream_workflow_run_events(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<StreamQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    require_workflow_viewer(&state, &ctx, id, trace.as_deref()).await?;
    let runs = runs_or_503(&state)?.clone();
    // Last-Event-ID ヘッダ優先（EventSource の自動再接続）・無ければ query。
    let last_seq = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .or(q.last_event_id)
        .unwrap_or(0);
    let tenant = ctx.tenant_id.clone();

    struct PollState {
        runs: workflow_engine::RunStore,
        tenant: String,
        id: Uuid,
        run_id: Uuid,
        last_seq: i64,
        first: bool,
        done: bool,
    }
    let state0 = PollState {
        runs,
        tenant,
        id,
        run_id,
        last_seq,
        first: true,
        done: false,
    };
    // DB=truth のリプレイ→1s ポーリング（unfold で 1 バッチ/イテレーション・terminal で終端）。
    let stream = futures::stream::unfold(state0, |mut st| async move {
        if st.done {
            return None;
        }
        if !st.first {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        st.first = false;
        let mut batch: Vec<Result<Event, Infallible>> = Vec::new();
        match st
            .runs
            .list_events(&st.tenant, st.id, st.run_id, st.last_seq, 500)
            .await
        {
            Ok(events) => {
                for e in events {
                    st.last_seq = e.seq;
                    let data = serde_json::json!({
                        "kind": e.kind,
                        "payload": e.payload.0,
                        "created_at": e.created_at.to_rfc3339(),
                    });
                    batch.push(Ok(Event::default()
                        .id(st.last_seq.to_string())
                        .event("run_event")
                        .data(data.to_string())));
                }
            }
            Err(e) => tracing::warn!(error = %e, "run_event リプレイに失敗（SSE 継続）"),
        }
        // terminal 判定はリプレイが**追いついた**（1 ページ未満）ときだけ行う。500 件超の
        // バックログを持つ terminal run で残りを取りこぼして閉じない（Codex P2）。
        if batch.len() >= 500 {
            return Some((futures::stream::iter(batch), st));
        }
        // terminal なら終端イベントを添えて次回 None（EventSource は再接続不要）。
        match st.runs.run_detail(&st.tenant, st.id, st.run_id).await {
            Ok(Some(d)) if matches!(d.status.as_str(), "succeeded" | "failed" | "cancelled") => {
                batch.push(Ok(Event::default()
                    .event("run.terminal")
                    .data(serde_json::json!({ "status": d.status }).to_string())));
                st.done = true;
            }
            Ok(None) => st.done = true,
            _ => {}
        }
        Some((futures::stream::iter(batch), st))
    })
    .flatten();
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
