//! `config` モジュールのユニットテスト（本体から分離・巨大ファイル回避）。

use super::*;

#[test]
fn both_tenancies_supported() {
    // SAAS.1（#84）で全隔離層が tenant_id スコープになり、multi は設定だけで運用可能
    // （旧 dev opt-in ゲートは撤去）。single も従来どおり可。
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
        provisioner_client_id: None,
        provisioner_client_secret: None,
        admin_base_url: None,
    }
}

#[test]
fn admin_base_derivation() {
    // 内部 base（realm URL）から `{root}/admin/realms/{realm}` を導出する。
    let cfg = auth_config(
        "https://kc.example.com/realms/shiki",
        Some("http://keycloak:8080/realms/shiki"),
        None,
    );
    assert_eq!(
        cfg.admin_base().as_deref(),
        Some("http://keycloak:8080/admin/realms/shiki")
    );
    // 明示上書きが最優先（末尾スラッシュは除去）。
    let mut cfg2 = auth_config("https://kc.example.com/realms/shiki", None, None);
    cfg2.admin_base_url = Some("http://proxy:9999/admin/realms/shiki/".into());
    assert_eq!(
        cfg2.admin_base().as_deref(),
        Some("http://proxy:9999/admin/realms/shiki")
    );
    // realm セグメントが無い URL からは導出できない（fail-closed で None）。
    let cfg3 = auth_config("http://idp.example.com/oauth", None, None);
    assert_eq!(cfg3.admin_base(), None);
}

#[test]
fn account_base_prefers_internal_base() {
    // サーバ側から叩くので内部 base 優先。無ければ issuer 由来（末尾スラッシュは除去）。
    let cfg = auth_config(
        "https://kc.example.com/realms/shiki",
        Some("http://keycloak:8080/realms/shiki/"),
        None,
    );
    assert_eq!(
        cfg.account_base(),
        "http://keycloak:8080/realms/shiki/account"
    );
    let cfg2 = auth_config("https://kc.example.com/realms/shiki", None, None);
    assert_eq!(
        cfg2.account_base(),
        "https://kc.example.com/realms/shiki/account"
    );
}

