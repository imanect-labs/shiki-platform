//! 設定ローダ（docs/roadmap phase-0 Task 0.3）。
//!
//! env と TOML から読み、[`AppConfig`] に集約する。クラウド/オンプレの差し替えは
//! `*Backend` enum の値として設定で表現する起点（docs/design.md §3.1）。
//! Phase 0 では値の読み込みと検証のみで、実装インスタンス化は行わない。
//!
//! 優先順位（後勝ち）: 組み込みデフォルト → TOML(`SHIKI_CONFIG` or `config/default.toml`)
//! → 環境変数（`SHIKI__SECTION__KEY`、区切りは `__`）。
//! 必須項目は非 Option とし、欠落時は起動エラーで明確に落とす。

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub authz: AuthzConfig,
    /// BFF セッション（オパーク Cookie + Redis）。
    pub session: SessionConfig,
    pub telemetry: TelemetryConfig,
    // 差し替え点（Phase 0 は値の検証のみ）。
    pub storage: StorageConfig,
    pub vector: VectorConfig,
    pub llm: LlmConfig,
    /// RAG（インジェスト・パイプライン＋検索・Phase 2）。既定は無効。
    #[serde(default)]
    pub rag: rag::RagConfig,
    /// チャット（生成ワーカー・接続非依存生成・Phase 3）。既定は無効。
    #[serde(default)]
    pub chat: ChatConfig,
    /// web 検索（web_search / web_fetch ツール・Phase 4）。既定は無効。
    #[serde(default)]
    pub websearch: WebSearchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// CORS で credential 付きリクエストを許可するオリジン（完全一致）。
    /// 既定は空＝CORS レイヤ無効（同一オリジン配信前提・最も安全）。別オリジン dev 時のみ列挙。
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Postgres 接続 URL（必須）。
    pub url: String,
    pub max_connections: u32,
}

mod auth;
mod backends;
#[cfg(test)]
mod tests;

