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
    /// 地図の背景タイル（回答の ```ui の map・外部接続必須機能）。既定は無効。
    #[serde(default)]
    pub map: MapConfig,
    /// CSV クエリサービス（tabular・Task 11P.7）。既定値で有効（隔離 DuckDB ランナー）。
    #[serde(default)]
    pub tabular: TabularConfig,
    /// シークレット管理（Task 10.9）。既定は無効（マスターキー未設定なら /secrets は 503）。
    #[serde(default)]
    pub secrets: SecretsConfig,
    /// 構造化データ（Task 15.1: keyset カーソルの暗号鍵）。
    #[serde(default)]
    pub data: DataConfig,
    /// ワークフロー実行時（run ワーカー/スケジューラ/イベント relay・Task 10・Stage A W3）。
    /// 既定は無効（/workflows は保存のみ）。
    #[serde(default)]
    pub workflow: crate::workflow_runtime::WorkflowConfig,
    /// 公開 API ゲートウェイ（第2リスナ・別オリジン・Task 9.6）。既定は無効。
    #[serde(default)]
    pub gateway: GatewayConfig,
    /// Office 統合（Collabora＋WOPI ホスト・Task 11.5/11.6）。既定は無効。
    #[serde(default)]
    pub office: OfficeConfig,
    /// AI 活用分析の定期集計（#751）。既定は有効・1 時間ごと。
    #[serde(default)]
    pub insights: crate::insights::InsightsConfig,
}

/// Office 統合（Collabora＋WOPI ホスト）の設定（`SHIKI__OFFICE__*`）。
///
/// `enabled=false` なら `/wopi` も `/office/sessions` も配線しない（fail-closed）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficeConfig {
    /// Office 統合を有効化するか（既定 false）。
    #[serde(default)]
    pub enabled: bool,
    /// Collabora のベース URL（例 `http://localhost:9980`）。enabled 時は必須。
    #[serde(default)]
    pub collabora_base_url: Option<String>,
    /// **Collabora コンテナから見た** shiki-server のベース URL（例 `http://shiki-server:8080`）。
    /// WOPISrc（`{wopi_base_url}/wopi/files/{file_id}`）の組み立てに使う。ブラウザ側では
    /// 知り得ない値のためサーバが返す。enabled 時は必須（起動時検証・fail-closed）。
    #[serde(default)]
    pub wopi_base_url: Option<String>,
    /// WOPI access_token の署名秘密鍵（32 バイト以上）。未設定なら起動時乱数生成
    /// （再起動で編集セッション失効＝許容。複数レプリカでは設定注入が必須）。
    #[serde(default)]
    pub token_secret: Option<String>,
    /// ブラウザから見た web の origin（CheckFileInfo の PostMessageOrigin）。
    #[serde(default)]
    pub web_origin: Option<String>,
    /// ブラウザ ↔ Collabora の中継（PIT-68）を待ち受けるポート。既定 9980。
    ///
    /// ブラウザは Collabora に直接つながず、必ずこの中継を通す（スクリプト実行命令を落とすため）。
    /// Collabora の `server_name` はこの中継の公開ホストに合わせ、Collabora 自体のポートは
    /// ブラウザへ公開しない。`0` で中継を起動しない（Collabora を別の中継の裏に置く構成のみ）。
    #[serde(default = "default_office_relay_port")]
    pub relay_port: u16,
}

fn default_office_relay_port() -> u16 {
    9980
}

impl Default for OfficeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            collabora_base_url: None,
            wopi_base_url: None,
            token_secret: None,
            web_origin: None,
            relay_port: default_office_relay_port(),
        }
    }
}

