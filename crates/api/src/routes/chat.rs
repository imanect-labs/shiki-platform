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
    // skill / ミニアプリの選択を**作成者の権限**で解決し、version 込みでピンする（Task 6.7/6.10・
    // #344 で複数化）。ミニアプリ指定時は skill ピンをバンドル定義から引く（個別指定より優先）。
    let skill_pins: Vec<chat::SkillPin>;
    let mut mini_app_pin: Option<(Uuid, i64)> = None;
    if let Some(pin) = req.mini_app {
        let resolved = state
            .mini_apps
            .resolve(&ctx, pin.artifact_id, pin.version, trace.as_deref())
            .await
            .map_err(super::gui_errors::map_gui_err)?;
        mini_app_pin = Some((resolved.id, resolved.version));
        skill_pins = resolved
            .body
            .skill
            .map(|p| {
                vec![chat::SkillPin {
                    skill_id: p.artifact_id,
                    skill_version: p.version,
                    // バンドル同梱の skill はコマンド起動ではない。
                    command_args: None,
                }]
            })
            .unwrap_or_default();
    } else {
        // `skills`（複数）が正・`skill`（単数）は後方互換の別名（併用時は skills 優先）。
        let requested = match (&req.skills, req.skill) {
            (Some(list), _) => list.clone(),
            (None, Some(single)) => vec![single],
            (None, None) => Vec::new(),
        };
        // スレッド作成時のピンは「最初からロード済み」の宣言であり起動ではない（`None`）。
        skill_pins = super::chat_skills::resolve_skill_pins(
            &state,
            &ctx,
            &requested,
            None,
            trace.as_deref(),
        )
        .await?;
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

    // 由来ノートは**発話ユーザーの viewer 権限**で解決する（見えないノートに紐づけられない・
    // 表示名は解決時点の name を非正規化して持つ・issue #282）。fail-closed。
    let origin = if let Some(note_id) = req.origin_note_id {
        let node = state
            .storage
            .get_metadata(&ctx, note_id, trace.as_deref())
            .await?;
        Some(chat::ThreadOrigin {
            note_id,
            note_name: node.name,
        })
    } else {
        None
    };

    let store = chat_store(&state)?;
    let mut thread = store
        .create_thread(
            &ctx,
            req.title.as_deref().unwrap_or(""),
            req.agent_mode.unwrap_or(false),
            origin,
            trace.as_deref(),
        )
        .await?;
    if !skill_pins.is_empty() || mini_app_pin.is_some() {
        store
            .set_thread_pins(&ctx, thread.id, &skill_pins, mini_app_pin, trace.as_deref())
            .await?;
        thread.skill_pins = skill_pins;
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

/// スレッド一覧の検索語の上限（文字数）。タイトル検索に十分で、巨大なパターンを DB へ流さない。
const MAX_THREAD_QUERY_CHARS: usize = 200;

/// 自分のスレッド一覧（更新日降順・keyset ページング）。
#[utoipa::path(
    get, path = "/threads",
    params(
        ("cursor" = Option<String>, Query, description = "続きのカーソル"),
        ("limit" = Option<i64>, Query, description = "件数（既定 30・上限 100）"),
        ("origin_note_id" = Option<Uuid>, Query, description = "由来ノートで絞り込む（ノート側の会話一覧・#282）"),
        ("q" = Option<String>, Query, description = "タイトルの部分一致（大小無視・200 文字まで・#591）"),
        ("pinned" = Option<bool>, Query, description = "true ならピン留めだけ（新しい順・ページングなし）"),
    ),
    responses(
        (status = 200, description = "スレッド一覧", body = ThreadListResponse),
        (status = 400, description = "検索語が長すぎる"),
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
    if q.pinned == Some(true) {
        let threads = chat_store(&state)?.list_pinned_threads(&ctx).await?;
        return Ok(Json(ThreadListResponse {
            threads,
            next_cursor: None,
        }));
    }
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    if q.q
        .as_deref()
        .is_some_and(|s| s.chars().count() > MAX_THREAD_QUERY_CHARS)
    {
        return Err(ApiError::BadRequest(format!(
            "検索語は {MAX_THREAD_QUERY_CHARS} 文字以内で指定してください"
        )));
    }
    let (before_ts, before_id) = match &q.cursor {
        Some(c) => decode_cursor(c)?,
        None => (None, None),
    };
    let threads = chat_store(&state)?
        .list_threads(
            &ctx,
            before_ts,
            before_id,
            q.origin_note_id,
            q.q.as_deref(),
            limit,
        )
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
    // 進行中（非端末）の run があれば id と自律フラグを返す（再訪時の承認送信・進捗復元・
    // 自律 UI（モードセレクタ）の復元に使う・#350）。
    let active = store
        .current_run(id, &ctx.tenant_id)
        .await?
        .filter(|r| !r.status.is_terminal());
    // 順番待ちの発話（生成が始まっていない run の発話）。認可は上の get_messages が済ませている。
    // 進行中 run が無ければ順番待ちも存在し得ない（先頭が走り出しているはず）ので引かない。
    let queued_runs = if let Some(current) = active {
        store
            .queued_user_messages(id, &ctx.tenant_id, current.run_id)
            .await?
            .into_iter()
            .map(|(user_message_id, run_id)| super::chat_dto::QueuedRun {
                user_message_id,
                run_id,
            })
            .collect()
    } else {
        Vec::new()
    };
    let context_usage = store.latest_context_usage(id, &ctx.tenant_id).await?;
    Ok(Json(MessagesResponse {
        context_usage,
        messages,
        active_run_id: active.map(|r| r.run_id),
        active_run_autonomous: active.map(|r| r.autonomous),
        active_assistant_message_id: active.map(|r| r.message_id),
        queued_runs,
    }))
}

/// 発話を送信する（**単一 TX で保存＋生成ジョブ投入**して 202・同期実行しない）。
#[utoipa::path(
    post, path = "/threads/{id}/messages",
    params(("id" = Uuid, Path, description = "スレッド ID")),
    request_body = PostMessageRequest,
    responses(
        (status = 202, description = "受理（生成は接続非依存ジョブで継続）", body = PostMessageResponse),
        (status = 400, description = "空メッセージ・組織で使えないモデル・未知のツール名／指定できないツール"),
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
    // この発話にだけ使ってほしいツール（#734）。指定できる集合での照合・重複除去（400）。
    // 誘導のみで権限ではないため、ここでは認可を引かない（提示はワーカーのプロファイルが決める）。
    let requested_tools = super::chat_dto::parse_requested_tools(req.tools.as_deref())?;
    let attachments = req.attachments.unwrap_or_default();
    // 選択コンテキストの受理検証（fail-closed・存在秘匿・chat_selection.rs 参照）。
    let selection =
        super::chat_selection::resolve_selection(&state, &ctx, req.context, trace.as_deref())
            .await?;
    // この発話にだけ適用する skill（スラッシュコマンド起動・#387）。**発話者の権限で解決**し、
    // 読めなければここで落ちる（fail-closed）。thread のピンは変えない＝次の発話へ持ち越さない。
    let once_skills = match &req.skills {
        Some(pins) if !pins.is_empty() => {
            super::chat_skills::resolve_skill_pins(
                &state,
                &ctx,
                pins,
                Some(req.text.as_str()),
                trace.as_deref(),
            )
            .await?
        }
        _ => Vec::new(),
    };
    let model = resolve_run_model(&state, &ctx, req.model.as_deref(), req.effort).await?;
    let r = chat_store(&state)?
        .post_message_with_model(
            &ctx,
            id,
            &req.text,
            &attachments,
            selection,
            req.agent_mode,
            req.autonomous.unwrap_or(false),
            &once_skills,
            model.as_ref(),
            &requested_tools,
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

/// 発話のモデル指定をテナント設定で解決する（#553）。組織で使えないモデルは 400。
///
/// LLM ゲートウェイが無い構成（テスト・未構成）では、指定が無ければ従来どおり run にモデルを
/// 載せない（ワーカーの既定で生成）。指定があるのに解決できないなら 503。
async fn resolve_run_model(
    state: &AppState,
    ctx: &authz::AuthContext,
    model: Option<&str>,
    effort: Option<super::llm_models::EffortLevel>,
) -> Result<Option<chat::RunModel>, ApiError> {
    let Some(gw) = state.llm.as_ref() else {
        if model.is_some() || effort.is_some() {
            return Err(ApiError::ServiceUnavailable(
                "llm gateway is not configured".into(),
            ));
        }
        return Ok(None);
    };
    let choice = llm_gateway::ModelChoice {
        model: model.map(str::to_string),
        effort: effort.map(Into::into),
    };
    match gw
        .resolve_choice(ctx, &choice)
        .await
        .map_err(|e| ApiError::Internal(format!("tenant llm settings: {e}")))?
    {
        Ok(r) => Ok(Some(chat::RunModel {
            model: r.model,
            effort: r.effort,
            auto: r.auto,
        })),
        Err(e) => Err(ApiError::UnprocessableJson(serde_json::json!({
            "status": 400,
            "title": "Bad Request",
            "detail": e.to_string(),
        }))),
    }
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
    // viewer 認可（403/404）。引用とツール結果は viewer_event_stream が閲覧者の権限で見直す（#513）。
    chat.get_thread(&ctx, id, trace.as_deref()).await?;

    let from_seq = last_event_id(&headers, &q);
    // いま映すべき run（＝最も古い未完了 run）を購読する。run が無ければ即終了の空ストリーム。
    // 順番待ちの run を購読した場合はイベント 0 件で待機し、ワーカーが着手した時点で流れ出す。
    let stream: SseEventStream = match chat.current_run(id, &ctx.tenant_id).await? {
        Some(current) => chat
            .viewer_event_stream(&ctx, id, current.run_id, from_seq, trace.as_deref())
            .await?
            .map(|ev| {
                let data = serde_json::to_string(&ev.event).unwrap_or_else(|_| "{}".to_string());
                Ok(Event::default().id(ev.seq.to_string()).data(data))
            })
            .boxed(),
        None => futures::stream::empty().boxed(),
    };
    // 逆プロキシのバッファ/圧縮で承認待ち・進捗イベントが live で止まるのを防ぐ（sse_util 参照）。
    Ok(super::sse_util::no_buffer(
        Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response(),
    ))
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
