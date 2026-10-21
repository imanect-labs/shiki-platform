//! 編集セッションの WebSocket 中継（ブラウザ ↔ 中継 ↔ Collabora）。
//!
//! ブラウザ → Collabora のフレームだけを検査する（Collabora → ブラウザは素通し）。
//! スクリプト系の命令を見つけたら、そのフレームを捨てて**両側の接続を切る**。
//! 正規の編集画面はこの命令を送らない（拡張機能の UI を使わない限り）ので、
//! 届いた時点で改ざんされたクライアントとみなす。

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, protocol::WebSocketConfig};

use super::forward::upstream_url;
use super::policy::{client_frame_verdict, FrameVerdict};
use super::RelayConfig;

/// 上流へ WebSocket を張るまでの上限。届かない上流を待ち続けて接続を抱えない。
const UPSTREAM_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// ポリシー違反で切るときのクローズコード（RFC 6455 の Policy Violation）。
const POLICY_VIOLATION: u16 = 1008;

/// ブラウザ側のアップグレードを受け、上流へ同じパスで WebSocket を張って中継する。
pub(super) fn bridge(upgrade: WebSocketUpgrade, config: &RelayConfig, parts: &Parts) -> Response {
    let ws_base = config
        .upstream_base
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let url = upstream_url(&ws_base, parts.uri.path(), parts.uri.query());
    // 1 メッセージの上限は HTTP の本文と同じ（貼り付け・画像の挿入が通る大きさ）。認証前の接続でも
    // 確保されうるので、上限を上げすぎない。
    let max_message = config.max_body_bytes;
    // coolwsd は Origin と Host の一致を見て WS を受け入れる（不一致は 403・実機確認）。
    // ブラウザの Origin と Host をそのまま渡す（Collabora 公式の nginx 例の `Host $host` と同じ）。
    let origin = parts.headers.get(header::ORIGIN).cloned();
    let host = parts.headers.get(header::HOST).cloned();
    upgrade
        .max_message_size(max_message)
        .on_upgrade(move |client| async move {
            if let Err(e) = run(client, url, origin, host, max_message).await {
                tracing::warn!(error = %e, "office relay: 編集セッションの中継が異常終了");
            }
        })
}

async fn run(
    client: WebSocket,
    url: String,
    origin: Option<HeaderValue>,
    host: Option<HeaderValue>,
    max_message: usize,
) -> Result<(), tungstenite::Error> {
    let mut request = url.into_client_request()?;
    if let Some(origin) = origin {
        request.headers_mut().insert(header::ORIGIN, origin);
    }
    if let Some(host) = host {
        request.headers_mut().insert(header::HOST, host);
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(max_message))
        .max_frame_size(Some(max_message));
    let connect = tokio_tungstenite::connect_async_with_config(request, Some(config), false);
    let Ok(connected) = tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, connect).await else {
        let mut client = client;
        let closed = client
            .send(Message::Close(Some(CloseFrame {
                code: 1011,
                reason: "".into(),
            })))
            .await;
        if let Err(e) = closed {
            tracing::debug!(error = %e, "office relay: ブラウザへの切断通知を送れません");
        }
        return Err(tungstenite::Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Collabora へ WebSocket を張れません（時間切れ）",
        )));
    };
    let (upstream, _) = connected?;
    let (mut up_tx, mut up_rx) = upstream.split();
    let (mut cl_tx, mut cl_rx) = client.split();
    // ブラウザへの送信は 1 か所に集める（素通しの流れと、違反時の切断通知の両方が使う）。
    let (to_client, mut to_client_rx) = tokio::sync::mpsc::channel::<Message>(64);

    let client_writer = async {
        while let Some(msg) = to_client_rx.recv().await {
            let closing = matches!(msg, Message::Close(_));
            if cl_tx.send(msg).await.is_err() || closing {
                break;
            }
        }
        if let Err(e) = cl_tx.close().await {
            tracing::debug!(error = %e, "office relay: ブラウザ側の接続を閉じられません");
        }
    };

    // Collabora → ブラウザ: 素通し。
    let downstream = {
        let to_client = to_client.clone();
        async move {
            while let Some(Ok(msg)) = up_rx.next().await {
                let Some(out) = to_axum(msg) else { continue };
                let closing = matches!(out, Message::Close(_));
                if to_client.send(out).await.is_err() || closing {
                    break;
                }
            }
        }
    };

    // ブラウザ → Collabora: 検査してから送る。
    let upstream_pump = async move {
        while let Some(Ok(msg)) = cl_rx.next().await {
            let payload: Option<&[u8]> = match &msg {
                Message::Text(t) => Some(t.as_bytes()),
                Message::Binary(b) => Some(b),
                _ => None,
            };
            if let Some(FrameVerdict::Block(command)) = payload.map(client_frame_verdict) {
                tracing::warn!(
                    command,
                    "office relay: ブラウザからのスクリプト実行命令を拒否し、接続を切りました"
                );
                if to_client
                    .send(Message::Close(Some(CloseFrame {
                        code: POLICY_VIOLATION,
                        reason: "".into(),
                    })))
                    .await
                    .is_err()
                {
                    tracing::debug!("office relay: 切断通知の前にブラウザ側が閉じました");
                }
                break;
            }
            let closing = matches!(msg, Message::Close(_));
            if up_tx.send(to_tungstenite(msg)).await.is_err() || closing {
                break;
            }
        }
        if let Err(e) = up_tx.close().await {
            tracing::debug!(error = %e, "office relay: Collabora 側の接続を閉じられません");
        }
    };

    // どちらかの流れが終われば中継を終える（片側だけ生き残らせない）。両方の流れが落ちると
    // 送信側がすべて drop され、書き込み側は残りを送り切って（違反時の Close を含む）終わる。
    let pumps = async move {
        tokio::select! {
            () = downstream => {}
            () = upstream_pump => {}
        }
    };
    tokio::join!(pumps, client_writer);
    Ok(())
}

fn to_tungstenite(msg: Message) -> tungstenite::Message {
    match msg {
        Message::Text(t) => tungstenite::Message::Text(t.as_str().into()),
        Message::Binary(b) => tungstenite::Message::Binary(b),
        Message::Ping(p) => tungstenite::Message::Ping(p),
        Message::Pong(p) => tungstenite::Message::Pong(p),
        Message::Close(c) => {
            tungstenite::Message::Close(c.map(|c| tungstenite::protocol::CloseFrame {
                code: CloseCode::from(c.code),
                reason: c.reason.as_str().into(),
            }))
        }
    }
}

fn to_axum(msg: tungstenite::Message) -> Option<Message> {
    Some(match msg {
        tungstenite::Message::Text(t) => Message::Text(t.as_str().into()),
        tungstenite::Message::Binary(b) => Message::Binary(b),
        tungstenite::Message::Ping(p) => Message::Ping(p),
        tungstenite::Message::Pong(p) => Message::Pong(p),
        tungstenite::Message::Close(c) => Message::Close(c.map(|c| CloseFrame {
            code: c.code.into(),
            reason: c.reason.as_str().into(),
        })),
        tungstenite::Message::Frame(_) => return None,
    })
}