pub use auth::{AuthConfig, SessionConfig, Tenancy};
pub use backends::{
    AuthzConfig, ChatConfig, LangfuseConfig, LlmBackend, LlmConfig, LlmModelEntry, LogFormat,
    ObjectStoreBackend, StorageConfig, TelemetryConfig, VectorConfig, VectorStoreBackend,
    WebSearchBackend, WebSearchConfig,
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    // figment::Error は大きいため Box 化（clippy::result_large_err 回避）。
    #[error("設定の読み込みに失敗しました: {0}")]
    Load(#[from] Box<figment::Error>),
    #[error("設定の検証に失敗しました: {0}")]
    Invalid(String),
}

/// デフォルト値（必須でない項目のみ）。必須項目はここに含めない＝欠落で失敗させる。
fn defaults() -> serde_json::Value {
    serde_json::json!({
        "server": { "host": "0.0.0.0", "port": 8080 },
        "database": { "max_connections": 10 },
        "auth": {
            "jwks_ttl_secs": 300,
            "tenancy": "single",
            "tenant_id": "default",
            "client_id": "shiki-web",
            "redirect_uri": "http://localhost:3000/auth/callback",
            "post_logout_redirect_uri": "http://localhost:3000/",
            "scopes": "openid profile",
        },
        "session": {
            "redis_url": "redis://localhost:6379",
            "ttl_secs": 86400,
            "secure": true,
            "refresh_leeway_secs": 60,
        },
        "telemetry": { "service_name": "shiki-server", "log_format": "json" },
        "storage": { "backend": "minio" },
        "vector": { "backend": "qdrant" },
        "llm": { "backend": "vllm" },
    })
}

impl AppConfig {
    /// 環境変数・設定ファイルから設定をロードして検証する。
    pub fn load() -> Result<Self, ConfigError> {
        let config_path =
            std::env::var("SHIKI_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string());

        let mut config: AppConfig = Figment::new()
            .merge(Serialized::defaults(defaults()))
            .merge(Toml::file(config_path))
            .merge(Env::prefixed("SHIKI__").split("__"))
            .extract()
            .map_err(Box::new)?;

        // issuer の末尾スラッシュを正規化。Keycloak のトークン iss は末尾スラッシュ無しのため、
        // 設定に付いていると JWT の iss 検証が一致せず 401 になる（JWKS 側は別途 trim 済み）。
        config.auth.issuer = config.auth.issuer.trim_end_matches('/').to_string();

        config.validate()?;
        Ok(config)
    }

    /// 値の整合性を検証する（必須 URL のパース可否など）。
    pub fn validate(&self) -> Result<(), ConfigError> {
        Self::check_tenancy_supported(self.auth.tenancy)?;
        if self.auth.issuer.trim().is_empty() {
            return Err(ConfigError::Invalid("auth.issuer が空です".into()));
        }
        if self.auth.audience.trim().is_empty() {
            return Err(ConfigError::Invalid("auth.audience が空です".into()));
        }
        if self.database.url.trim().is_empty() {
            return Err(ConfigError::Invalid("database.url が空です".into()));
        }
        if self.auth.redirect_uri.trim().is_empty() {
            return Err(ConfigError::Invalid("auth.redirect_uri が空です".into()));
        }
        if self.session.redis_url.trim().is_empty() {
            return Err(ConfigError::Invalid("session.redis_url が空です".into()));
        }
        Self::check_session_bounds(&self.session)?;
        // 必須 URL。
        let mut urls: Vec<(&str, &str)> = vec![
            ("auth.issuer", self.auth.issuer.as_str()),
            ("authz.base_url", self.authz.base_url.as_str()),
            ("auth.redirect_uri", self.auth.redirect_uri.as_str()),
            (
                "auth.post_logout_redirect_uri",
                self.auth.post_logout_redirect_uri.as_str(),
            ),
        ];
        // 任意 URL（指定時のみ検証。不正値の起動後潜伏を防ぐ）。
        if let Some(url) = self.auth.internal_base_url.as_deref() {
            urls.push(("auth.internal_base_url", url));
        }
        if let Some(url) = self.auth.jwks_uri.as_deref() {
            urls.push(("auth.jwks_uri", url));
        }
        for (name, url) in urls {
            if reqwest::Url::parse(url).is_err() {
                return Err(ConfigError::Invalid(format!(
                    "{name} が URL として不正です: {url}"
                )));
            }
        }
        Ok(())
    }

    /// テナンシーモードが現状サポートされているか。
    ///
    /// multi-tenant（SaaS）は **全隔離層が tenant_id スコープ**になり、`auth.tenancy=multi` の明示設定
    /// だけで本番運用できる（旧 `SHIKI_DEV_ALLOW_MULTI_TENANT` ゲートは SAAS.1 完了で撤去）:
    /// - authz: OpenFGA 識別子を `<type>:<tenant_id>|<local>` へ名前空間化（`authz::Namespace`・#84）
    /// - storage: blob キー/PK を `{tenant_id}/{org}/...` へ（`content_address`・migration 0005）
    /// - audit: ハッシュチェーン探索と advisory ロックを tenant_id+org スコープへ
    /// - session: Redis キーを tenant_id スコープ、DB（storage/directory/outbox）は tenant_id 行分離
    /// - 解決時に `tenant_id` の禁止文字（`| : # @`・空白）を fail-closed 検証
    ///
    /// なおオンボーディング自動化・課金・クォータ（SAAS.2〜4）は**隔離の安全性とは独立の運用トラック**。
    // 現状は info ログのみで Err を返さないが、他の `check_*` バリデータと同じ
    // `-> Result<(), ConfigError>` 形を保ち `validate()` で一様に `?` 連結できるようにする。
    #[allow(clippy::unnecessary_wraps)]
    fn check_tenancy_supported(tenancy: Tenancy) -> Result<(), ConfigError> {
        if tenancy == Tenancy::Multi {
            tracing::info!(
                "auth.tenancy=multi: 全隔離層（authz 識別子名前空間化・storage キー・audit チェーン・\
                 session/DB 行）を tenant_id スコープで強制します（SAAS.1・#84 完了）"
            );
        }
        Ok(())
    }

    /// セッション数値設定の境界を検証する（失効/更新判定を壊す不正値を弾く）。
    fn check_session_bounds(session: &SessionConfig) -> Result<(), ConfigError> {
        if session.ttl_secs == 0 {
            return Err(ConfigError::Invalid(
                "session.ttl_secs は 1 以上が必要です".into(),
            ));
        }
        if session.refresh_leeway_secs < 0 {
            return Err(ConfigError::Invalid(
                "session.refresh_leeway_secs は 0 以上が必要です".into(),
            ));
        }
        Ok(())
    }
}
