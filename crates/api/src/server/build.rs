//! ルータ合成（[`build_router`]）。宣言表は親 `server.rs` の [`route_table`] が正。
//!
//! [`route_table`] をポリシー種別ごとに束ね、認証 middleware とタイムアウトを
//! **グループ単位で一律適用**する（ハンドラ個別のチェックを持たない）。

use std::time::Duration;

use axum::{http::StatusCode, middleware, Router};
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

use super::http_util::{cors_layer, make_request_span};
use super::{route_table, AccessPolicy};
use crate::{middleware::require_session, routes, state::AppState, telemetry};

/// アプリの axum ルータを構築する（テストからも利用）。
pub fn build_router(state: AppState) -> Router {
    let session_layer = middleware::from_fn_with_state(state.clone(), require_session);
    let standard_timeout =
        || TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30));
    let long_timeout =
        || TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_mins(5));

    let mut public = Router::new();
    let mut session_std = Router::new();
    let mut session_long = Router::new();
    let mut session_stream = Router::new();
    let mut admin = Router::new();
    // admin ルート（SAAS.2）: **config（provisioner 資格情報＋admin base）が揃っている時のみ
    // 組み込む**（未設定なら 404 = fail-closed）。
    let admin_enabled = state.config.auth.provisioner_credentials().is_some()
        && state.config.auth.admin_base().is_some();
    for decl in route_table() {
        // office ルート（Task 11.6）: `office.enabled=false` なら配線しない
        // （admin と同じ「未設定なら 404 = fail-closed」）。
        if decl.path.starts_with("/office/") && state.office.is_none() {
            continue;
        }
        let method_router = (decl.handler)();
        match decl.policy {
            AccessPolicy::Public => public = public.route(decl.path, method_router),
            AccessPolicy::Session => session_std = session_std.route(decl.path, method_router),
            AccessPolicy::SessionLongRunning => {
                session_long = session_long.route(decl.path, method_router);
            }
            AccessPolicy::SessionStreaming => {
                session_stream = session_stream.route(decl.path, method_router);
            }
            AccessPolicy::Provisioner => {
                if admin_enabled {
                    admin = admin.route(decl.path, method_router);
                }
            }
        }
    }

    let public = public.layer(standard_timeout());
    let session_std = session_std
        .route_layer(session_layer.clone())
        .layer(standard_timeout());
    let session_long = session_long
        .route_layer(session_layer.clone())
        .layer(long_timeout());
    // SSE ストリーム: セッション必須だがタイムアウトレイヤは付けない（接続を長時間開く）。
    let session_stream = session_stream.route_layer(session_layer);
    let admin = if admin_enabled {
        admin
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                routes::admin::require_provisioner,
            ))
            .layer(long_timeout())
    } else {
        admin
    };

    // WOPI ルータ（Task 11.6・トークン認証の別面）: Collabora（サーバ間通信）から
    // access_token クエリで呼ばれるため、cookie セッションの middleware を**通さず**
    // マウントする。認証・毎呼び出し ReBAC（HigherConsistency）はルータ内部の共通
    // 前段が一律に担う（crates/office::wopi::routes）。`office.enabled=false` では不在。
    let wopi = state
        .office
        .as_ref()
        .map(|o| office::build_wopi_router(o.wopi.clone()).layer(long_timeout()));

    let cors = cors_layer(&state.config.server.cors_allowed_origins);
    let mut router = public
        .merge(session_std)
        .merge(session_long)
        .merge(session_stream)
        .merge(admin)
        .with_state(state);
    // WOPI は状態適用済み（`Router<()>`）のため、`with_state` 後に merge する。
    // 後段の telemetry/Trace/CORS レイヤは merge 済みの全ルート（WOPI 含む）に掛かる。
    if let Some(wopi) = wopi {
        router = router.merge(wopi);
    }
    let router = router
        // observe は span 内で動く必要があるため TraceLayer より内側（先に追加）。
        .layer(middleware::from_fn(telemetry::observe))
        .layer(TraceLayer::new_for_http().make_span_with(make_request_span));

    // CORS: 同一オリジン配信が既定（レイヤ無し）。別オリジン dev のみ、設定された
    // オリジンに限定して credential 付きを許可する（permissive はセッション Cookie と
    // 併用すると危険なので使わない）。
    match cors {
        Some(cors) => router.layer(cors),
        None => router,
    }
}
