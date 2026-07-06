//! `ChatStore`: SSE 用イベントストリーム（replay-then-subscribe・Task 3.5）。
//!
//! **DB=真実のソース／Redis=best-effort 起床通知**の設計。ストリームは常に
//! `generation_event` を `from_seq` から replay して単調 seq 順に配信し（重複しない）、
//! Redis メッセージは「新イベントが来た」の起床にのみ使う（取りこぼしても短周期 replay で補填）。
//! これによりバッファ順序レースを避けつつ、ページ離脱→再訪の復元と再接続の非重複を両立する。

#[allow(clippy::wildcard_imports)]
use super::*;

use std::time::Duration;

use futures::channel::mpsc;
use futures::stream::StreamExt;
use uuid::Uuid;

use super::runs::is_terminal_event;
use crate::model::StreamEvent;

impl ChatStore {
    /// run のイベントを `from_seq` より後から購読するストリームを返す（端末イベントで終了）。
    ///
    /// `Last-Event-ID`(=seq) を `from_seq` に渡せば再接続時に途中から再開できる（重複しない）。
    pub fn event_stream(
        &self,
        run_id: Uuid,
        from_seq: i64,
    ) -> mpsc::UnboundedReceiver<StreamEvent> {
        let (tx, rx) = mpsc::unbounded();
        let store = self.clone();
        tokio::spawn(async move {
            store.run_event_loop(run_id, from_seq, tx).await;
        });
        rx
    }

    async fn run_event_loop(
        self,
        run_id: Uuid,
        mut cursor: i64,
        tx: mpsc::UnboundedSender<StreamEvent>,
    ) {
        // Redis 購読（best-effort）。失敗しても DB replay で動作する。
        let mut on_message = match &self.redis {
            Some(r) => match r.client.get_async_pubsub().await {
                Ok(mut ps) => {
                    if ps.subscribe(run_channel(run_id)).await.is_ok() {
                        Some(ps.into_on_message())
                    } else {
                        None
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "redis subscribe failed; DB polling only");
                    None
                }
            },
            None => None,
        };

        loop {
            // 1) DB replay（真実のソース）。cursor より後を seq 順に配信。
            match self.replay_events(run_id, cursor).await {
                Ok(events) => {
                    for e in events {
                        cursor = e.seq;
                        let terminal = is_terminal_event(&e.event);
                        if tx.unbounded_send(e).is_err() {
                            return; // クライアント切断
                        }
                        if terminal {
                            return;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, run_id = %run_id, "replay failed");
                    return;
                }
            }

            // 2) crash safety: run が端末状態で残イベントも無ければ終了（最終 flush 済み）。
            match self.run_status(run_id).await {
                Ok(Some(s)) if s.is_terminal() => {
                    if let Ok(tail) = self.replay_events(run_id, cursor).await {
                        for e in tail {
                            let _ = tx.unbounded_send(e);
                        }
                    }
                    return;
                }
                Ok(None) => return, // run が消えた（テナント消去等）
                _ => {}
            }

            // 3) 起床待ち: Redis メッセージ or 短いタイムアウトで再 replay。
            match &mut on_message {
                Some(stream) => {
                    tokio::select! {
                        _ = stream.next() => {}
                        () = tokio::time::sleep(Duration::from_millis(700)) => {}
                    }
                }
                None => tokio::time::sleep(Duration::from_millis(300)).await,
            }
        }
    }
}