/// 公開 API ゲートウェイ（ミニアプリ用・第2リスナ）の設定。
///
/// 内部 API（cookie セッション）とは**別ポート＝別オリジン**で待ち受ける。`enabled=false`
/// なら第2リスナを起動しない（既定・段階導入）。有効化には audience（トークンの aud 検証値）
/// が必要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// 第2リスナを起動するか（既定 false）。
    #[serde(default)]
    pub enabled: bool,
    /// バインドポート（内部 API とは別ポート＝別オリジン）。既定 8090。
    #[serde(default = "default_gateway_port")]
    pub port: u16,
    /// ゲートウェイトークンの `aud` 検証値（ミニアプリ client に付与される）。既定 `shiki-gateway`。
    #[serde(default = "default_gateway_audience")]
    pub audience: String,
    /// ミニアプリ AI（llm/agent.invoke）の日次予算・管理者キャップ（マイクロ USD・Task 9.9）。
    /// アプリのマニフェスト宣言との min が実効上限。既定 $5/日。
    #[serde(default = "default_ai_daily_cap")]
    pub ai_daily_cap_usd_micros: i64,
    /// B1 バンドル配信（第3リスナ・apps オリジン）のポート（Task 9.11）。既定 8091。
    #[serde(default = "default_b1_port")]
    pub b1_port: u16,
    /// ブラウザから見たゲートウェイのオリジン（CSP connect-src）。未設定は
    /// `http://localhost:<port>`（dev 既定）。
    #[serde(default)]
    pub public_origin: Option<String>,
    /// ホスト（web シェル）のオリジン（CSP frame-ancestors）。未設定は
    /// `auth.redirect_uri` のオリジンから導出。
    #[serde(default)]
    pub web_origin: Option<String>,
    /// 組み込み砂箱バンドル（スライドエディタ等）の配置ディレクトリ（Task 11.2）。
    /// 未設定なら組み込みバンドル配信は off（/builtin/* は 404）。
    #[serde(default)]
    pub builtin_dir: Option<String>,
}

fn default_gateway_port() -> u16 {
    8090
}

fn default_gateway_audience() -> String {
    "shiki-gateway".to_string()
}

fn default_ai_daily_cap() -> i64 {
    5_000_000
}

fn default_b1_port() -> u16 {
    8091
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            enabled: false,
            port: default_gateway_port(),
            audience: default_gateway_audience(),
            ai_daily_cap_usd_micros: default_ai_daily_cap(),
            b1_port: default_b1_port(),
            public_origin: None,
            web_origin: None,
            builtin_dir: None,
        }
    }
}

/// シークレット管理の設定。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretsConfig {
    /// マスターキーファイルのパス（32 バイト生 or base64）。未設定なら secrets 無効。
    /// クラウド KMS 実装（KeyProvider）は Stage B/Phase 8 で追加する。
    #[serde(default)]
    pub master_key_file: Option<String>,
}

/// 構造化データの設定（`SHIKI__DATA__*`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DataConfig {
    /// keyset カーソル（`record_int` を含む不透明トークン）の AES-256-GCM 鍵ファイルのパス。
    /// 形式は secrets のマスターキーと同じ（32 バイト生 or base64）。
    ///
    /// 鍵の決め方（Task 15.3・全レプリカで同じ鍵になること）:
    /// 1. `cursor_key_file` があればそれ
    /// 2. 無ければ `secrets.master_key_file` から用途別に導出（HMAC-SHA256・ドメイン分離ラベル付き）
    /// 3. どちらも無く `ephemeral_cursor_key = true` なら起動時乱数（**開発専用**: 再起動・レプリカ間で
    ///    カーソルが無効になる）
    /// 4. それ以外は**起動エラー**（複数レプリカでカーソルが壊れる構成を黙って起動させない）
    #[serde(default, deserialize_with = "empty_str_as_none")]
    pub cursor_key_file: Option<String>,
    /// 開発専用: カーソル鍵が設定されていなくても起動時乱数鍵で起動する（既定 false）。
    #[serde(default)]
    pub ephemeral_cursor_key: bool,
    /// レコード変更の SSE 差分配信（`SHIKI__DATA__STREAM__*`・Task 15.6・data-platform §9）。
    #[serde(default)]
    pub stream: DataStreamConfig,
}

