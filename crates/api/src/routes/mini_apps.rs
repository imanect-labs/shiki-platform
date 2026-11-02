//! ミニアプリ API（Task 6.10・#604）。
//!
//! 作成/更新は**保存する人の権限**でピンを検証し（前の版と同じピンは確かめ直さない・BUG-80）、解決（実行）は
//! ミニアプリ本体の viewer のみを要求する（部品はバンドル権限チョークポイント経由・部品の個別共有は不要）。
//! 保存は下書きの版になり、使う人（editor でない viewer）が開く・押すのは公開した版だけ。公開は editor が
//! `POST /mini-apps/{id}/publish` で版を選んで行う（D-16・GAP-36）。
//! 画面（Yui の ```ui を含む Markdown）のボタンは、保存済みの版の画面から読み直し、束ねた
//! ワークフローだけを押した本人の権限で起動する（アンビエント権限なし）。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use gui::{MiniAppActionRequest, MiniAppRun, PublishPreview, SaveMode, SavedMiniApp};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::gui_errors::{map_gui_err, GuiValidationErrorResponse};
use super::ui_actions::{map_yui_action_err, UiActionResponse};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMiniAppRequest {
    pub name: String,
    /// mini_app body（description・screen・skill/workflows のバージョンピン）。
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
}

/// 更新（新バージョン追記）リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMiniAppRequest {
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// ミニアプリ本文レスポンス（保存した版・下書き）。
#[derive(Debug, Serialize, ToSchema)]
pub struct MiniAppResponse {
    pub id: Uuid,
    pub version: i64,
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
    /// 公開した版（保存しても変わらない。新しいアプリは `null`＝まだ公開していない・D-16）。
    pub published_version: Option<i64>,
    /// 画面の書き損じ（保存はできた。直すまで公開できない・D-16 の (c)）。`path` は `screen`。
    pub warnings: Vec<gui::GuiValidationError>,
}

impl MiniAppResponse {
    fn saved(saved: SavedMiniApp, body: serde_json::Value) -> Self {
        MiniAppResponse {
            id: saved.id,
            version: saved.version,
            body,
            published_version: saved.published_version,
            warnings: saved.warnings,
        }
    }
}

/// 解決済みミニアプリ（実行画面が使う一式・全て検証済み）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedMiniAppResponse {
    pub id: Uuid,
    pub version: i64,
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
    /// 使えない部品（壊れた任意のピン・BUG-17）。アプリは開けて、ここに挙がった部品だけが使えない。
    /// `path` が部品の位置（`skill`）、`message` が利用者向けの理由。
    pub unavailable: Vec<gui::GuiValidationError>,
    /// 閲覧者本人がこのアプリを編集できる（アプリの editor 以上。共有で編集にした人を含む・BUG-18）。
    /// 画面の出し分けだけに使う（保存は `PUT /mini-apps/{id}` が editor を改めて確かめる）。
    pub can_edit: bool,
    /// 閲覧者本人がこのアプリを共有できる（アプリの owner。共有相手の一覧と付与・解除は owner が要る・BUG-78）。
    /// 画面の出し分けだけに使う（共有の API が owner を改めて確かめる）。
    pub can_share: bool,
    /// 公開した版（まだ公開していなければ `null`・D-16）。`version` と違えば、開いたのは下書き（editor だけが開ける）。
    pub published_version: Option<i64>,
    /// 公開した時刻。
    pub published_at: Option<DateTime<Utc>>,
    /// 最新の版（下書きを含む・editor にだけ返す）。
    pub latest_version: Option<i64>,
    /// 前に公開していた版（公開を戻す先・editor にだけ返す）。
    pub previous_published_version: Option<i64>,
    /// 開いた版の画面の書き損じ（直すまで公開できない・editor にだけ返す・D-16 の (c)）。
    pub warnings: Vec<gui::GuiValidationError>,
}

/// 公開のリクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct PublishMiniAppRequest {
    /// 公開する版（前に公開していた版を選ぶと公開を戻せる）。
    pub version: i64,
}

/// 公開の前の確かめのクエリ（version 省略時は最新の版）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct PublishPreviewQuery {
    pub version: Option<i64>,
}

