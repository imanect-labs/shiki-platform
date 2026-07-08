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
use chat::{Attachment, ChatStore, Message, Thread, ThreadRole};
use chrono::{DateTime, Utc};
use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use storage::ShareTarget;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// SSE イベントストリーム（run のイベントを Event へ写した無限/有限ストリーム）。
type SseEventStream = BoxStream<'static, Result<Event, Infallible>>;

/// keyset カーソル（更新日時＋id）。
type Cursor = (Option<DateTime<Utc>>, Option<Uuid>);

/// AppState からチャットストアを取り出す（無効なら 503）。
pub(super) fn chat_store(state: &AppState) -> Result<&ChatStore, ApiError> {
    state
        .chat
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("chat.enabled=false".into()))
}

// ── DTO ─────────────────────────────────────────────────────────────

/// スレッド作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateThreadRequest {
    #[serde(default)]
    pub title: Option<String>,
    /// エージェントモード既定（既定 false＝通常チャット）。
    #[serde(default)]
    pub agent_mode: Option<bool>,
}

/// スレッド一覧レスポンス（keyset ページング）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadListResponse {
    pub threads: Vec<Thread>,
    pub next_cursor: Option<String>,
}

/// メッセージ一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct MessagesResponse {
    pub messages: Vec<Message>,
}

/// 発話送信リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct PostMessageRequest {
    pub text: String,
    #[serde(default)]
    pub attachments: Option<Vec<Attachment>>,
    /// このメッセージのエージェントモード上書き（未指定はスレッド既定）。
    #[serde(default)]
    pub agent_mode: Option<bool>,
}

/// 発話送信レスポンス（202・生成は接続非依存ジョブで継続）。
#[derive(Debug, Serialize, ToSchema)]
pub struct PostMessageResponse {
    pub run_id: Uuid,
    pub user_message_id: Uuid,
    pub assistant_message_id: Uuid,
    pub agent_mode: bool,
}

/// 共有/解除リクエスト（viewer/commenter/editor）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ShareThreadRequest {
    pub target: ShareTarget,
    pub role: ThreadRole,
}

/// 共有相手 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadShareEntry {
    pub target: ShareTarget,
    pub role: ThreadRole,
}

/// 共有相手一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadSharesResponse {
    pub shares: Vec<ThreadShareEntry>,
}

/// 一覧クエリ。
#[derive(Debug, Deserialize)]
pub struct ListThreadsQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// SSE クエリ（Last-Event-ID の代替）。
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    pub last_event_id: Option<i64>,
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
    let thread = chat_store(&state)?
        .create_thread(
            &ctx,
            req.title.as_deref().unwrap_or(""),
            req.agent_mode.unwrap_or(false),
            trace.as_deref(),
        )
        .await?;
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
    let messages = chat_store(&state)?
        .get_messages(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(MessagesResponse { messages }))
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
