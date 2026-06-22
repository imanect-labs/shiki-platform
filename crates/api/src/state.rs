//! アプリケーション共有状態。

use std::sync::Arc;

use authz::AuthzClient;
use sqlx::PgPool;

use crate::{config::AppConfig, middleware::JwksCache, session::SessionStore};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub db: PgPool,
    /// 認可チョークポイント（具象でなくトレイト経由）。
    pub authz: Arc<dyn AuthzClient>,
    pub jwks: Arc<JwksCache>,
    /// BFF セッションストア（チョークポイント。Redis 実装をトレイト裏に隠す）。
    pub sessions: Arc<dyn SessionStore>,
    /// OIDC backchannel（token 交換・refresh）用の共有 HTTP クライアント。
    pub http: reqwest::Client,
}
