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
mod notification;
mod oauth;
mod ports;
mod router;
mod routes;
mod scope_map;
mod token;
mod token_exchange;
mod usage;

pub use installation::{AppInstallation, AppInstallationStore, InstallStatus, NewAppInstallation};
pub use notification::{AppNotification, NotificationStore};
pub use oauth::{client_representation, ClientKind, OAuthClient, RegisteredClient};
pub use ports::{NoRag, RagHit, RagPort};
pub use router::{build_gateway_router, CapabilityDeps, GatewayCtx, GatewayState};
pub use scope_map::{required_scope_for, GatewayRoute, RouteScope};
pub use token::{verify_gateway_token, GatewayIdentity, GatewayTokenConfig, KeyResolver};
pub use token_exchange::{exchange_for_user, exchange_params, ExchangedToken};
pub use usage::{fetch_usage, CapabilityUsage};

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
    /// 楽観ロック不一致・名前重複など（409）。
    #[error("競合しています: {0}")]
    Conflict(String),
    /// Keycloak / 上流エラー（502 相当）。
    #[error("上流エラー: {0}")]
    Upstream(String),
    /// 内部エラー（500）。
    #[error("内部エラー: {0}")]
    Internal(String),
}

/// 内部詳細を out-of-trust クライアントへ出さない汎用 500 本文。詳細は tracing にのみ残す。
const INTERNAL_MSG: &str = "内部エラーが発生しました";

impl From<data::DataError> for GatewayError {
    fn from(e: data::DataError) -> Self {
        use data::DataError;
        match e {
            // 不可視（行述語）と不存在は data 側で既に同一形状（PIT-21）。そのまま透過する。
            DataError::NotFound => GatewayError::NotFound,
            DataError::Forbidden => GatewayError::Forbidden("この操作は許可されていません".into()),
            DataError::Invalid(m) => GatewayError::Invalid(m),
            DataError::Conflict(m) => GatewayError::Conflict(m),
            DataError::Internal(m) => {
                tracing::error!(error = %m, "gateway data 内部エラー");
                GatewayError::Internal(INTERNAL_MSG.into())
            }
        }
    }
}

impl From<storage::StorageError> for GatewayError {
    fn from(e: storage::StorageError) -> Self {
        use storage::StorageError;
        match e {
            StorageError::NotFound => GatewayError::NotFound,
            StorageError::Forbidden => {
                GatewayError::Forbidden("この操作は許可されていません".into())
            }
            StorageError::Invalid(m) => GatewayError::Invalid(m),
            StorageError::Conflict => GatewayError::Conflict("名前が競合しています".into()),
            // 整合性・オブジェクトストア・DB・authz 障害の詳細はクライアントへ出さない。
            other => {
                tracing::error!(error = %other, "gateway storage 内部エラー");
                GatewayError::Internal(INTERNAL_MSG.into())
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_db(e: sqlx::Error) -> GatewayError {
    tracing::error!(error = %e, "gateway db エラー");
    GatewayError::Internal(INTERNAL_MSG.into())
}
