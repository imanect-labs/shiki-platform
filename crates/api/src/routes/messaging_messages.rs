//! 職員間メッセージ API — 発言・スレッド・リアクション・検索・メンション・配信（Task 14.3〜14.7）。
//!
//! 配信（`GET /messaging/channels/{id}/stream`）は SSE。合図（[`messaging::pubsub`]）を受けたら
//! **購読者の `AuthContext` で DB を読み直して**送る（合図は本文を運ばない）。権限は剥奪（`leave`）の
//! 合図を受けたら即座に引き直し、合図を取りこぼしても [`RECHECK`] ごとに引き直す（上限 TTL・PIT-64）。
//! 読めなくなったら `leave` を送って閉じる。

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json,
};
use chat::model::ContentBlock;
use chrono::{DateTime, Utc};
use messaging::pubsub::{Signal, Subscription};
use messaging::{
    AgentReplyView, MentionNotice, MessageView, MessagingService, NewMessage, ReplyMode, SearchHit,
};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::messaging::{error, service};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

/// `keepalive` イベントの間隔。
const KEEPALIVE: Duration = Duration::from_secs(15);
/// 読み直しで遡る幅（コミット順と `updated_at` の前後を吸収する）。
const OVERLAP: chrono::TimeDelta = chrono::TimeDelta::seconds(10);
/// 合図が来なくても DB を読み直す間隔（Redis が落ちたときの保険）。
const POLL: Duration = Duration::from_secs(10);
/// 購読の権限を引き直す上限 TTL（leave の合図を取りこぼしても、この時間で閉じる・PIT-64）。
pub const RECHECK: Duration = messaging::stream::GRANT_TTL;

pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/messaging/channels/{id}/messages",
            &["GET", "POST"],
            AccessPolicy::Session,
            || get(list_messages).post(post_message),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/threads/{parent_id}",
            &["GET"],
            AccessPolicy::Session,
            || get(get_thread),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/stream",
            &["GET"],
            AccessPolicy::SessionStreaming,
            || get(stream_channel),
        ),
        RouteDecl::new(
            "/messaging/channels/{id}/agent-replies",
            &["GET"],
            AccessPolicy::Session,
            || get(list_agent_replies),
        ),
        RouteDecl::new(
            "/messaging/messages/{id}",
            &["GET", "PATCH", "DELETE"],
            AccessPolicy::Session,
            || get(get_message).patch(edit_message).delete(delete_message),
        ),
        RouteDecl::new(
            "/messaging/messages/{id}/ui-actions",
            &["POST"],
            AccessPolicy::Session,
            || post(invoke_ui_action),
        ),
        RouteDecl::new(
            "/messaging/messages/{id}/reactions",
            &["POST"],
            AccessPolicy::Session,
            || post(react),
        ),
        RouteDecl::new("/messaging/search", &["GET"], AccessPolicy::Session, || {
            get(search)
        }),
        RouteDecl::new(
            "/messaging/mentions",
            &["GET"],
            AccessPolicy::Session,
            || get(list_mentions),
        ),
        RouteDecl::new(
            "/messaging/mentions/read",
            &["POST"],
            AccessPolicy::Session,
            || post(read_mentions),
        ),
    ]
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MessagesQuery {
    /// これより前の発言（ページング・省略時は最新から）。
    pub before: Option<DateTime<Utc>>,
    /// 件数（既定 50・最大 200）。
    pub limit: Option<i64>,
}

/// 最上位の発言（新しい順）。
#[utoipa::path(get, path = "/messaging/channels/{id}/messages", params(("id" = Uuid, Path), MessagesQuery),
    responses((status = 200, description = "成功", body = Vec<MessageView>), (status = 404, description = "無い・読めない")),
    security(("session" = [])))]
pub async fn list_messages(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
    Query(q): Query<MessagesQuery>,
) -> Result<Json<Vec<MessageView>>, ApiError> {
    Ok(Json(
        service(&state)?
            .list_messages(&ctx, id, q.before, q.limit.unwrap_or(50))
            .await
            .map_err(error)?,
    ))
}

/// 投稿する。自分のエージェントへのメンションを含むと、投稿の後にエージェントが答える。
#[utoipa::path(post, path = "/messaging/channels/{id}/messages", params(("id" = Uuid, Path)), request_body = NewMessage,
    responses((status = 200, description = "投稿した", body = MessageView), (status = 400, description = "本文が不正"),
        (status = 403, description = "投稿できない"), (status = 404, description = "無い・読めない")),
    security(("session" = [])))]
