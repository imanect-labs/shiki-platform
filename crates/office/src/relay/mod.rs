//! ブラウザ ↔ Collabora の中継（PIT-68・design §4.8）。
//!
//! Collabora 26.04 の `executescript` は、編集セッションの WebSocket に 1 行送るだけで kit 内の
//! QuickJS に完全な UNO API を持つ JS を走らせる。しかも閲覧専用セッションからでも通る
//! （上流の設定 `experimental_features` / `security.allow_external_scripting` では止まらない・実機確認）。
//! AI 参加者はこれを文書操作エンジンに使うが、ブラウザに同じ力を渡すと
//! 「閲覧者が文書を書き換える」「kit からの SSRF」が開く。
//!
//! そこでブラウザの経路を shiki が持つ中継に一本化し、
//! - HTTP は編集画面が使う経路だけを許可リストで通す（[`policy::route_for`]）
//! - 編集セッションの WebSocket はフレームを検査し、スクリプト系の命令を落とす
//!   （[`policy::client_frame_verdict`]・違反したら接続ごと切る）
//!
//! AI 参加者は中継を通らず、内部網から Collabora へ直接つなぐ。**Collabora 自体をブラウザへ
//! 公開しないこと**が前提（compose はポートを公開せず、中継のポートを公開する）。

mod forward;
pub mod policy;
mod ws;

use std::sync::Arc;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;

pub use policy::{client_frame_verdict, route_for, FrameVerdict, Route};

/// 中継の設定。
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// 転送先の Collabora（内部網の URL・例 `http://collabora:9980`）。
    pub upstream_base: String,
    /// 転送する要求本文の上限（バイト）。画像の挿入・クリップボードが通る大きさにする。
    pub max_body_bytes: usize,
}

#[derive(Clone)]
struct RelayState {
    config: Arc<RelayConfig>,
    http: reqwest::Client,
}

/// 中継のルータを組む（全パスを 1 つのハンドラで受け、[`policy::route_for`] で振り分ける）。
///
/// `http` はリダイレクトを追わないクライアントを渡すこと（上流の 3xx はそのままブラウザへ返す）。
pub fn build_relay_router(config: RelayConfig, http: reqwest::Client) -> Router {
    let state = RelayState {
        config: Arc::new(config),
        http,
    };
    Router::new().fallback(relay).with_state(state)
}

async fn relay(State(state): State<RelayState>, req: Request) -> Response {
    let is_upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let route = policy::route_for(req.method().as_str(), req.uri().path(), is_upgrade);
    match route {
        None => {
            tracing::debug!(method = %req.method(), path = %req.uri().path(), "office relay: 許可リスト外の要求を拒否");
            StatusCode::NOT_FOUND.into_response()
        }
        Some(Route::Http) => forward::forward(&state.http, &state.config, req).await,
        Some(Route::DocumentSocket) => {
            let (mut parts, _body) = req.into_parts();
            let upgrade =
                match axum::extract::WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
                    Ok(u) => u,
                    Err(rejection) => return rejection.into_response(),
                };
            ws::bridge(upgrade, &state.config, &parts)
        }
    }
}
