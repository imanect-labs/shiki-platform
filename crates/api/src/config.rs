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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// OIDC issuer（Keycloak realm URL。必須。トークンの `iss` 検証値であり、
    /// ブラウザがログインで到達する**公開 URL**でもある）。
    pub issuer: String,
    /// サーバ側 OIDC 呼び出し（token 交換・end-session backchannel）に使う**内部 base URL**。
    /// 未指定なら `issuer` を使う。compose では公開 URL（issuer）がコンテナ内から引けないため、
    /// `http://keycloak:8080/realms/shiki` のような内部 URL を指定する（JWKS の内部 URL 指定と同様）。
    pub internal_base_url: Option<String>,
    /// JWKS エンドポイント。未指定なら `internal_base_url`（無ければ issuer）から導出する。
    pub jwks_uri: Option<String>,
    /// アクセストークンの `aud` 検証値（必須）。
    pub audience: String,
    /// JWKS キャッシュの TTL（秒）。
    pub jwks_ttl_secs: u64,
    /// BFF（confidential client）の client_id。既定 `"shiki-web"`。
    pub client_id: String,
    /// BFF（confidential client）の client_secret。BFF はサーバ側でのみ保持する。
    pub client_secret: Option<String>,
    /// OIDC code フローのブラウザ向け redirect_uri（callback の登録 URL）。
    /// 既定はローカル開発の `http://localhost:3000/auth/callback`（Next rewrites で同一オリジン）。
    pub redirect_uri: String,
    /// ログアウト後のブラウザ向けリダイレクト先。既定 `http://localhost:3000/`。
    pub post_logout_redirect_uri: String,
    /// 要求スコープ（スペース区切り）。既定 `"openid profile"`。
    pub scopes: String,
    /// テナンシーモード（`single`=オンプレ/cell・`multi`=SaaS）。既定 `single`。
    /// `resolve_tenant_id` の解決戦略を分岐し、SaaS では claim 欠落を fail-closed にする。
    pub tenancy: Tenancy,
    /// `single` モードのテナント固定値（案C）。オンプレ/cell のシングルテナントで使う。
    /// 既定 `"default"`。`multi` モードでは使わず claim `tenant` を必須にする（案A）。
    pub tenant_id: Option<String>,
}

/// BFF セッション（オパーク Cookie + Redis）の設定。
///
/// ブラウザにはトークンを置かず、`session.cookie_name` の不透明セッション ID のみを渡す。
/// セッション本体（principal/claims/OIDC token/expiry）は Redis に `tenant_id` スコープで保持する。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Redis 接続 URL（例 `redis://redis:6379`）。
    pub redis_url: String,
    /// セッション TTL（秒）。既定 86400（24h）。
    pub ttl_secs: u64,
    /// Cookie の `Secure` 属性。本番(HTTPS)は true、ローカル HTTP 開発のみ false。既定 true。
    pub secure: bool,
    /// access token の期限が残りこの秒数を切ったらサーバ側で refresh する閾値。既定 60。
    pub refresh_leeway_secs: i64,
}

impl AuthConfig {
    /// サーバ側 OIDC 呼び出しの base URL（`internal_base_url` 優先・末尾スラッシュ除去）。
    fn backchannel_base(&self) -> String {
        self.internal_base_url
            .as_deref()
            .unwrap_or(&self.issuer)
            .trim_end_matches('/')
            .to_string()
    }

    /// ブラウザ向け authorize エンドポイント（公開 issuer 由来）。
    pub fn authorize_endpoint(&self) -> String {
        format!(
            "{}/protocol/openid-connect/auth",
            self.issuer.trim_end_matches('/')
        )
    }

    /// サーバ側 token エンドポイント（内部 base 由来。code 交換・refresh で使う）。
    pub fn token_endpoint(&self) -> String {
        format!("{}/protocol/openid-connect/token", self.backchannel_base())
    }

    /// ブラウザ向け end-session エンドポイント（公開 issuer 由来）。
    pub fn end_session_endpoint(&self) -> String {
        format!(
            "{}/protocol/openid-connect/logout",
            self.issuer.trim_end_matches('/')
        )
    }
}

/// テナンシーモード。`tenant_id` の取得元（案A/案C）を決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tenancy {
    /// オンプレ/cell シングルテナント。固定値 `auth.tenant_id`（案C）を使う。
    Single,
    /// SaaS マルチテナント。Keycloak claim `tenant`（案A）を必須にし、欠落は fail-closed。
    Multi,
}