#[test]
fn provisioner_credentials_require_both() {
    let mut cfg = auth_config("https://kc.example.com/realms/shiki", None, None);
    assert_eq!(cfg.provisioner_credentials(), None);
    cfg.provisioner_client_id = Some("shiki-provisioner".into());
    assert_eq!(cfg.provisioner_credentials(), None, "secret 無しでは無効");
    cfg.provisioner_client_secret = Some("s3cret".into());
    assert_eq!(
        cfg.provisioner_credentials(),
        Some(("shiki-provisioner", "s3cret"))
    );
    // 空文字は未設定扱い（fail-closed）。
    cfg.provisioner_client_secret = Some("".into());
    assert_eq!(cfg.provisioner_credentials(), None);
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
fn validate_rejects_zero_upload_gc_ttl() {
    // ttl_secs=0 は初回 sweep が全テナントの進行中アップロードを一掃する（#468）。
    let mut cfg = valid_config();
    cfg.storage.upload_gc.ttl_secs = 0;
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_rejects_zero_upload_gc_interval() {
    // interval_secs=0 は sweep が休みなく回り、DB/オブジェクトストアを飽和させる（#468）。
    let mut cfg = valid_config();
    cfg.storage.upload_gc.interval_secs = 0;
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_rejects_upload_gc_ttl_not_longer_than_presign_put() {
    // presigned PUT がまだ有効なうちに declare を回収する設定を起動時に弾く（#468）。
    let mut cfg = valid_config();
    cfg.storage.s3 = Some(storage::S3Config {
        internal_endpoint: "http://minio:9000".into(),
        public_endpoint: "http://localhost:9000".into(),
        bucket: "shiki".into(),
        access_key: "key".into(),
        secret_key: "secret".into(),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 120,
        presign_put_ttl_secs: 3600,
        cors_allowed_origins: vec![],
    });

    cfg.storage.upload_gc.ttl_secs = 3600; // 同値も不可（URL 有効中に消える）。
    assert!(cfg.validate().is_err());
    cfg.storage.upload_gc.ttl_secs = 3601;
    assert!(cfg.validate().is_ok());
}

#[test]
fn validate_accepts_multi_tenancy() {
    // SAAS.1（#84）で全隔離層が tenant_id スコープになり、tenancy=multi は設定だけで validate を通る
    // （旧 dev opt-in ゲートは撤去）。
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

#[test]
fn object_store_backend_parses_canonical_and_legacy_names() {
    // 正式名。実装が S3 互換汎用（S3ObjectStore）なので enum も S3。
    assert_eq!(
        serde_json::from_str::<ObjectStoreBackend>(r#""s3""#).unwrap(),
        ObjectStoreBackend::S3
    );
    // **旧名 `minio` を受け付け続けること。** RustFS へ差し替えるときに
    // enum を Minio → S3 へ改名したが、既存の .env / TOML に
    // `SHIKI__STORAGE__BACKEND=minio` が書かれている環境を壊してはいけない。
    // ここが落ちたら、既存環境は起動時の設定パースで死ぬ。
    assert_eq!(
        serde_json::from_str::<ObjectStoreBackend>(r#""minio""#).unwrap(),
        ObjectStoreBackend::S3
    );
    assert_eq!(
        serde_json::from_str::<ObjectStoreBackend>(r#""gcs""#).unwrap(),
        ObjectStoreBackend::Gcs
    );
    // 知らない値は黙って既定に落とさず失敗させる（設定ミスを起動時に出す）。
    assert!(serde_json::from_str::<ObjectStoreBackend>(r#""rustfs""#).is_err());
}

// ---- llm.model_catalog（env だけの構成で既定モデルの窓を渡す・#529） ----

#[test]
fn context_window_reaches_synthesized_default_model() {
    let window = |w: Option<u32>| {
        let mut cfg = valid_config();
        cfg.llm.models.clear();
        cfg.llm.default_model = Some("m".into());
        cfg.llm.context_window_tokens = w;
        let catalog = cfg.llm.model_catalog();
        assert_eq!(catalog.default_model, "m");
        catalog
            .default_entry()
            .and_then(|e| e.context_window_tokens)
    };
    assert_eq!(window(Some(32_768)), Some(32_768));
    // 0 は未設定（Compose の env 既定値）。窓を知らない扱いで固定閾値のまま。
    assert_eq!(window(Some(0)), None);
    assert_eq!(window(None), None);
}

#[test]
fn explicit_catalog_keeps_per_model_windows() {
    let mut cfg = valid_config();
    cfg.llm.context_window_tokens = Some(8_192); // カタログがあれば使わない
    cfg.llm.models = vec![LlmModelEntry {
        id: "big".into(),
        real_id: None,
        prompt_price_micros_per_mtok: 0,
        completion_price_micros_per_mtok: 0,
        context_window_tokens: Some(200_000),
        ..Default::default()
    }];
    let catalog = cfg.llm.model_catalog();
    assert_eq!(catalog.default_model, "big");
    assert_eq!(catalog.models.len(), 1);
    assert_eq!(catalog.models[0].context_window_tokens, Some(200_000));
}

#[test]
fn chat_yui_defaults_on() {
    // 節ごと省略しても、節はあって yui だけ省略しても既定オン（#530）。明示の false は効く。
    assert!(ChatConfig::default().yui);
    let partial: ChatConfig =
        serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
    assert!(partial.yui);
    let off: ChatConfig = serde_json::from_value(serde_json::json!({ "yui": false })).unwrap();
    assert!(!off.yui);
}

#[test]
fn catalog_carries_provider_and_display_metadata() {
    // 複数プロバイダ（#553）: モデルの振り先・表示名・国外処理・思考強度の指定が gateway へ届く。
    let mut cfg = valid_config();
    cfg.llm.models = vec![LlmModelEntry {
        id: "gpt-6-luna".into(),
        provider: Some("opencode".into()),
        display_name: Some("GPT-6 Luna".into()),
        description: Some("速い".into()),
        processed_abroad: true,
        supports_effort: Some(true),
        reasoning_in_content: true,
        ..Default::default()
    }];
    let m = &cfg.llm.model_catalog().models[0];
    assert_eq!(m.provider_id(), "opencode");
    assert_eq!(m.label(), "GPT-6 Luna");
    assert_eq!(m.description.as_deref(), Some("速い"));
    assert!(m.processed_abroad);
    assert_eq!(m.supports_effort, Some(true));
    assert!(m.reasoning_in_content);
}

#[test]
fn catalog_reads_cache_prices_from_toml() {
    // キャッシュ単価（#581）: TOML のキーがそのまま gateway のカタログへ届き、省略は None
    // （＝プロンプト単価で数える）。
    use figment::providers::{Format, Toml};
    let toml = r#"
        [[models]]
        id = "luna"
        prompt_price_micros_per_mtok = 200000
        completion_price_micros_per_mtok = 1200000
        cache_read_price_micros_per_mtok = 20000
        cache_write_price_micros_per_mtok = 250000

        [[models]]
        id = "plain"
        prompt_price_micros_per_mtok = 100000
    "#;
    #[derive(serde::Deserialize)]
    struct Models {
        models: Vec<LlmModelEntry>,
    }
    let parsed: Models = figment::Figment::from(Toml::string(toml))
        .extract()
        .expect("toml");
    let mut cfg = valid_config();
    cfg.llm.models = parsed.models;
    let catalog = cfg.llm.model_catalog();
    let luna = catalog.get("luna").expect("luna");
    assert_eq!(luna.cache_read_price(), 20_000);
    assert_eq!(luna.cache_write_price(), 250_000);
    let plain = catalog.get("plain").expect("plain");
    assert_eq!(plain.cache_read_price_micros_per_mtok, None);
    assert_eq!(plain.cache_read_price(), 100_000);
    assert_eq!(plain.cache_write_price(), 100_000);
}

#[test]
fn llm_timeouts_default_and_reach_the_gateway() {
    // 省略時は 60 秒 / 120 秒 / 15 分（#554）。
    let llm: LlmConfig = serde_json::from_value(serde_json::json!({ "backend": "stub" })).unwrap();
    assert_eq!(
        llm.timeouts(),
        llm_gateway::LlmTimeouts::from_secs(60, 120, 900)
    );
    assert_eq!(
        LlmConfig::default().timeouts(),
        llm_gateway::LlmTimeouts::default()
    );
    let llm: LlmConfig = serde_json::from_value(serde_json::json!({
        "backend": "stub",
        "first_response_timeout_secs": 5,
        "idle_timeout_secs": 30,
        "total_timeout_secs": 3600,
    }))
    .unwrap();
    assert_eq!(
        llm.timeouts(),
        llm_gateway::LlmTimeouts::from_secs(5, 30, 3600)
    );
}

#[test]
fn zero_llm_timeouts_are_rejected() {
    for name in [
        "first_response_timeout_secs",
        "idle_timeout_secs",
        "total_timeout_secs",
    ] {
        let mut cfg = valid_config();
        match name {
            "first_response_timeout_secs" => cfg.llm.first_response_timeout_secs = 0,
            "idle_timeout_secs" => cfg.llm.idle_timeout_secs = 0,
            _ => cfg.llm.total_timeout_secs = 0,
        }
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains(name), "{err}");
    }
}

#[test]
fn empty_min_rerank_score_env_means_disabled() {
    // compose は未設定を空文字で渡す（`${SHIKI__RAG__MIN_RERANK_SCORE:-}`）。空なら足切りしない。
    let parsed: rag::RagConfig = figment::Figment::from(figment::providers::Serialized::defaults(
        rag::RagConfig::default(),
    ))
    .merge(("min_rerank_score", ""))
    .extract()
    .unwrap();
    assert_eq!(parsed.min_rerank_score, None);
    let parsed: rag::RagConfig = figment::Figment::from(figment::providers::Serialized::defaults(
        rag::RagConfig::default(),
    ))
    .merge(("min_rerank_score", "0.45"))
    .extract()
    .unwrap();
    assert_eq!(parsed.min_rerank_score, Some(0.45));
}

#[test]
fn rag_query_hash_key_is_read_from_env_and_empty_means_unset() {
    // 検索語ハッシュの鍵（#833）は `SHIKI__RAG__QUERY_HASH_KEY` から読む。compose は未設定の env を
    // 空文字で渡すので、空は未設定（起動時乱数）として扱う。数字だけの鍵も文字列として受ける。
    use figment::providers::{Env, Serialized};
    let load = |prefix: &str| -> Result<AppConfig, Box<figment::Error>> {
        Figment::new()
            .merge(Serialized::defaults(valid_config()))
            .merge(Env::prefixed(prefix).split("__"))
            .extract()
            .map_err(Box::new)
    };
    std::env::set_var("SHIKITEST833A__RAG__QUERY_HASH_KEY", "k".repeat(32));
    std::env::set_var("SHIKITEST833B__RAG__QUERY_HASH_KEY", "");
    std::env::set_var("SHIKITEST833C__RAG__QUERY_HASH_KEY", "short");
    std::env::set_var("SHIKITEST833D__RAG__QUERY_HASH_KEY", "1".repeat(40));
    assert!(load("SHIKITEST833A__")
        .unwrap()
        .rag
        .query_hash_key
        .is_some());
    assert!(load("SHIKITEST833B__")
        .unwrap()
        .rag
        .query_hash_key
        .is_none());
    assert!(load("SHIKITEST833C__").is_err(), "短すぎる鍵は起動時に弾く");
    assert!(load("SHIKITEST833D__")
        .unwrap()
        .rag
        .query_hash_key
        .is_some());
}
