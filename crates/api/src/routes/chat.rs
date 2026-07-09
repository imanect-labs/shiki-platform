//! チャット API（Task 3.5 / 3.7）。thread/message CRUD・**接続非依存生成**（202＋SSE）・共有。
//!
//! ハンドラは薄く、実体は `chat::ChatStore`（authz・監査・generation_run/event のチョークポイント）。
//! DTO は chat 側のドメイン型（`Thread`/`Message`/`ContentBlock`/`StreamEventKind`）をそのまま
//! OpenAPI へ流し、フロント `chat-api.ts` と同型に保つ（手書きミラー禁止・codegen が正）。

use std::convert::Infallible;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use base64::Engine as _;
use chat::{ChatStore, Thread};
use chrono::{DateTime, Utc};
use futures::stream::{BoxStream, StreamExt};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::chat_dto::Cursor;
pub use super::chat_dto::{
    ArtifactPinRequest, CreateThreadRequest, ListThreadsQuery, MessagesResponse,
    PostMessageRequest, PostMessageResponse, ShareThreadRequest, StreamQuery, ThreadListResponse,
    ThreadShareEntry, ThreadSharesResponse, WorkspaceChoiceRequest,
};

/// SSE イベントストリーム（run のイベントを Event へ写した無限/有限ストリーム）。
type SseEventStream = BoxStream<'static, Result<Event, Infallible>>;

/// AppState からチャットストアを取り出す（無効なら 503）。
pub(super) fn chat_store(state: &AppState) -> Result<&ChatStore, ApiError> {
    state
        .chat
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("chat.enabled=false".into()))
}

// ── ハンドラ ─────────────────────────────────────────────────────────

