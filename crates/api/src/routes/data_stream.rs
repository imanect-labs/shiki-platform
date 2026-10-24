//! 構造化データ API — レコード変更の SSE 差分配信（Task 15.6・data-platform §9）。
//!
//! `GET /data/tables/{id}/stream?since=<delivery_seq>`（`Last-Event-ID` ヘッダでも可）。
//! 購読者ごとに行述語とフィールドマスクを評価してから配信する（見えない行・列は流れない）。
//! 可視→不可視・削除は `record.removed`（id のみ）。イベントの型は [`data::StreamEvent`] が単一ソース。
//!
//! **製品の約束（PIT-70）**: SSE 購読中の権限の剥奪は、配信への反映が最大 TTL（既定 5 秒）遅れる。
//! REST の読取は毎回解決するので遅れない。
//!
//! `POST /data/tables/{id}/awareness` は他ユーザーのカーソル位置（データを含まない）。購読の直後
//! （`stream.ready` の後の最初のバッチ）には、接続前に置かれた有効期間内のカーソルも届く
//! （見えない行を指すもの・マスク列のものは除く・判定は data 側）。

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
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// `keepalive` イベントの間隔（コメント行の keep-alive も同じ間隔）。
const STREAM_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(15);

/// 購読の開始位置。
#[derive(Debug, Deserialize, IntoParams)]
pub struct StreamQuery {
    /// 連続して受けた最後の位置（`Last-Event-ID` と同じ・ヘッダが優先）。省略時は現在の位置から
    /// （REST の応答の `snapshot_seq` を渡すと、読取と購読の間の変更を取りこぼさない）。
    pub since: Option<i64>,
}

/// awareness（カーソル位置・データを含まない）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct AwarenessRequest {
    /// 指しているレコード（`null` は離脱）。本人に見えない行は 404。
    pub record_id: Option<Uuid>,
    /// 指している列の参照名（マスク列は 403）。
    pub field: Option<String>,
}

fn last_event_id(headers: &HeaderMap) -> Result<Option<i64>, ApiError> {
    match headers.get("last-event-id") {
        None => Ok(None),
        Some(v) => v
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .map(Some)
            .ok_or_else(|| ApiError::BadRequest("Last-Event-ID が不正です".into())),
    }
}

fn to_sse(m: &data::StreamMessage) -> Event {
    let ev = Event::default()
        .event(m.event.name())
        .data(m.event.data_json().to_string());
    match m.id {
        Some(id) => ev.id(id.to_string()),
        None => ev,
    }
}

/// レコード変更を購読する（viewer・SSE）。
///
/// 見えない行・マスク列は流れない。可視→不可視・削除は `record.removed`（id のみ）。
/// **SSE 購読中の権限の剥奪は、配信への反映が最大 TTL（既定 5 秒・`/org-admin/data/settings` の
/// `stream_revocation_delay_ms`）遅れる**（REST の読取は遅れない）。到達は at-least-once で、
/// クライアントは `(record_id, rev)` で冪等に適用する。保持期間外の `since` には `stream.reset`。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/stream",
    params(("id" = Uuid, Path, description = "テーブル ID"), StreamQuery),
    responses(
        (status = 200, description = "SSE（text/event-stream）。各イベントの data は StreamEvent の対応する型", body = data::StreamEvent),
        (status = 400, description = "Last-Event-ID が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "テーブルの viewer でない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn stream_records(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let since = last_event_id(&headers)?.or(q.since);
    let rx = state
        .data
        .subscribe_records(&ctx, id, since, trace.as_deref())
        .await?;
    // 15 秒ごとに `keepalive` イベントを送る（コメント行はブラウザの EventSource から読めないため、
    // クライアントが途絶を検知して繋ぎ直せるよう名前つきのイベントにする・検証 D-2）。
    let mut tick = tokio::time::interval_at(
        tokio::time::Instant::now() + STREAM_KEEPALIVE,
        STREAM_KEEPALIVE,
    );
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let stream = futures::stream::unfold((rx, tick), |(mut rx, mut tick)| async move {
        let m = tokio::select! {
            m = rx.next() => m?,
            _ = tick.tick() => data::StreamMessage {
                id: None,
                event: data::StreamEvent::Keepalive(data::StreamKeepaliveEvent {
                    at: chrono::Utc::now(),
                }),
            },
        };
        Some((Ok::<Event, Infallible>(to_sse(&m)), (rx, tick)))
    });
    Ok(super::sse_util::no_buffer(
        Sse::new(stream)
            .keep_alive(KeepAlive::new().interval(STREAM_KEEPALIVE))
            .into_response(),
    ))
}

/// 自分のカーソル位置を知らせる（viewer・データを含まない）。
#[utoipa::path(
    post,
    path = "/data/tables/{id}/awareness",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = AwarenessRequest,
    responses(
        (status = 204, description = "受け付けた"),
        (status = 400, description = "存在しない列"),
        (status = 401, description = "未認証"),
        (status = 403, description = "viewer でない・マスク列"),
        (status = 404, description = "テーブル・レコードが（本人に）存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn post_awareness(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<AwarenessRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .data
        .post_awareness(&ctx, id, req.record_id, req.field, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_event_id_parses_or_rejects() {
        let mut h = HeaderMap::new();
        assert_eq!(last_event_id(&h).unwrap(), None);
        h.insert("last-event-id", "42".parse().unwrap());
        assert_eq!(last_event_id(&h).unwrap(), Some(42));
        h.insert("last-event-id", "x".parse().unwrap());
        assert!(last_event_id(&h).is_err());
    }
}
