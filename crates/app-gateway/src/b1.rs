//! B1 フロントバンドル配信（Task 9.11・第3リスナ＝apps オリジン）。
//!
//! `GET /a/{app_id}/{sha256}` で**単一 self-contained HTML** バンドルを配信する。
//! - **同意時ピン突合**: `app_installation.frontend_bundle == sha`（active）以外は 404
//!   （publish 済みでも未インストール/未同意バージョンは配信しない）
//! - **content address**: 応答前に sha256 を再計算して検証（オブジェクトストア改竄検知）・
//!   URL が内容を一意に決めるため `immutable` キャッシュ
//! - **隔離**: cookie を発行しない・CSP `sandbox` ＋ `connect-src` をゲートウェイに限定・
//!   `frame-ancestors` をホスト（web シェル）に限定。iframe 側の `sandbox` 属性
//!   （allow-same-origin なし＝opaque origin）と二重で効く（PR10 web 側）。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use storage::ObjectStore;
use uuid::Uuid;

use crate::AppInstallationStore;

/// 第3リスナの共有状態。
#[derive(Clone)]
pub struct B1State {
    pub installations: AppInstallationStore,
    pub store: Arc<dyn ObjectStore>,
    /// CSP `connect-src` に許可するゲートウェイのオリジン（例 `http://localhost:8090`）。
    pub gateway_origin: String,
    /// CSP `frame-ancestors` に許可するホスト（web シェル）のオリジン。
    pub host_origin: String,
}

/// B1 配信 Router（`/a/{app_id}/{sha256}`）。
pub fn build_b1_router(state: B1State) -> Router {
    Router::new()
        .route("/a/{app_id}/{sha256}", get(serve_bundle))
        .with_state(state)
}

/// バンドル HTML の CSP 値（純粋関数・golden 単体テスト対象）。
///
/// - `sandbox allow-scripts allow-forms`: opaque origin 化（cookie/storage/親 DOM 不可達）
/// - `default-src 'none'`: 明示した以外の読み込みを全遮断
/// - `script-src/style-src 'unsafe-inline'`: 単一 self-contained HTML（インライン前提）。
///   opaque origin のため 'self' は意味を持たない
/// - `connect-src <gateway>`: **ゲートウェイ以外への通信を遮断**（データ持ち出し防止）
/// - `img-src data:`: 埋め込み画像のみ
/// - `frame-ancestors <host>`: ホストシェル以外への埋め込み禁止（クリックジャッキング防止）
pub fn bundle_csp(gateway_origin: &str, host_origin: &str) -> String {
    format!(
        "sandbox allow-scripts allow-forms; default-src 'none'; \
         script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; \
         connect-src {gateway_origin}; frame-ancestors {host_origin}"
    )
}

async fn serve_bundle(
    State(state): State<B1State>,
    Path((app_id, sha)): Path<(Uuid, String)>,
) -> Response {
    // sha は hex 64 のみ受理（オブジェクトキーへの注入防止・fail-closed）。
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // 同意時ピン突合: active インストールの frontend_bundle と一致しなければ存在秘匿 404。
    // B1 リスナは cookie/URL にテナントを持たない（opaque origin・cookieless）ため、
    // グローバル一意な app_id から active インストールと所属テナントを解決する。
    let (tenant_id, installation) = match state
        .installations
        .resolve_active_by_app_global(app_id)
        .await
    {
        Ok(Some(pair)) => pair,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "B1 配信のインストール解決に失敗");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if installation.frontend_bundle.as_deref() != Some(sha.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let key = miniapp_bundle_key(&tenant_id, &sha);
    let bytes = match state.store.get_object(&key).await {
        Ok(b) => b,
        Err(storage::ObjectStoreError::NotFound(_)) => {
            return StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "B1 バンドル読み出しに失敗");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // 配信前に content address を再検証（オブジェクトストア側の改竄/破損の検知）。
    if sha256_hex(&bytes) != sha {
        tracing::error!(app_id = %app_id, sha, "B1 バンドルの sha256 不一致（配信拒否）");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let csp = bundle_csp(&state.gateway_origin, &state.host_origin);
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_str(&csp).unwrap_or_else(|_| HeaderValue::from_static("sandbox")),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::bundle_csp;

    /// CSP golden（受け入れ条件: ゲートウェイ以外への通信遮断・opaque origin・埋め込み制限）。
    #[test]
    fn csp_golden() {
        let csp = bundle_csp("http://gw.example:8090", "http://host.example:3000");
        assert_eq!(
            csp,
            "sandbox allow-scripts allow-forms; default-src 'none'; \
             script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; \
             connect-src http://gw.example:8090; frame-ancestors http://host.example:3000"
        );
        // 変更検知の要点: sandbox に allow-same-origin が無い・connect-src が単一オリジン。
        assert!(!csp.contains("allow-same-origin"));
    }
}