/// スレッドを新規作成する。
#[utoipa::path(
    post, path = "/threads", request_body = CreateThreadRequest,
    responses(
        (status = 200, description = "作成したスレッド", body = Thread),
        (status = 401, description = "未認証"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn create_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateThreadRequest>,
) -> Result<Json<Thread>, ApiError> {
    // skill / ミニアプリの選択を**作成者の権限**で解決し、version 込みでピンする（Task 6.7/6.10）。
    // ミニアプリ指定時は skill ピンをバンドル定義から引く（skill 指定より優先）。
    let mut skill_pin: Option<(Uuid, i64)> = None;
    let mut mini_app_pin: Option<(Uuid, i64)> = None;
    if let Some(pin) = req.mini_app {
        let resolved = state
            .mini_apps
            .resolve(&ctx, pin.artifact_id, pin.version, trace.as_deref())
            .await
            .map_err(super::ui_specs::map_gui_err)?;
        mini_app_pin = Some((resolved.id, resolved.version));
        skill_pin = resolved.body.skill.map(|p| (p.artifact_id, p.version));
    } else if let Some(pin) = req.skill {
        let (version, _body, _raw) = match pin.version {
            Some(v) => {
                state
                    .skills
                    .get_version(&ctx, pin.artifact_id, v, trace.as_deref())
                    .await
            }
            None => {
                state
                    .skills
                    .get_latest(&ctx, pin.artifact_id, trace.as_deref())
                    .await
            }
        }
        .map_err(super::ui_specs::map_gui_err)?;
        skill_pin = Some((pin.artifact_id, version));
    }

    // ワークスペース場所は **thread 作成前に**認可検証する（不正なら孤児 thread を残さない）。
    // existing（既存フォルダをそのままワークスペースに）はそのフォルダの内容がスレッド共有相手へ
    // 波及するため **Owner** を要求し、new_under（配下に隔離フォルダを新規作成）は **Editor** で足りる。
    let workspace = if let Some(choice) = req.workspace {
        let (folder, parent, target, relation) = match choice {
            WorkspaceChoiceRequest::Existing { folder_id } => {
                (Some(folder_id), None, folder_id, authz::Relation::Owner)
            }
            WorkspaceChoiceRequest::NewUnder { folder_id } => {
                (None, Some(folder_id), folder_id, authz::Relation::Editor)
            }
        };
        state
            .storage
            .require_folder_access(&ctx, target, relation, trace.as_deref())
            .await?;
        Some((folder, parent))
    } else {
        None
    };

    let store = chat_store(&state)?;
    let mut thread = store
        .create_thread(
            &ctx,
            req.title.as_deref().unwrap_or(""),
            req.agent_mode.unwrap_or(false),
            trace.as_deref(),
        )
        .await?;
    if skill_pin.is_some() || mini_app_pin.is_some() {
        store
            .set_thread_pins(&ctx, thread.id, skill_pin, mini_app_pin, trace.as_deref())
            .await?;
        thread.skill_id = skill_pin.map(|(id, _)| id);
        thread.skill_version = skill_pin.map(|(_, v)| v);
        thread.mini_app_id = mini_app_pin.map(|(id, _)| id);
        thread.mini_app_version = mini_app_pin.map(|(_, v)| v);
    }
    if let Some((folder, parent)) = workspace {
        store
            .set_thread_workspace(thread.id, &ctx.tenant_id, folder, parent)
            .await?;
    }
    Ok(Json(thread))
}

/// 自分のスレッド一覧（更新日降順・keyset ページング）。
#[utoipa::path(
    get, path = "/threads",
    params(
        ("cursor" = Option<String>, Query, description = "続きのカーソル"),
        ("limit" = Option<i64>, Query, description = "件数（既定 30・上限 100）"),
    ),
    responses(
        (status = 200, description = "スレッド一覧", body = ThreadListResponse),
        (status = 401, description = "未認証"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn list_threads(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<ListThreadsQuery>,
) -> Result<Json<ThreadListResponse>, ApiError> {
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let (before_ts, before_id) = match &q.cursor {
        Some(c) => decode_cursor(c)?,
        None => (None, None),
    };
    let threads = chat_store(&state)?
        .list_threads(&ctx, before_ts, before_id, limit)
        .await?;
    let next_cursor = if threads.len() as i64 == limit {
        threads.last().map(|t| encode_cursor(t.updated_at, t.id))
    } else {
        None
    };
    Ok(Json(ThreadListResponse {
        threads,
        next_cursor,
    }))
}

/// スレッドを取得する（viewer 認可）。
#[utoipa::path(
    get, path = "/threads/{id}",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 200, description = "スレッド", body = Thread),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Thread>, ApiError> {
    Ok(Json(
        chat_store(&state)?
            .get_thread(&ctx, id, trace.as_deref())
            .await?,
    ))
}

/// メッセージを線形取得する（viewer 認可・引用は閲覧者権限で再評価）。
#[utoipa::path(
    get, path = "/threads/{id}/messages",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 200, description = "メッセージ一覧", body = MessagesResponse),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_messages(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<MessagesResponse>, ApiError> {
    let store = chat_store(&state)?;
    let messages = store.get_messages(&ctx, id, trace.as_deref()).await?;
    // 進行中（非端末）の run があれば id を返す（再訪時の承認送信・進捗復元に使う）。
    let active_run_id = store
        .latest_run(id, &ctx.tenant_id)
        .await?
        .filter(|(_, status)| !status.is_terminal())
        .map(|(run_id, _)| run_id);
    Ok(Json(MessagesResponse {
        messages,
        active_run_id,
    }))
}

/// 発話を送信する（**単一 TX で保存＋生成ジョブ投入**して 202・同期実行しない）。
#[utoipa::path(
    post, path = "/threads/{id}/messages",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    request_body = PostMessageRequest,
    responses(
        (status = 202, description = "受理（生成は接続非依存ジョブで継続）", body = PostMessageResponse),
        (status = 400, description = "空メッセージ"),
        (status = 403, description = "投稿権限なし"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn post_message(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<PostMessageRequest>,
) -> Result<(StatusCode, Json<PostMessageResponse>), ApiError> {
    let attachments = req.attachments.unwrap_or_default();
    let r = chat_store(&state)?
        .post_message(
            &ctx,
            id,
            &req.text,
            &attachments,
            req.agent_mode,
            req.autonomous.unwrap_or(false),
            trace.as_deref(),
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(PostMessageResponse {
            run_id: r.run_id,
            user_message_id: r.user_message_id,
            assistant_message_id: r.assistant_message_id,
            agent_mode: r.agent_mode,
        }),
    ))
}

/// スレッドの最新 run を SSE で購読する（replay-then-subscribe）。
///
/// `Last-Event-ID`（ヘッダ）または `?last_event_id=` で再接続時に途中から再開する（重複しない）。
/// 各イベントは `id: <seq>` を付けて `StreamEventKind` の JSON を data に載せる。
#[utoipa::path(
    get, path = "/threads/{id}/stream",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 200, description = "SSE イベントストリーム（text/event-stream）"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn stream_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let chat = chat_store(&state)?;
    // viewer 認可（403/404）。共有スレッドの引用は get_messages 側で閲覧者権限に再評価される。
    chat.get_thread(&ctx, id, trace.as_deref()).await?;

    let from_seq = last_event_id(&headers, &q);
    // 最新 run のイベントを購読する。run が無ければ即終了の空ストリーム。
    let stream: SseEventStream = match chat.latest_run(id, &ctx.tenant_id).await? {
        Some((run_id, _status)) => chat
            .event_stream(run_id, from_seq)
            .map(|ev| {
                let data = serde_json::to_string(&ev.event).unwrap_or_else(|_| "{}".to_string());
                Ok(Event::default().id(ev.seq.to_string()).data(data))
            })
            .boxed(),
        None => futures::stream::empty().boxed(),
    };
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// 生成をユーザー明示停止する（editor 認可）。ページ離脱はキャンセルしない。
#[utoipa::path(
    post, path = "/threads/{id}/runs/{run_id}/cancel",
    params(
        ("id" = Uuid, Path, description = "スレッド ID"),
        ("run_id" = Uuid, Path, description = "run ID"),
    ),
    responses(
        (status = 204, description = "キャンセル要求を受理"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn cancel_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .request_cancel(&ctx, id, run_id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// スレッドを共有する（owner 権限）。
#[utoipa::path(
    post, path = "/threads/{id}/shares",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    request_body = ShareThreadRequest,
    responses(
        (status = 204, description = "共有を付与"),
        (status = 403, description = "owner でない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn share_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ShareThreadRequest>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .share_thread(&ctx, id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 共有を解除する（owner 権限・冪等）。
#[utoipa::path(
    delete, path = "/threads/{id}/shares",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    request_body = ShareThreadRequest,
    responses(
        (status = 204, description = "共有を解除"),
        (status = 403, description = "owner でない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn unshare_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ShareThreadRequest>,
) -> Result<StatusCode, ApiError> {
    chat_store(&state)?
        .unshare_thread(&ctx, id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 共有相手一覧（owner 権限）。
#[utoipa::path(
    get, path = "/threads/{id}/shares",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    responses(
        (status = 200, description = "共有相手一覧", body = ThreadSharesResponse),
        (status = 403, description = "owner でない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn list_thread_shares(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ThreadSharesResponse>, ApiError> {
    let entries = chat_store(&state)?
        .list_thread_shares(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(ThreadSharesResponse {
        shares: entries
            .into_iter()
            .map(|(target, role)| ThreadShareEntry { target, role })
            .collect(),
    }))
}

// ── ヘルパ ───────────────────────────────────────────────────────────

/// `Last-Event-ID`（ヘッダ優先）または `?last_event_id=` から再開 seq を得る（既定 0）。
fn last_event_id(headers: &HeaderMap, q: &StreamQuery) -> i64 {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .or(q.last_event_id)
        .unwrap_or(0)
}

/// keyset カーソルを `base64(updated_at_rfc3339|uuid)` へ符号化する。
fn encode_cursor(updated_at: DateTime<Utc>, id: Uuid) -> String {
    let raw = format!("{}|{}", updated_at.to_rfc3339(), id);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// keyset カーソルを復号する（不正なら 400）。
fn decode_cursor(cursor: &str) -> Result<Cursor, ApiError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| ApiError::BadRequest("不正なカーソル".into()))?;
    let raw =
        String::from_utf8(bytes).map_err(|_| ApiError::BadRequest("不正なカーソル".into()))?;
    let (ts, id) = raw
        .split_once('|')
        .ok_or_else(|| ApiError::BadRequest("不正なカーソル".into()))?;
    let ts = DateTime::parse_from_rfc3339(ts)
        .map_err(|_| ApiError::BadRequest("不正なカーソル".into()))?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id).map_err(|_| ApiError::BadRequest("不正なカーソル".into()))?;
    Ok((Some(ts), Some(id)))
}