/// resolve クエリ（version 省略時は公開した版。まだ公開していないアプリを editor が開くときは最新の版）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ResolveQuery {
    pub version: Option<i64>,
}

/// 本人が画面のボタンで起動した記録。
#[derive(Debug, Serialize, ToSchema)]
pub struct MiniAppRunsResponse {
    pub runs: Vec<MiniAppRun>,
}

/// 回答の本文の ```ui app を「アプリとして保存」するリクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveYuiAppRequest {
    /// 保存する図のブロックの識別子（ボタンの id の `:` より前・16 桁）。
    pub block: String,
    /// アプリの名前（一覧の名前・テナント内で一意）。
    pub name: String,
    /// 説明（画面の見出し）。空なら図の見出しを使う。
    #[serde(default)]
    pub description: Option<String>,
}

/// ミニアプリを作成する（ピン検証 → version 1）。
#[utoipa::path(
    post,
    path = "/mini-apps",
    request_body = CreateMiniAppRequest,
    responses(
        (status = 201, description = "作成した", body = MiniAppResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名が既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_mini_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateMiniAppRequest>,
) -> Result<(StatusCode, Json<MiniAppResponse>), ApiError> {
    let saved = state
        .mini_apps
        .create(
            &ctx,
            &req.name,
            &req.body,
            SaveMode::Draft,
            trace.as_deref(),
        )
        .await
        .map_err(map_gui_err)?;
    Ok((
        StatusCode::CREATED,
        Json(MiniAppResponse::saved(saved, req.body)),
    ))
}

/// ミニアプリに新バージョン（下書き）を追記する。公開した版は変えない（使う人の画面は公開するまで変わらない・D-16）。
#[utoipa::path(
    put,
    path = "/mini-apps/{id}",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    request_body = UpdateMiniAppRequest,
    responses(
        (status = 200, description = "追記した（画面の書き損じは warnings）", body = MiniAppResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "バージョン競合"),
    ),
    security(("session" = [])),
)]
pub async fn update_mini_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateMiniAppRequest>,
) -> Result<Json<MiniAppResponse>, ApiError> {
    let saved = state
        .mini_apps
        .update(
            &ctx,
            id,
            &req.body,
            req.expected_version,
            SaveMode::Draft,
            trace.as_deref(),
        )
        .await
        .map_err(map_gui_err)?;
    Ok(Json(MiniAppResponse::saved(saved, req.body)))
}

