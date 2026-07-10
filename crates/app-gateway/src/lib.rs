//! 公開 API ゲートウェイ（Phase 9 Task 9.6/9.7・design §4.6）。
//!
//! ミニアプリ（out-of-trust）が内部機能を叩く**唯一の入口**。内部 API（cookie セッション）
//! とは**別オリジン（別ポート）**で待ち受け、Bearer JWT のみを受理する（cookie を持たない・
//! CORS credentials なし）ことで「ゲートウェイ以外からミニアプリ経由で内部へ到達できない」を
//! 構造的に担保する。
//!
//! **二重ゲート**（confused-deputy 防御・design §4.3）:
//! 1. アクセストークン検証（JWKS・iss/exp/aud=gateway/azp=登録 client）
//! 2. ルート → 必要 [`authz::CapabilityScope`] の**宣言的マップ**（個別ハンドラでチェックしない）
//! 3. `app_installation.granted_scopes` 突合（同意失効・scope 縮小の即時反映）
//! 4. ハンドラ内で per-call OpenFGA（**呼出ユーザーの** ReBAC）
//!
//! 実効権限 = アプリ付与スコープ ∩ ユーザー ReBAC。B1=public+PKCE / B2=confidential+token-exchange
//! の OAuth2 クライアントは [`oauth`] が Keycloak へ動的登録する（新しい認証基盤は作らない）。

mod installation;
mod oauth;
mod router;
mod scope_map;
mod token;
mod token_exchange;

pub use installation::{AppInstallation, AppInstallationStore, InstallStatus, NewAppInstallation};
pub use oauth::{client_representation, ClientKind, OAuthClient, RegisteredClient};
pub use router::{build_gateway_router, GatewayCtx, GatewayState};
pub use scope_map::{required_scope_for, GatewayRoute, RouteScope};
pub use token::{verify_gateway_token, GatewayIdentity, GatewayTokenConfig, KeyResolver};
pub use token_exchange::{exchange_for_user, exchange_params, ExchangedToken};

/// 公開 API ゲートウェイのエラー。
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// Bearer トークンが無い / 不正 / 期限切れ（401）。
    #[error("認証に失敗しました: {0}")]
    Unauthenticated(String),
    /// スコープ不足 / 同意失効 / ユーザー ReBAC 不許可（403）。
    #[error("権限がありません: {0}")]
    Forbidden(String),
    /// インストールが見つからない / アプリ未登録（404 相当）。
    #[error("対象が見つかりません")]
    NotFound,
    /// 入力が不正（400）。
    #[error("不正な入力: {0}")]
    Invalid(String),
    /// Keycloak / 上流エラー（502 相当）。
    #[error("上流エラー: {0}")]
    Upstream(String),
    /// 内部エラー（500）。
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_db(e: sqlx::Error) -> GatewayError {
    GatewayError::Internal(format!("db: {e}"))
}