pub async fn post_message(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(input): Json<NewMessage>,
) -> Result<Json<MessageView>, ApiError> {
    let svc = service(&state)?.clone();
    let private = input.agent_private;
    let view = svc
        .post(&ctx, id, input, trace.0.as_deref())
        .await
        .map_err(error)?;
    if messaging::mentions_agent(&view.blocks) {
        spawn_agent(&state, &svc, ctx, view.id, private, trace.0);
    }
    Ok(Json(view))
}

/// エージェントの回答の図（```ui）のボタンを押す。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UiActionBody {
    /// ボタンの id（`{ブロックの識別子}:{a1}`）。
    pub action_id: String,
    /// 図の入力部品の値（部品の名前 → 値）。サーバーが保存済みの図で検証する。
    #[serde(default)]
    #[schema(value_type = Object)]
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// 自分のエージェントの回答の図のボタン（`-> chat`）を押す。押した内容を自分の発言としてスレッドに書き、
/// エージェントが続けて答える（通常の投稿と同じ経路・権限は増えない）。
#[utoipa::path(post, path = "/messaging/messages/{id}/ui-actions", params(("id" = Uuid, Path)), request_body = UiActionBody,
    responses((status = 200, description = "スレッドに書いた発言", body = MessageView),
        (status = 400, description = "値が不正・ワークフローのボタン"),
        (status = 403, description = "自分のエージェントの回答ではない・投稿できない"),
        (status = 404, description = "回答・ボタンが無い")),
    security(("session" = [])))]
pub async fn invoke_ui_action(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(body): Json<UiActionBody>,
) -> Result<Json<MessageView>, ApiError> {
    let svc = service(&state)?.clone();
    let (channel, input) = svc
        .ui_action_reply(&ctx, id, &body.action_id, &body.values)
        .await
        .map_err(error)?;
    let view = svc
        .post(&ctx, channel, input, trace.0.as_deref())
        .await
        .map_err(error)?;
    spawn_agent(&state, &svc, ctx, view.id, false, trace.0);
    Ok(Json(view))
}

/// 自分のエージェントを呼ぶ（照会者の `AuthContext` のまま・応答は非同期）。
fn spawn_agent(
    state: &AppState,
    svc: &Arc<MessagingService>,
    ctx: authz::AuthContext,
    trigger: Uuid,
    private: bool,
    trace: Option<String>,
) {
    let mode = if private {
        ReplyMode::Private
    } else {
        ReplyMode::Public
    };
    let svc = Arc::clone(svc);
    let runtime = state.messaging_agent.clone();
    tokio::spawn(async move {
        let result = match runtime {
            Some(rt) => {
                svc.invoke_agent(&ctx, &rt, trigger, mode, trace.as_deref())
                    .await
            }
            None => Err(messaging::Error::Unavailable("AI が構成されていません")),
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "messaging: エージェントが応答できませんでした");
            svc.agent_failed(&ctx, trigger, &e).await;
        }
    });
}

/// スレッド（親＋返信）。
#[utoipa::path(get, path = "/messaging/channels/{id}/threads/{parent_id}",
    params(("id" = Uuid, Path), ("parent_id" = Uuid, Path)),
    responses((status = 200, description = "成功", body = Vec<MessageView>), (status = 404, description = "無い・読めない")),
    security(("session" = [])))]
pub async fn get_thread(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path((id, parent)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<MessageView>>, ApiError> {
    Ok(Json(
        service(&state)?
            .thread(&ctx, id, parent)
            .await
            .map_err(error)?,
    ))
}

/// 1 件。
#[utoipa::path(get, path = "/messaging/messages/{id}", params(("id" = Uuid, Path)),
    responses((status = 200, description = "成功", body = MessageView), (status = 404, description = "無い・読めない")),
    security(("session" = [])))]