/// ミニアプリを解決する（実行画面用・部品はバンドル権限で読む）。
///
/// 任意の skill のピンが壊れていても（skill が削除された・本文が壊れている）アプリは開ける。
/// 200 で返し、使えない部品とその理由を `unavailable` に載せる（BUG-17）。
///
/// 使う人（editor でない viewer）が開けるのは公開した版だけ（D-16）。まだ公開していないアプリは 409
/// `not_published`、公開した版のほかを指定したら 409 `app_version_changed`（どちらも `published_version` つき）。
#[utoipa::path(
    get,
    path = "/mini-apps/{id}/resolved",
    params(
        ("id" = Uuid, Path, description = "ミニアプリ ID"),
        ResolveQuery,
    ),
    responses(
        (status = 200, description = "解決済みミニアプリ", body = ResolvedMiniAppResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "使う人に出していない版（code=not_published・app_version_changed）"),
    ),
    security(("session" = [])),
)]
pub async fn resolve_mini_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<ResolveQuery>,
) -> Result<Json<ResolvedMiniAppResponse>, ApiError> {
    let resolved = state
        .mini_apps
        .resolve(&ctx, id, q.version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    let body = serde_json::to_value(&resolved.body)
        .map_err(|e| ApiError::Internal(format!("mini_app body: {e}")))?;
    // 編集・共有できるか（表示の出し分けだけ）。それぞれ保存（新しい版の追記）・共有の API と同じ relation・同じ
    // 一貫性で確かめる。編集は共有で編集にした人にもボタンを出し（持ち主だけに出していた・BUG-18）、共有は
    // owner だけに出す（誰にでも出していて、owner でない人には一覧が空・付与が 403 になっていた・BUG-78）。
    let subject = ctx.subject();
    let object = ctx.ns().artifact(&id.to_string());
    let (can_edit, can_share) = tokio::try_join!(
        state.authz.check(
            &subject,
            authz::Relation::Editor,
            &object,
            authz::Consistency::HigherConsistency,
        ),
        state.authz.check(
            &subject,
            authz::Relation::Owner,
            &object,
            authz::Consistency::HigherConsistency,
        ),
    )?;
    // 下書きの番号・前に公開していた版・書き損じは、作る人（editor）にだけ返す（使う人には公開した版だけを出す）。
    let editor_only = |v| if can_edit { v } else { None };
    Ok(Json(ResolvedMiniAppResponse {
        id: resolved.id,
        version: resolved.version,
        unavailable: resolved.unavailable(),
        warnings: if can_edit {
            gui::screen_problems(&resolved.body)
        } else {
            Vec::new()
        },
        body,
        can_edit,
        can_share,
        published_version: resolved.published_version,
        published_at: resolved.published_at,
        latest_version: editor_only(Some(resolved.latest_version)),
        previous_published_version: editor_only(resolved.previous_published_version),
    }))
}

/// 公開の前の確かめ（editor）: その版といま公開している版の差と、公開を止める誤り（画面の書き損じ）。
#[utoipa::path(
    get,
    path = "/mini-apps/{id}/publish-preview",
    params(
        ("id" = Uuid, Path, description = "ミニアプリ ID"),
        PublishPreviewQuery,
    ),
    responses(
        (status = 200, description = "公開した版から変わるところと、公開を止める誤り", body = PublishPreview),
        (status = 400, description = "その版の本文が今の検証を通らない（公開できない）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（editor でない）"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn preview_mini_app_publish(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<PublishPreviewQuery>,
) -> Result<Json<PublishPreview>, ApiError> {
    let preview = state
        .mini_apps
        .publish_preview(&ctx, id, q.version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(preview))
}

/// 版を公開する（editor・D-16）。共有した人の画面とボタンの実行先が、この版に変わる。
///
/// 前に公開していた版を選ぶと公開を戻せる。画面に書き損じがある版は公開しない（400・D-16 の (c)）。
#[utoipa::path(
    post,
    path = "/mini-apps/{id}/publish",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    request_body = PublishMiniAppRequest,
    responses(
        (status = 200, description = "公開した", body = artifact::Publication),
        (status = 400, description = "画面に書き損じがある・本文が今の検証を通らない（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（editor でない）"),
        (status = 404, description = "存在しない・その版が無い"),
    ),
    security(("session" = [])),
)]
pub async fn publish_mini_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<PublishMiniAppRequest>,
) -> Result<Json<artifact::Publication>, ApiError> {
    let publication = state
        .mini_apps
        .publish(&ctx, id, req.version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(publication))
}

/// ミニアプリの画面のボタンを実行する（画面から読み直し・値の検証・冪等キー・実行者権限・監査つき）。
///
/// 画面の表（```data_grid）も同じ入口で描く: `action_id` は `data_grid:{別名}`、`params` は
/// `DataViewPage`（`cursor`・`limit` だけ）、冪等キーは不要。結果は `result.kind = "data_view"` と
/// `result.result`（`DataViewRun`）で、行・列は**閲覧者本人の権限**で毎回評価したもの（Task 15.7）。
#[utoipa::path(
    post,
    path = "/mini-apps/{id}/ui-actions",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    request_body = MiniAppActionRequest,
    responses(
        (status = 200, description = "実行した（同じ冪等キーの再送は前の結果）・表は DataViewRun", body = UiActionResponse),
        (status = 400, description = "値の検証エラー・冪等キーが無い"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "ボタンが見つからない"),
        (status = 409, description = "同じ送信が起動の途中・表の列の索引を準備中（code=index_preparing・field・progress）・開いた後に公開した版が変わった（code=app_version_changed・not_published）"),
        (status = 503, description = "実行先が無効"),
    ),
    security(("session" = [])),
)]
pub async fn invoke_mini_app_action(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<MiniAppActionRequest>,
) -> Result<Json<UiActionResponse>, ApiError> {
    let result = state
        .mini_apps
        .invoke_yui_action(&ctx, &state.ui_actions, id, req, trace.as_deref())
        .await
        .map_err(map_yui_action_err)?;
    Ok(Json(UiActionResponse { result }))
}