/// SSE 差分配信の設定。未指定は `data::StreamConfig` の既定（TTL 5 秒・保持 10 分）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DataStreamConfig {
    /// 購読中の認可材料の TTL（ミリ秒・既定 5,000）。**剥奪の配信への反映はこれだけ遅れ得る**
    /// （PIT-70）ので上限は 60,000。ロール変更・共有解除・テーブル権限の変更は TTL を待たずに反映する。
    #[serde(default, deserialize_with = "empty_as_none_u64")]
    pub authz_ttl_ms: Option<u64>,
    /// ストリームの保持期間（秒・既定 600）。これより古い位置からの再開は `stream.reset`、
    /// スナップショット読取は 410。60〜86,400。
    #[serde(default, deserialize_with = "empty_as_none_u64")]
    pub retention_secs: Option<u64>,
}

/// 空文字（空白だけ）を未設定として読む。compose の `${VAR:-}` は未設定の変数を空文字で渡すため。
fn empty_str_as_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.filter(|s| !s.trim().is_empty()))
}

/// 数値の設定を、数値か数字の文字列で読む（空文字は未設定・[`empty_str_as_none`] と同じ理由）。
fn empty_as_none_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(u64),
        Str(String),
    }
    match Option::<NumOrStr>::deserialize(d)? {
        None => Ok(None),
        Some(NumOrStr::Num(n)) => Ok(Some(n)),
        Some(NumOrStr::Str(s)) if s.trim().is_empty() => Ok(None),
        Some(NumOrStr::Str(s)) => s.trim().parse().map(Some).map_err(serde::de::Error::custom),
    }
}

/// 認可材料の TTL の範囲（ミリ秒）。
const STREAM_AUTHZ_TTL_MS: std::ops::RangeInclusive<u64> = 100..=60_000;
/// 保持期間の範囲（秒）。
const STREAM_RETENTION_SECS: std::ops::RangeInclusive<u64> = 60..=86_400;

impl DataStreamConfig {
    /// `data::StreamConfig` へ写す（範囲外は起動エラー）。
    pub fn to_stream_config(&self) -> Result<data::StreamConfig, ConfigError> {
        let mut c = data::StreamConfig::default();
        if let Some(ms) = self.authz_ttl_ms {
            if !STREAM_AUTHZ_TTL_MS.contains(&ms) {
                return Err(ConfigError::Invalid(format!(
                    "data.stream.authz_ttl_ms は {}〜{} で指定してください（{ms}）",
                    STREAM_AUTHZ_TTL_MS.start(),
                    STREAM_AUTHZ_TTL_MS.end()
                )));
            }
            c.authz_ttl = std::time::Duration::from_millis(ms);
        }
        if let Some(secs) = self.retention_secs {
            if !STREAM_RETENTION_SECS.contains(&secs) {
                return Err(ConfigError::Invalid(format!(
                    "data.stream.retention_secs は {}〜{} で指定してください（{secs}）",
                    STREAM_RETENTION_SECS.start(),
                    STREAM_RETENTION_SECS.end()
                )));
            }
            c.retention = std::time::Duration::from_secs(secs);
        }
        Ok(c)
    }
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
mod data_tests;
mod map;
#[cfg(test)]
mod tests;

pub use auth::{AuthConfig, SessionConfig, Tenancy};
pub(crate) use backends::chatgpt_model_id;
pub use backends::{
    AuditRetentionConfig, AuthzConfig, ChatConfig, LangfuseConfig, LlmAutoConfig, LlmBackend,
    LlmChatGptConfig, LlmConfig, LlmLimitsConfig, LlmModelEntry, LlmProviderEntry,
    LlmProviderLimit, LogFormat, ObjectStoreBackend, StorageConfig, TabularConfig, TelemetryConfig,
    UploadGcConfig, VectorConfig, VectorStoreBackend, WebSearchBackend, WebSearchConfig,
};
pub use map::{MapClientConfig, MapConfig, MapProvider};

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
        "storage": { "backend": "s3" },
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
        Self::check_tenancy_supported(&self.auth)?;
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
        Self::check_upload_gc_bounds(&self.storage)?;
        Self::check_llm_timeouts(&self.llm)?;
        Self::check_llm_limits(&self.llm)?;
        self.data.stream.to_stream_config()?;
        Self::check_llm_chatgpt(&self.llm)?;
        self.map.validate()?;
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
    ///
    /// `single` で provisioner 資格情報が揃っている場合は警告だけ出す（#808）。provisioner は
    /// ミニアプリ同意インストールの Keycloak client 登録（オンプレでも使う）を兼ねるため起動は
    /// 止めず、テナント作成（`POST /admin/tenants`）だけをハンドラ側で fail-closed に拒否する。
    // 現状はログのみで Err を返さないが、他の `check_*` バリデータと同じ
    // `-> Result<(), ConfigError>` 形を保ち `validate()` で一様に `?` 連結できるようにする。
    #[allow(clippy::unnecessary_wraps)]
    fn check_tenancy_supported(auth: &AuthConfig) -> Result<(), ConfigError> {
        match auth.tenancy {
            Tenancy::Multi => tracing::info!(
                "auth.tenancy=multi: 全隔離層（authz 識別子名前空間化・storage キー・audit チェーン・\
                 session/DB 行）を tenant_id スコープで強制します（SAAS.1・#84 完了）"
            ),
            Tenancy::Single if auth.provisioner_credentials().is_some() => tracing::warn!(
                tenant_id = ?auth.tenant_id,
                "auth.tenancy=single で provisioner が設定されています: 全ログインは固定の \
                 auth.tenant_id へ解決され、POST /admin/tenants は拒否されます（SaaS なら \
                 auth.tenancy=multi を設定してください）"
            ),
            Tenancy::Single => {}
        }
        Ok(())
    }