pub async fn get_message(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
) -> Result<Json<MessageView>, ApiError> {
    Ok(Json(
        service(&state)?.message(&ctx, id).await.map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EditBody {
    pub blocks: Vec<ContentBlock>,
}

/// 編集する（本人だけ）。
#[utoipa::path(patch, path = "/messaging/messages/{id}", params(("id" = Uuid, Path)), request_body = EditBody,
    responses((status = 200, description = "成功", body = MessageView), (status = 403, description = "本人ではない")),
    security(("session" = [])))]
pub async fn edit_message(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(body): Json<EditBody>,
) -> Result<Json<MessageView>, ApiError> {
    Ok(Json(
        service(&state)?
            .edit(&ctx, id, body.blocks, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

/// 削除する（本人とチャンネルの管理者・論理削除）。
#[utoipa::path(delete, path = "/messaging/messages/{id}", params(("id" = Uuid, Path)),
    responses((status = 204, description = "削除した"), (status = 403, description = "削除できない")),
    security(("session" = [])))]
pub async fn delete_message(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    service(&state)?
        .delete(&ctx, id, trace.0.as_deref())
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReactBody {
    /// Unicode 絵文字 1 つ。
    pub emoji: String,
    /// 付ける（true）・外す（false）。
    pub on: bool,
}

/// リアクションを付ける／外す。
#[utoipa::path(post, path = "/messaging/messages/{id}/reactions", params(("id" = Uuid, Path)), request_body = ReactBody,
    responses((status = 200, description = "成功", body = MessageView), (status = 400, description = "絵文字ではない")),
    security(("session" = [])))]
pub async fn react(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(body): Json<ReactBody>,
) -> Result<Json<MessageView>, ApiError> {
    Ok(Json(
        service(&state)?
            .react(&ctx, id, &body.emoji, body.on, trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SearchQuery {
    pub q: String,
    pub limit: Option<i64>,
}

/// 発言を検索する（自分が参加しているチャンネルと自分の DM だけ・二段 authz）。
#[utoipa::path(get, path = "/messaging/search", params(SearchQuery),
    responses((status = 200, description = "成功", body = Vec<SearchHit>), (status = 400, description = "検索語が不正")),
    security(("session" = [])))]
pub async fn search(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<SearchHit>>, ApiError> {
    Ok(Json(
        service(&state)?
            .search(&ctx, &q.q, q.limit.unwrap_or(30), trace.0.as_deref())
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MentionsQuery {
    #[serde(default)]
    pub unread: bool,
}

/// 自分あてのメンション（アプリ内通知）。
#[utoipa::path(get, path = "/messaging/mentions", params(MentionsQuery),
    responses((status = 200, description = "成功", body = Vec<MentionNotice>)),
    security(("session" = [])))]
pub async fn list_mentions(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<MentionsQuery>,
) -> Result<Json<Vec<MentionNotice>>, ApiError> {
    Ok(Json(
        service(&state)?
            .mentions(&ctx, q.unread)
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReadMentionsBody {
    /// 既読にするメンション（空なら全部）。
    #[serde(default)]
    pub message_ids: Vec<Uuid>,
}

/// メンションを既読にする。
#[utoipa::path(post, path = "/messaging/mentions/read", request_body = ReadMentionsBody,
    responses((status = 204, description = "既読にした")),
    security(("session" = [])))]
pub async fn read_mentions(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Json(body): Json<ReadMentionsBody>,
) -> Result<StatusCode, ApiError> {
    service(&state)?
        .mark_mentions_read(&ctx, &body.message_ids)
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 自分だけに表示中のエージェントの回答。
#[utoipa::path(get, path = "/messaging/channels/{id}/agent-replies", params(("id" = Uuid, Path)),
    responses((status = 200, description = "成功", body = Vec<AgentReplyView>)),
    security(("session" = [])))]
pub async fn list_agent_replies(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AgentReplyView>>, ApiError> {
    Ok(Json(
        service(&state)?
            .agent_replies(&ctx, id)
            .await
            .map_err(error)?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct StreamQuery {
    /// これより後に変わった発言から（`Last-Event-ID` と同じ・ヘッダが優先）。
    pub since: Option<DateTime<Utc>>,
}

/// 購読の状態（[`stream_channel`] の unfold）。
struct StreamState {
    svc: Arc<MessagingService>,
    ctx: authz::AuthContext,
    grant: messaging::stream::ReadGrant,
    sub: Subscription,
    cursor: DateTime<Utc>,
    /// 送った発言の版（`updated_at`）。重なりのある読み直しで同じ版を二度送らない。
    sent: std::collections::HashMap<Uuid, DateTime<Utc>>,
    /// 自分だけに表示する回答を最後に確かめた時刻（合図を取りこぼしたときにポーリングで拾う）。
    replies_checked: DateTime<Utc>,
    pending: std::collections::VecDeque<Event>,
    keepalive: tokio::time::Interval,
    poll: tokio::time::Interval,
    recheck: tokio::time::Interval,
    closed: bool,
}

impl StreamState {
    /// 変わった発言を自分の権限で読み直して積む。読めなくなっていたら `leave` を積んで閉じる。
    async fn refresh(&mut self) {
        // `updated_at` は書いたトランザクションの時刻で、コミット順と前後しうる。カーソルぴったりから
        // 読むと、後からコミットされた古い時刻の行を取りこぼすので、OVERLAP だけ遡って読み、
        // 送った版は飛ばす。
        let since = self.cursor - OVERLAP;
        match self
            .svc
            .changes_with(&self.ctx, &mut self.grant, since, &mut self.sent)
            .await
        {
            Ok(views) => {
                for v in views {
                    self.cursor = self.cursor.max(v.updated_at);
                    if let Ok(json) = serde_json::to_string(&v) {
                        self.pending.push_back(
                            Event::default()
                                .event("message")
                                .id(self
                                    .cursor
                                    .to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
                                .data(json),
                        );
                    }
                }
            }
            Err(messaging::Error::NotFound | messaging::Error::Forbidden(_)) => self.leave(),
            Err(e) => tracing::warn!(error = %e, "messaging stream: 読み直しに失敗"),
        }
    }

    /// 自分だけに表示する回答が増えていたら `agent_reply` を積む（別レプリカで書かれ合図が届かない場合）。
    async fn poll_agent_replies(&mut self) {
        let now = Utc::now();
        match self
            .svc
            .has_agent_replies_since(
                &self.ctx,
                self.grant.channel(),
                self.replies_checked - OVERLAP,
            )
            .await
        {
            Ok(true) => self
                .pending
                .push_back(Event::default().event("agent_reply").data("{}")),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "messaging stream: 回答の確認に失敗"),
        }
        self.replies_checked = now;
    }

    /// 権限を引き直す（剥奪の合図・上限 TTL）。
    async fn recheck(&mut self) {
        match self
            .svc
            .authorize_read(&self.ctx, self.grant.channel())
            .await
        {
            Ok(g) => self.grant = g,
            Err(messaging::Error::NotFound | messaging::Error::Forbidden(_)) => self.leave(),
            Err(e) => tracing::warn!(error = %e, "messaging stream: 権限の引き直しに失敗"),
        }
    }

    fn leave(&mut self) {
        self.pending
            .push_back(Event::default().event("leave").data("{}"));
        self.closed = true;
    }

    async fn next(&mut self) -> Option<Event> {
        loop {
            if let Some(e) = self.pending.pop_front() {
                return Some(e);
            }
            if self.closed {
                return None;
            }
            let me = self.ctx.principal().id.clone();
            tokio::select! {
                s = self.sub.recv() => match s {
                    Some(Signal::Changed) => self.refresh().await,
                    Some(Signal::Leave { user_id }) if user_id == me => self.recheck().await,
                    Some(Signal::Channel) => {
                        self.pending.push_back(Event::default().event("channel").data("{}"));
                        self.recheck().await;
                    }
                    Some(Signal::AgentReply { user_id }) if user_id == me => {
                        self.pending.push_back(Event::default().event("agent_reply").data("{}"));
                    }
                    // 他人あての合図（leave・自分だけの回答）は見ない。
                    Some(Signal::Leave { .. } | Signal::AgentReply { .. }) => {}
                    None => self.closed = true,
                },
                _ = self.poll.tick() => {
                    self.refresh().await;
                    self.poll_agent_replies().await;
                }
                _ = self.recheck.tick() => self.recheck().await,
                _ = self.keepalive.tick() => {
                    return Some(Event::default().event("keepalive").data("{}"));
                }
            }
        }
    }
}

fn interval(d: Duration) -> tokio::time::Interval {
    let mut i = tokio::time::interval_at(tokio::time::Instant::now() + d, d);
    i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    i
}

/// チャンネルの配信（SSE）。イベントは `message`（発言の追加・変更）・`agent_reply`（自分だけの回答）・
/// `channel`（設定の変更）・`leave`（読めなくなった・接続を閉じる）・`keepalive`。
#[utoipa::path(get, path = "/messaging/channels/{id}/stream", params(("id" = Uuid, Path), StreamQuery),
    responses((status = 200, description = "text/event-stream"), (status = 404, description = "無い・読めない")),
    security(("session" = [])))]
pub async fn stream_channel(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let svc = service(&state)?.clone();
    // 購読の開始時に読めることを確かめる（読めなければ 404）。以後は許可を持ち回り、TTL で引き直す。
    let grant = svc.authorize_read(&ctx, id).await.map_err(error)?;
    let since = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
        .map(|d| d.with_timezone(&Utc))
        .or(q.since)
        .unwrap_or_else(Utc::now);
    // トピックは AuthContext の tenant_id から組む（呼び出し側の値を使わない・PIT-66）。
    let sub = svc.hub().subscribe(ctx.tenant_id(), id);
    let st = StreamState {
        svc,
        ctx,
        grant,
        sub,
        cursor: since,
        sent: std::collections::HashMap::new(),
        replies_checked: Utc::now(),
        pending: std::collections::VecDeque::from([Event::default().event("ready").data("{}")]),
        keepalive: interval(KEEPALIVE),
        poll: interval(POLL),
        recheck: interval(RECHECK),
        closed: false,
    };
    let stream = futures::stream::unfold(st, |mut st| async move {
        let e = st.next().await?;
        Some((Ok::<Event, Infallible>(e), st))
    });
    Ok(super::sse_util::no_buffer(
        Sse::new(stream)
            .keep_alive(KeepAlive::new().interval(KEEPALIVE))
            .into_response(),
    ))
}
