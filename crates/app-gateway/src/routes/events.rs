//! events.subscribe 能力アダプタ（Task 9.8・SSE ライブテール）。
//!
//! outbox（`storage_event_outbox`）の**アプリ向けドメインイベント**（`payload.event_type` 付き・
//! 例 `data.record.transitioned`）を SSE で流す。対象は**アプリ所有 ∩ 呼出ユーザーが viewer**
//! のテーブルに束縛する（接続時スナップショット・剥奪の反映は再接続単位）。
//!
//! 配送保証は**ライブテールのみ**（接続時点以降・GC 済みイベントは見えない）。B2 関数トリガの
//! at-least-once 消費は配送台帳（`claim_undelivered`・PR11）が担い、この SSE は UI のリアルタイム
//! 更新用。読み取り専用で配送状態（processed_at / outbox_delivery）には一切触れない。

use std::collections::{HashSet, VecDeque};
use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    Extension,
};
use futures::stream::Stream;
use storage::event::{latest_event_id, peek_app_events_after};
use storage::OutboxEvent;
use uuid::Uuid;

use crate::{
    router::{GatewayCtx, GatewayState},
    GatewayError,
};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const PEEK_BATCH: i64 = 100;

/// イベントが購読アプリへ可視か（`payload.table_id` が許可テーブル集合に含まれるか）。
///
/// table_id を持たない・パースできないイベントは**流さない**（fail-closed）。
fn event_visible(event: &OutboxEvent, allowed_tables: &HashSet<Uuid>) -> bool {
    event
        .payload
        .get("table_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .is_some_and(|id| allowed_tables.contains(&id))
}

/// [`OutboxEvent`] → SSE イベント（id=outbox id・event=event_type・data=payload）。
fn to_sse(event: &OutboxEvent) -> Event {
    let kind = event
        .payload
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    Event::default()
        .id(event.id.to_string())
        .event(kind)
        .data(event.payload.to_string())
}

pub(crate) async fn subscribe(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, GatewayError> {
    // 束縛テーブル: アプリ所有 ∩ ユーザー viewer（接続時スナップショット）。
    let allowed: HashSet<Uuid> = state
        .caps
        .data
        .list_tables(&ctx.auth, 200)
        .await?
        .into_iter()
        .filter(|t| t.app_id == Some(ctx.installation.app_id))
        .map(|t| t.id)
        .collect();
    // 接続時点のカーソルから未来分のみ（履歴のリプレイはしない）。
    let cursor = latest_event_id(&state.caps.db, &ctx.auth.tenant_id).await?;
    let db = state.caps.db.clone();
    let tenant = ctx.auth.tenant_id.clone();

    struct TailState {
        cursor: i64,
        db: sqlx::PgPool,
        tenant: String,
        allowed: HashSet<Uuid>,
        buf: VecDeque<Event>,
    }
    let stream = futures::stream::unfold(
        TailState {
            cursor,
            db,
            tenant,
            allowed,
            buf: VecDeque::new(),
        },
        |mut s| async move {
            loop {
                if let Some(ev) = s.buf.pop_front() {
                    return Some((Ok::<Event, Infallible>(ev), s));
                }
                tokio::time::sleep(POLL_INTERVAL).await;
                match peek_app_events_after(&s.db, &s.tenant, s.cursor, PEEK_BATCH).await {
                    Ok(events) => {
                        for e in events {
                            s.cursor = s.cursor.max(e.id);
                            if event_visible(&e, &s.allowed) {
                                s.buf.push_back(to_sse(&e));
                            }
                        }
                    }
                    Err(e) => {
                        // 一時的な DB 障害では切断しない（次のポーリングで再試行）。
                        tracing::warn!(error = %e, "events.subscribe のポーリングに失敗");
                    }
                }
            }
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn ev(payload: serde_json::Value) -> OutboxEvent {
        OutboxEvent {
            id: 1,
            org: "acme".into(),
            tenant_id: "t".into(),
            node_id: Uuid::new_v4(),
            version: 1,
            op: "update".into(),
            actor: "alice".into(),
            trace_id: None,
            payload,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn visible_only_for_allowed_tables() {
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        let allowed: HashSet<Uuid> = [t1].into_iter().collect();
        assert!(event_visible(
            &ev(json!({ "event_type": "data.record.transitioned", "table_id": t1 })),
            &allowed
        ));
        // 非許可テーブル・table_id 欠落・不正 UUID は fail-closed。
        assert!(!event_visible(
            &ev(json!({ "event_type": "data.record.transitioned", "table_id": t2 })),
            &allowed
        ));
        assert!(!event_visible(
            &ev(json!({ "event_type": "data.record.transitioned" })),
            &allowed
        ));
        assert!(!event_visible(
            &ev(json!({ "event_type": "x", "table_id": "not-a-uuid" })),
            &allowed
        ));
    }
}