    /// LLM のタイムアウトを検証する（0 秒は全呼び出しを即時に失敗させるため弾く・#554）。
    fn check_llm_timeouts(llm: &LlmConfig) -> Result<(), ConfigError> {
        for (name, secs) in [
            (
                "llm.first_response_timeout_secs",
                llm.first_response_timeout_secs,
            ),
            ("llm.idle_timeout_secs", llm.idle_timeout_secs),
            ("llm.total_timeout_secs", llm.total_timeout_secs),
        ] {
            if secs == 0 {
                return Err(ConfigError::Invalid(format!("{name} は 1 以上が必要です")));
            }
        }
        Ok(())
    }

    /// ChatGPT プラン（#854）の設定を検証する（有効なのに使えるモデルが無い・ID の衝突を弾く）。
    fn check_llm_chatgpt(llm: &LlmConfig) -> Result<(), ConfigError> {
        let c = &llm.chatgpt;
        if !c.enabled {
            return Ok(());
        }
        if c.credential_path.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "llm.chatgpt.credential_path が空です".into(),
            ));
        }
        if c.models.is_empty() {
            return Err(ConfigError::Invalid(
                "llm.chatgpt.models に 1 つ以上のモデル（slug）が必要です".into(),
            ));
        }
        if llm
            .providers
            .iter()
            .any(|p| p.id == llm_gateway::CHATGPT_PROVIDER_ID)
        {
            return Err(ConfigError::Invalid(format!(
                "llm.providers[].id の {} は llm.chatgpt が使うため指定できません",
                llm_gateway::CHATGPT_PROVIDER_ID
            )));
        }
        let ids: Vec<String> = c
            .models
            .iter()
            .map(|s| backends::chatgpt_model_id(s))
            .collect();
        if let Some(dup) = ids
            .iter()
            .enumerate()
            .find(|(i, id)| {
                // `models` が空なら、既定モデルと同じ ID の合成エントリが先頭に入る（先勝ちで引かれる）。
                let synthesized =
                    llm.models.is_empty() && llm.default_model.as_deref() == Some(id.as_str());
                ids[..*i].contains(id) || llm.models.iter().any(|m| &m.id == *id) || synthesized
            })
            .map(|(_, id)| id)
        {
            return Err(ConfigError::Invalid(format!(
                "llm.chatgpt.models のモデル ID が重複しています: {dup}"
            )));
        }
        Ok(())
    }

    /// 利用制限の値を検証する（金額クリティカル・効かない設定で起動させない）。
    ///
    /// 負の上限は即座に全員を止めるので弾く。プロバイダ ID の綴り違いは**黙って上限が
    /// 掛からない**状態を作るため、`llm.providers[].id`（と既定プロバイダ）に無い ID を
    /// 指していたら起動を止める（fail-closed）。
    fn check_llm_limits(llm: &LlmConfig) -> Result<(), ConfigError> {
        if llm.limits.per_user_usd_micros.is_some_and(|v| v < 0) {
            return Err(ConfigError::Invalid(
                "llm.limits.per_user_usd_micros は 0 以上が必要です".into(),
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for entry in &llm.limits.providers {
            if entry.usd_micros < 0 {
                return Err(ConfigError::Invalid(
                    "llm.limits.providers[].usd_micros は 0 以上が必要です".into(),
                ));
            }
            if entry.providers.is_empty() {
                return Err(ConfigError::Invalid(
                    "llm.limits.providers[].providers は 1 つ以上の ID が必要です".into(),
                ));
            }
            for id in &entry.providers {
                let known = id == llm_gateway::DEFAULT_PROVIDER_ID
                    || llm.providers.iter().any(|q| &q.id == id)
                    // ChatGPT プラン（#854）は有効なとき予約 ID で自動登録される。
                    || (llm.chatgpt.enabled && id == llm_gateway::CHATGPT_PROVIDER_ID);
                if !known {
                    return Err(ConfigError::Invalid(format!(
                        "llm.limits.providers に未知のプロバイダ ID があります: {id}\
                         （llm.providers[].id か {} を指定）",
                        llm_gateway::DEFAULT_PROVIDER_ID
                    )));
                }
                // 同じ ID が 2 つの枠に属すとどちらで数えるか決まらない（先勝ちで黙って
                // 緩い方へ倒れ得る）ので弾く。
                if seen.contains(&id.as_str()) {
                    return Err(ConfigError::Invalid(format!(
                        "llm.limits.providers でプロバイダ ID が重複しています: {id}"
                    )));
                }
                seen.push(id);
            }
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

    /// 中断アップロード回収（#468）の数値設定の境界を検証する。
    ///
    /// この GC は**全テナントの pending_upload とオブジェクトを消す**ので、不正値の影響が
    /// 設定ミスの範囲に留まらない。
    ///
    /// - `ttl_secs = 0` を通すと、起動直後の初回 sweep が全テナントの pending_upload を
    ///   一掃する。その瞬間に PUT 中だった全ユーザーの finalize が NotFound になり、
    ///   アップロード済みのバイトは復旧できない。
    /// - `interval_secs = 0` を通すと、sweep が休みなく連続実行され、DB とオブジェクト
    ///   ストアを自前で飽和させる。
    /// - presigned PUT の TTL 以下だと、URL がまだ有効なうちに declare が回収される。
    ///   低速回線での大きいファイルほど確実に踏む。
    fn check_upload_gc_bounds(storage: &StorageConfig) -> Result<(), ConfigError> {
        let gc = &storage.upload_gc;
        if gc.ttl_secs == 0 {
            return Err(ConfigError::Invalid(
                "storage.upload_gc.ttl_secs は 1 以上が必要です（0 は進行中のアップロードを\
                 即座に回収します）"
                    .into(),
            ));
        }
        if gc.interval_secs == 0 {
            return Err(ConfigError::Invalid(
                "storage.upload_gc.interval_secs は 1 以上が必要です".into(),
            ));
        }
        if let Some(s3) = storage.s3.as_ref() {
            if gc.ttl_secs <= s3.presign_put_ttl_secs {
                return Err(ConfigError::Invalid(format!(
                    "storage.upload_gc.ttl_secs ({}) は storage.s3.presign_put_ttl_secs ({}) より\
                     大きい必要があります（URL がまだ有効なうちに declare を回収してしまいます）",
                    gc.ttl_secs, s3.presign_put_ttl_secs
                )));
            }
        }
        Ok(())
    }
}