/// 本人がミニアプリの画面のボタンで起動した記録（新しい 20 件を古い順に）。
///
/// 状態は run の今の状態（待機中は `waiting` と `waiting_for`）。失敗した run には利用者向けの理由
/// （`failure`）、ワークフローが画面に返した結果（`reply`）は未確定の run と最新の終わった 1 件にだけ付ける
/// （他は `has_reply` を見て `/mini-apps/{id}/runs/{run_id}` で読む）。
#[utoipa::path(
    get,
    path = "/mini-apps/{id}/runs",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    responses(
        (status = 200, description = "起動の記録", body = MiniAppRunsResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_mini_app_runs(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<MiniAppRunsResponse>, ApiError> {
    let runs = state
        .mini_apps
        .my_runs(&ctx, id, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(MiniAppRunsResponse { runs }))
}

/// 本人が起動した 1 件（ワークフローが画面に返した結果を必ず付ける・GAP-09）。
///
/// 一覧（`/mini-apps/{id}/runs`）は応答を小さく保つため、結果を未確定の run と最新の終わった 1 件にだけ付ける。
/// 古い行を開いたときはこれで読む。関門は一覧と同じ（アプリの viewer ∧ 押した本人 ∧ org、エンジン側でも
/// 本人が起動した interactive の run だけ）。他人の run は存在を区別せず 404。
#[utoipa::path(
    get,
    path = "/mini-apps/{id}/runs/{run_id}",
    params(
        ("id" = Uuid, Path, description = "ミニアプリ ID"),
        ("run_id" = Uuid, Path, description = "起動した run の ID"),
    ),
    responses(
        (status = 200, description = "起動の記録と結果", body = MiniAppRun),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない・本人が起動した run ではない"),
    ),
    security(("session" = [])),
)]
pub async fn get_mini_app_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MiniAppRun>, ApiError> {
    let run = state
        .mini_apps
        .my_run(&ctx, id, run_id, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(run))
}

/// 回答の本文の ```ui app を、そのボタンの実行先ごとミニアプリとして保存する。
///
/// 会話を読める人が使える。実行先は回答の確定時に解決した束縛で、保存する人の権限で
/// ピンを検証し直す（見えないワークフローは束ねられない）。AI が書いた画面なので、書き損じた図も誤りとして
/// 止める（[`SaveMode::Strict`]・D-16 の (c)）。保存したアプリはまだ公開していない（D-16）。
#[utoipa::path(
    post,
    path = "/threads/{thread_id}/messages/{message_id}/save-as-app",
    params(
        ("thread_id" = Uuid, Path, description = "スレッド ID"),
        ("message_id" = Uuid, Path, description = "メッセージ ID"),
    ),
    request_body = SaveYuiAppRequest,
    responses(
        (status = 201, description = "保存した", body = MiniAppResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "会話を読めない"),
        (status = 404, description = "図が見つからない"),
        (status = 409, description = "同名が既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn save_yui_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((thread_id, message_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SaveYuiAppRequest>,
) -> Result<(StatusCode, Json<MiniAppResponse>), ApiError> {
    let chat = state
        .chat
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("チャットが無効です".into()))?;
    let draft = chat
        .yui_app_for_saving(&ctx, thread_id, message_id, &req.block, trace.as_deref())
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    let description = req
        .description
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| draft.title.clone());
    let body = serde_json::json!({
        "description": description,
        // 会話の図は ` ```ui:app ` と見出しだけで書かれることもあるので、種類をフェンスに残す。
        "screen": format!("```ui:app\n{}\n```\n", draft.source.trim_end()),
        "workflows": draft.workflows.iter().map(|(alias, id, version)| serde_json::json!({
            "alias": alias,
            "artifact_id": id,
            "version": version,
        })).collect::<Vec<_>>(),
    });
    let saved = state
        .mini_apps
        .create(
            &ctx,
            req.name.trim(),
            &body,
            SaveMode::Strict,
            trace.as_deref(),
        )
        .await
        .map_err(map_gui_err)?;
    Ok((
        StatusCode::CREATED,
        Json(MiniAppResponse::saved(saved, body)),
    ))
}
