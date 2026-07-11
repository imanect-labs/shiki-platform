//! WebSocket セッションループ（y-websocket 互換ワイヤ）。
//!
//! - 接続直後にサーバの sync step1（state vector）と awareness 全状態を送る。
//! - viewer は update（sync step2 / update）を**受理しない**（黙って破棄・fail-closed）。
//!   awareness（プレゼンス・カーソル）は viewer にも許可する。
//! - `CollabHub::recheck_interval` ごとに relation を再チェックし、剥奪されたら切断する（PIT-37②）。
//! - broadcast の Lagged（受信遅延で取りこぼし）は切断する。クライアントは再接続時の
//!   sync step1/2 で全差分を回復できるため安全側に倒せる。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use authz::AuthContext;
use axum::extract::ws::{Message as WsMessage, WebSocket};
use tokio::sync::broadcast::error::RecvError;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;

use crate::doc::LiveDoc;
use crate::error::CollabError;
use crate::hub::{AccessMode, CollabHub};

/// 接続 id の発番（プロセス内一意・自己エコー抑制用）。
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// 認可済み接続のセッションを実行する（切断まで戻らない）。
///
/// `mode` は接続時の判定結果。セッション中の再チェックで昇格/降格/剥奪を反映する。
pub async fn run_session(
    socket: WebSocket,
    hub: Arc<CollabHub>,
    ctx: AuthContext,
    doc: Arc<LiveDoc>,
    mode: AccessMode,
) {
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    let result = session_loop(socket, &hub, &ctx, &doc, mode, conn_id).await;
    if let Err(e) = result {
        tracing::debug!(node_id = %doc.node_id, conn_id, error = %e, "collab セッション終了（異常系）");
    }
    hub.leave(&doc).await;
}

/// セッション本体。終了理由に関わらず awareness の掃除は呼び出し側で行うため、
/// ここでは観測した client id を返す代わりに内部で掃除まで済ませる。
async fn session_loop(
    mut socket: WebSocket,
    hub: &Arc<CollabHub>,
    ctx: &AuthContext,
    doc: &Arc<LiveDoc>,
    mut mode: AccessMode,
    conn_id: u64,
) -> Result<(), CollabError> {
    let mut rx = doc.subscribe();
    // この接続が awareness で名乗った client 群（切断時に削除通知を流す）。
    let mut announced_clients: Vec<yrs::block::ClientID> = Vec::new();
    // 権限再チェック間隔（PIT-37②・WOPI と同じ「定期再チェック」の粒度）。
    let mut recheck = tokio::time::interval(hub.recheck_interval());
    recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    recheck.tick().await; // 初回 tick は即時発火するため読み捨てる（接続時に判定済み）。

    // 初期ハンドシェイク: サーバ state vector と awareness 全状態。
    let sv = doc.state_vector()?;
    send_binary(
        &mut socket,
        Message::Sync(SyncMessage::SyncStep1(sv)).encode_v1(),
    )
    .await?;
    if let Some(update) = doc.awareness_full()? {
        send_binary(&mut socket, Message::Awareness(update).encode_v1()).await?;
    }

    let close_reason = loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(WsMessage::Binary(bytes))) => {
                        if let Some(reason) = handle_client_message(
                            &mut socket, hub, doc, mode, conn_id, &bytes,
                            &ctx.principal.id, &mut announced_clients,
                        ).await? {
                            break Some(reason);
                        }
                    }
                    // Ping/Pong は axum が応答する。Text は本プロトコルでは使わない。
                    Some(Ok(WsMessage::Close(_)) | Err(_)) | None => break None,
                    Some(Ok(_)) => {}
                }
            }
            frame = rx.recv() => {
                match frame {
                    Ok(f) if f.from == conn_id => {}
                    Ok(f) => send_binary(&mut socket, f.data).await?,
                    // 取りこぼしは再接続での再同期に倒す（安全側）。
                    Err(RecvError::Lagged(_)) => break Some("resync required"),
                    Err(RecvError::Closed) => break None,
                }
            }
            _ = recheck.tick() => {
                match hub.authorize(ctx, doc.node_id).await {
                    Ok(m) => mode = m,
                    // 共有解除（Forbidden）・判定不能はどちらも切断（fail-closed）。
                    Err(_) => break Some("permission revoked"),
                }
            }
        }
    };

    // 切断掃除: この接続が名乗っていた awareness を削除し他接続へ通知する。
    if let Some(update) = doc.remove_awareness_clients(&announced_clients)? {
        doc.broadcast(conn_id, Message::Awareness(update).encode_v1());
    }
    if let Some(reason) = close_reason {
        let _ = socket
            .send(WsMessage::Close(Some(axum::extract::ws::CloseFrame {
                code: 4403,
                reason: reason.into(),
            })))
            .await;
    }
    Ok(())
}

/// クライアントからの 1 メッセージを処理する。`Some(reason)` はセッション終了指示。
#[allow(clippy::too_many_arguments)] // セッションループの内部分割（状態は借用で受ける）。
async fn handle_client_message(
    socket: &mut WebSocket,
    hub: &Arc<CollabHub>,
    doc: &Arc<LiveDoc>,
    mode: AccessMode,
    conn_id: u64,
    bytes: &[u8],
    author: &str,
    announced_clients: &mut Vec<yrs::block::ClientID>,
) -> Result<Option<&'static str>, CollabError> {
    // デコード不能な入力は敵対的とみなし切断する（PIT-23 と同じ前提）。
    let Ok(message) = Message::decode_v1(bytes) else {
        return Ok(Some("invalid message"));
    };
    match message {
        Message::Sync(SyncMessage::SyncStep1(sv)) => {
            let diff = doc.diff(&sv)?;
            let reply = Message::Sync(SyncMessage::SyncStep2(diff)).encode_v1();
            send_binary(socket, reply).await?;
        }
        Message::Sync(SyncMessage::SyncStep2(update) | SyncMessage::Update(update)) => {
            if mode != AccessMode::Editor {
                // viewer の書込は受理しない。プロトコル違反として扱わず黙って破棄する
                // （読取専用 UI でも過渡的に update が飛び得るため接続は維持する）。
                tracing::debug!(node_id = %doc.node_id, conn_id, "viewer からの update を破棄");
                return Ok(None);
            }
            doc.apply_and_persist(hub.store(), &update, author).await?;
            doc.broadcast(
                conn_id,
                Message::Sync(SyncMessage::Update(update)).encode_v1(),
            );
        }
        Message::Awareness(update) => {
            for client_id in update.clients.keys() {
                if !announced_clients.contains(client_id) {
                    announced_clients.push(*client_id);
                }
            }
            doc.apply_awareness(update.clone())?;
            doc.broadcast(conn_id, Message::Awareness(update).encode_v1());
        }
        Message::AwarenessQuery => {
            if let Some(update) = doc.awareness_full()? {
                send_binary(socket, Message::Awareness(update).encode_v1()).await?;
            }
        }
        // Auth はサーバ→クライアント通知専用・Custom は未使用（無視して前進互換に倒す）。
        Message::Auth(_) | Message::Custom(..) => {}
    }
    Ok(None)
}

async fn send_binary(socket: &mut WebSocket, data: Vec<u8>) -> Result<(), CollabError> {
    socket
        .send(WsMessage::Binary(data.into()))
        .await
        .map_err(|e| CollabError::InvalidUpdate(format!("ws send: {e}")))
}
