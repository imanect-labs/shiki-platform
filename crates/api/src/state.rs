//! アプリケーション共有状態。

use std::sync::Arc;

use authz::AuthzClient;
use sqlx::PgPool;

use crate::{config::AppConfig, middleware::JwksCache};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub db: PgPool,
    /// 認可チョークポイント（具象でなくトレイト経由）。
    pub authz: Arc<dyn AuthzClient>,
    pub jwks: Arc<JwksCache>,
}
