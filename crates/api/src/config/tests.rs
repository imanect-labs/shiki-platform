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