impl AuthConfig {
    /// 実効 JWKS URI。`jwks_uri` 未指定なら内部 base（無ければ issuer）から OIDC 規約で導出する。
    /// JWKS はサーバ側 backchannel で取得するため内部 base を優先する。
    pub fn effective_jwks_uri(&self) -> String {
        self.jwks_uri
            .clone()
            .unwrap_or_else(|| format!("{}/protocol/openid-connect/certs", self.backchannel_base()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthzConfig {
    /// OpenFGA HTTP API ベース URL（必須）。
    pub base_url: String,
    pub store_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// OTLP エクスポート先（例: `http://otel-collector:4317`）。未指定なら OTel 無効。
    pub otlp_endpoint: Option<String>,
    pub service_name: String,
    /// ログ出力形式（`json` or `pretty`）。
    pub log_format: LogFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub backend: ObjectStoreBackend,
    /// MinIO/S3 接続設定（`backend=minio` のとき必須。起動時に main で検証）。
    #[serde(default)]
    pub s3: Option<storage::S3Config>,
    /// 1 ファイルの最大アップロードサイズ（バイト）。既定 5 GiB。declare の宣言サイズが
    /// これを超えたら拒否し、容量枯渇（認証ユーザーによる無制限アップロード）を防ぐ。
    #[serde(default = "default_max_upload_size_bytes")]
    pub max_upload_size_bytes: i64,
}

fn default_max_upload_size_bytes() -> i64 {
    5 * 1024 * 1024 * 1024 // 5 GiB
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStoreBackend {
    Minio,
    Gcs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorConfig {
    pub backend: VectorStoreBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorStoreBackend {
    Qdrant,
    Pgvector,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub backend: LlmBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackend {
    Vllm,
    Anthropic,
    Gemini,
    Vertex,
}

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
    /// multi-tenant（SaaS）の session tenant 解決は session Cookie へのテナントスコープ束ね
    /// （`session::encode_session_cookie`）で単一ホストでも成立する。claim `tenant` 由来の
    /// `tenant_id` で Postgres（storage/directory/audit/outbox）を分離する。
    ///
    /// ただし OpenFGA の subject/object 識別子の tenant 名前空間化（roadmap SAAS.1）は未了で、
    /// authz ストアは共用のまま（ノード ID は UUID で衝突しないが、防御的多層化は SAAS.1 の責務）。
    /// 本番クラウドでは host ベースの tenant ルーティング＋FGA 名前空間化を SAAS.1 で仕上げること。
    fn check_tenancy_supported(tenancy: Tenancy) -> Result<(), ConfigError> {
        if tenancy == Tenancy::Multi {
            tracing::warn!(
                "auth.tenancy=multi: Postgres レイヤで tenant_id 分離を強制します。\
                 OpenFGA の tenant 名前空間化（SAAS.1）は未了のため、本番では host ルーティングと\
                 FGA 名前空間化を仕上げること"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_tenancy_modes_are_supported() {
        // multi は session Cookie へのテナントスコープ束ね＋Postgres tenant_id 分離で起動可能。
        // FGA 名前空間化（SAAS.1）は残課題だが起動はブロックしない。
        assert!(AppConfig::check_tenancy_supported(Tenancy::Multi).is_ok());
        assert!(AppConfig::check_tenancy_supported(Tenancy::Single).is_ok());
    }

    fn session(ttl_secs: u64, refresh_leeway_secs: i64) -> SessionConfig {
        SessionConfig {
            redis_url: "redis://localhost:6379".into(),
            ttl_secs,
            secure: true,
            refresh_leeway_secs,
        }
    }

    #[test]
    fn session_bounds_reject_invalid_numbers() {
        assert!(AppConfig::check_session_bounds(&session(86400, 60)).is_ok());
        // ttl_secs=0 は失効しないセッションになり危険。
        assert!(AppConfig::check_session_bounds(&session(0, 60)).is_err());
        // 負の leeway は refresh 判定を壊す。
        assert!(AppConfig::check_session_bounds(&session(86400, -1)).is_err());
        // leeway=0（境界）は許容する。
        assert!(AppConfig::check_session_bounds(&session(86400, 0)).is_ok());
    }

    // ---- AuthConfig のエンドポイント導出 ----

    /// テスト用 AuthConfig。`issuer`/`internal_base_url`/`jwks_uri` を差し替えて使う。
    fn auth_config(
        issuer: &str,
        internal_base_url: Option<&str>,
        jwks_uri: Option<&str>,
    ) -> AuthConfig {
        AuthConfig {
            issuer: issuer.into(),
            internal_base_url: internal_base_url.map(str::to_string),
            jwks_uri: jwks_uri.map(str::to_string),
            audience: "shiki-api".into(),
            jwks_ttl_secs: 300,
            client_id: "shiki-web".into(),
            client_secret: None,
            redirect_uri: "http://localhost:3000/auth/callback".into(),
            post_logout_redirect_uri: "http://localhost:3000/".into(),
            scopes: "openid profile".into(),
            tenancy: Tenancy::Single,
            tenant_id: Some("default".into()),
        }
    }

    #[test]
    fn authorize_endpoint_uses_public_issuer() {
        // authorize はブラウザ向け＝公開 issuer 由来で導出する。
        let cfg = auth_config(
            "https://kc.example.com/realms/shiki",
            Some("http://keycloak:8080/realms/shiki"),
            None,
        );
        assert_eq!(
            cfg.authorize_endpoint(),
            "https://kc.example.com/realms/shiki/protocol/openid-connect/auth"
        );
    }

    #[test]
    fn authorize_endpoint_trims_trailing_slash() {
        // issuer 末尾スラッシュが二重 `//` を生まないこと。
        let cfg = auth_config("https://kc.example.com/realms/shiki/", None, None);
        assert_eq!(
            cfg.authorize_endpoint(),
            "https://kc.example.com/realms/shiki/protocol/openid-connect/auth"
        );
    }

    #[test]
    fn end_session_endpoint_uses_public_issuer() {
        // end-session もブラウザ向け＝公開 issuer 由来。
        let cfg = auth_config("https://kc.example.com/realms/shiki", None, None);
        assert_eq!(
            cfg.end_session_endpoint(),
            "https://kc.example.com/realms/shiki/protocol/openid-connect/logout"
        );
    }

    #[test]
    fn token_endpoint_prefers_internal_base() {
        // token はサーバ側 backchannel＝内部 base 由来（公開 issuer ではない）。
        let cfg = auth_config(
            "https://kc.example.com/realms/shiki",
            Some("http://keycloak:8080/realms/shiki"),
            None,
        );
        assert_eq!(
            cfg.token_endpoint(),
            "http://keycloak:8080/realms/shiki/protocol/openid-connect/token"
        );
    }

    #[test]
    fn token_endpoint_falls_back_to_issuer() {
        // internal_base_url 未指定なら issuer にフォールバックする。
        let cfg = auth_config("https://kc.example.com/realms/shiki", None, None);
        assert_eq!(
            cfg.token_endpoint(),
            "https://kc.example.com/realms/shiki/protocol/openid-connect/token"
        );
    }

    #[test]
    fn backchannel_base_trims_trailing_slash() {
        // internal_base_url の末尾スラッシュは除去される（token_endpoint 経由で確認）。
        let cfg = auth_config(
            "https://kc.example.com/realms/shiki",
            Some("http://keycloak:8080/realms/shiki/"),
            None,
        );
        assert_eq!(
            cfg.token_endpoint(),
            "http://keycloak:8080/realms/shiki/protocol/openid-connect/token"
        );
    }

    #[test]
    fn effective_jwks_uri_explicit_takes_priority() {
        // 明示指定の jwks_uri はそのまま使う。
        let cfg = auth_config(
            "https://kc.example.com/realms/shiki",
            Some("http://keycloak:8080/realms/shiki"),
            Some("http://keycloak:8080/realms/shiki/protocol/openid-connect/certs"),
        );
        assert_eq!(
            cfg.effective_jwks_uri(),
            "http://keycloak:8080/realms/shiki/protocol/openid-connect/certs"
        );
    }

    #[test]
    fn effective_jwks_uri_derives_from_internal_base() {
        // jwks_uri 未指定なら内部 base から OIDC 規約で導出する。
        let cfg = auth_config(
            "https://kc.example.com/realms/shiki",
            Some("http://keycloak:8080/realms/shiki"),
            None,
        );
        assert_eq!(
            cfg.effective_jwks_uri(),
            "http://keycloak:8080/realms/shiki/protocol/openid-connect/certs"
        );
    }

    #[test]
    fn effective_jwks_uri_derives_from_issuer_when_no_internal() {
        // 内部 base も無ければ issuer から導出する。
        let cfg = auth_config("https://kc.example.com/realms/shiki", None, None);
        assert_eq!(
            cfg.effective_jwks_uri(),
            "https://kc.example.com/realms/shiki/protocol/openid-connect/certs"
        );
    }

    // ---- AppConfig::validate() の各失敗分岐 ----

    /// defaults() を素に valid な AppConfig を組み立てる（必須項目を補完）。
    fn valid_config() -> AppConfig {
        let mut value = defaults();
        // 必須項目（defaults に含まれない）を補完する。
        value["database"]["url"] = serde_json::json!("postgres://localhost/shiki");
        value["auth"]["issuer"] = serde_json::json!("http://localhost/realms/shiki");
        value["auth"]["audience"] = serde_json::json!("shiki-api");
        value["authz"]["base_url"] = serde_json::json!("http://localhost:8081");
        value["authz"]["store_name"] = serde_json::json!("shiki");
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn valid_config_passes_validation() {
        // 健全な構成は validate を通過する（負例の対照）。
        assert!(valid_config().validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_issuer() {
        let mut cfg = valid_config();
        cfg.auth.issuer = "   ".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_audience() {
        let mut cfg = valid_config();
        cfg.auth.audience = String::new();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_database_url() {
        let mut cfg = valid_config();
        cfg.database.url = "  ".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_redirect_uri() {
        let mut cfg = valid_config();
        cfg.auth.redirect_uri = String::new();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_redis_url() {
        let mut cfg = valid_config();
        cfg.session.redis_url = "".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_url() {
        // 必須 URL が URL として不正なら拒否する。
        let mut cfg = valid_config();
        cfg.authz.base_url = "not a url".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_optional_url() {
        // 任意 URL（internal_base_url）も指定時は検証され、不正なら拒否する。
        let mut cfg = valid_config();
        cfg.auth.internal_base_url = Some("::::not-a-url".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_jwks_uri() {
        let mut cfg = valid_config();
        cfg.auth.jwks_uri = Some("htttp//missing-colon".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_ttl() {
        let mut cfg = valid_config();
        cfg.session.ttl_secs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_negative_leeway() {
        let mut cfg = valid_config();
        cfg.session.refresh_leeway_secs = -5;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_multi_tenancy() {
        // tenancy=multi は session Cookie スコープ束ね＋Postgres tenant_id 分離で起動可能。
        let mut cfg = valid_config();
        cfg.auth.tenancy = Tenancy::Multi;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn tenancy_serde_round_trip() {
        // snake_case でシリアライズ/デシリアライズされること。
        assert_eq!(
            serde_json::to_string(&Tenancy::Single).unwrap(),
            "\"single\""
        );
        let t: Tenancy = serde_json::from_str("\"multi\"").unwrap();
        assert_eq!(t, Tenancy::Multi);
    }

    #[test]
    fn log_format_serde_round_trip() {
        // LogFormat も snake_case 表現。
        assert_eq!(serde_json::to_string(&LogFormat::Json).unwrap(), "\"json\"");
        let f: LogFormat = serde_json::from_str("\"pretty\"").unwrap();
        assert_eq!(f, LogFormat::Pretty);
    }

    #[test]
    fn backend_enums_serde_round_trip() {
        // 差し替え点 enum の serde 表現を固定する。
        let b: ObjectStoreBackend = serde_json::from_str("\"gcs\"").unwrap();
        assert_eq!(b, ObjectStoreBackend::Gcs);
        let v: VectorStoreBackend = serde_json::from_str("\"pgvector\"").unwrap();
        assert_eq!(v, VectorStoreBackend::Pgvector);
        let l: LlmBackend = serde_json::from_str("\"anthropic\"").unwrap();
        assert_eq!(l, LlmBackend::Anthropic);
    }

    #[test]
    fn defaults_deserialize_into_partial_config() {
        // defaults() が想定キーを含むこと（load 相当の素材として健全）。
        let value = defaults();
        assert_eq!(value["auth"]["tenancy"], serde_json::json!("single"));
        assert_eq!(value["session"]["ttl_secs"], serde_json::json!(86400));
        assert_eq!(value["telemetry"]["log_format"], serde_json::json!("json"));
    }
}
