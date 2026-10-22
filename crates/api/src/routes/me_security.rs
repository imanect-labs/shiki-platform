//! `/me/security`・`/me/sessions` — ユーザー自身のログイン方法と端末（SSO セッション）。
//!
//! 正本は Keycloak。本人の access token で Account REST API を読む／セッションを切るだけで、
//! shiki 側にはパスキーや OTP の状態を持たない（`crate::keycloak_account`）。
//! 資格情報の追加・削除は `GET /auth/action`（Keycloak の画面で再認証・確認を経る）で行う。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, SessionAccessToken},
    keycloak_account::{KcCredentialContainer, KcDevice, KeycloakAccount, KeycloakAccountError},
    state::AppState,
};

/// Keycloak の資格情報種別（Account API の `type`）。
const KC_PASSWORD: &str = "password";
const KC_OTP: &str = "otp";
const KC_PASSKEY: &str = "webauthn-passwordless";

/// 登録済みのログイン方法 1 件（秘密値は含まない）。
#[derive(Debug, Serialize, ToSchema)]
pub struct CredentialInfo {
    pub id: String,
    /// ユーザーが付けた名前（パスキーの登録名・認証アプリのデバイス名）。
    pub label: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

/// パスワードの状態。
#[derive(Debug, Serialize, ToSchema)]
pub struct PasswordInfo {
    /// パスワードが設定済みか（パスキーだけのユーザーは false）。
    pub configured: bool,
    /// 最後に設定した日時。
    pub updated_at: Option<DateTime<Utc>>,
}

/// ログイン方法の一覧（設定 → セキュリティ）。
#[derive(Debug, Serialize, ToSchema)]
pub struct SecurityOverview {
    pub password: PasswordInfo,
    pub passkeys: Vec<CredentialInfo>,
    pub otp: Vec<CredentialInfo>,
    /// realm でパスキー登録が有効か（無効なら追加ボタンを出さない）。
    pub passkey_available: bool,
    /// realm で認証アプリの設定が有効か。
    pub otp_available: bool,
}

/// ログイン中のセッション 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct SessionInfo {
    pub id: String,
    pub ip_address: Option<String>,
    pub browser: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_access_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// このブラウザのセッションか（自分自身はここから切らない）。
    pub current: bool,
    /// このセッションでログインしているアプリ（表示名）。
    pub apps: Vec<String>,
}

/// 端末（OS・機種）ごとのセッション。
#[derive(Debug, Serialize, ToSchema)]
pub struct DeviceInfo {
    pub os: Option<String>,
    pub os_version: Option<String>,
    pub device: Option<String>,
    pub mobile: bool,
    pub current: bool,
    pub last_access_at: Option<DateTime<Utc>>,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionsResponse {
    pub devices: Vec<DeviceInfo>,
}

/// ユーザー自身のログイン方法（パスワード・パスキー・認証アプリ）。
#[utoipa::path(
    get,
    path = "/me/security",
    responses(
        (status = 200, description = "ログイン方法", body = SecurityOverview),
        (status = 401, description = "未認証"),
        (status = 503, description = "Keycloak Account API が使えない"),
    ),
    security(("session" = [])),
)]
pub async fn get_security(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
    SessionAccessToken(token): SessionAccessToken,
) -> Result<Json<SecurityOverview>, ApiError> {
    let containers = KeycloakAccount::new(&state.http, &state.config.auth, &token)
        .credentials()
        .await
        .map_err(account_error)?;
    Ok(Json(security_overview(&containers)))
}

/// ログイン中の端末とセッション。
#[utoipa::path(
    get,
    path = "/me/sessions",
    responses(
        (status = 200, description = "端末ごとのセッション", body = SessionsResponse),
        (status = 401, description = "未認証"),
        (status = 503, description = "Keycloak Account API が使えない"),
    ),
    security(("session" = [])),
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
    SessionAccessToken(token): SessionAccessToken,
) -> Result<Json<SessionsResponse>, ApiError> {
    let devices = KeycloakAccount::new(&state.http, &state.config.auth, &token)
        .devices()
        .await
        .map_err(account_error)?;
    Ok(Json(SessionsResponse {
        devices: devices.into_iter().map(device_info).collect(),
    }))
}

/// このブラウザ以外の全セッションからログアウトする。
#[utoipa::path(
    delete,
    path = "/me/sessions",
    responses(
        (status = 204, description = "ログアウトさせた"),
        (status = 401, description = "未認証"),
        (status = 503, description = "Keycloak Account API が使えない"),
    ),
    security(("session" = [])),
)]
pub async fn sign_out_other_sessions(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    SessionAccessToken(token): SessionAccessToken,
) -> Result<StatusCode, ApiError> {
    KeycloakAccount::new(&state.http, &state.config.auth, &token)
        .delete_other_sessions()
        .await
        .map_err(account_error)?;
    tracing::info!(user = %ctx.principal.id, "他の全セッションをログアウト");
    Ok(StatusCode::NO_CONTENT)
}

/// 指定したセッションからログアウトする（Keycloak が該当 BFF セッションへ backchannel logout を送る）。
#[utoipa::path(
    delete,
    path = "/me/sessions/{id}",
    params(("id" = String, Path, description = "セッション ID（`/me/sessions` の値）")),
    responses(
        (status = 204, description = "ログアウトさせた"),
        (status = 400, description = "ID が不正"),
        (status = 401, description = "未認証"),
        (status = 404, description = "自分のセッションに無い"),
        (status = 503, description = "Keycloak Account API が使えない"),
    ),
    security(("session" = [])),
)]
pub async fn sign_out_session(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    SessionAccessToken(token): SessionAccessToken,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    // Keycloak のセッション ID は UUID。パスへ埋めるので UUID 以外は受け付けない。
    let id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::BadRequest("セッション ID が不正です".into()))?
        .to_string();
    KeycloakAccount::new(&state.http, &state.config.auth, &token)
        .delete_session(&id)
        .await
        .map_err(account_error)?;
    tracing::info!(user = %ctx.principal.id, session = %id, "セッションをログアウト");
    Ok(StatusCode::NO_CONTENT)
}

/// Account API の失敗を API エラーへ写す。本文はログにだけ残す。
fn account_error(e: KeycloakAccountError) -> ApiError {
    match e {
        KeycloakAccountError::Status { status: 404, .. } => ApiError::NotFound,
        // 401/403 は realm 設定（account の manage-account ロール・audience）の不足が典型。
        other => ApiError::ServiceUnavailable(other.to_string()),
    }
}

fn credentials_of<'a>(
    containers: &'a [KcCredentialContainer],
    kind: &str,
) -> Option<&'a KcCredentialContainer> {
    containers.iter().find(|c| c.kind == kind)
}

fn credential_infos(container: Option<&KcCredentialContainer>) -> Vec<CredentialInfo> {
    container
        .map(|c| {
            c.metadatas
                .iter()
                .map(|m| CredentialInfo {
                    id: m.credential.id.clone(),
                    label: m.credential.user_label.clone().filter(|l| !l.is_empty()),
                    created_at: m
                        .credential
                        .created_date
                        .and_then(DateTime::from_timestamp_millis),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Account API の資格情報一覧を画面向けに要約する。
/// Account API は realm で使える種類だけを返すので、種類の有無＝その方法が使えるか。
fn security_overview(containers: &[KcCredentialContainer]) -> SecurityOverview {
    let password = credential_infos(credentials_of(containers, KC_PASSWORD));
    SecurityOverview {
        password: PasswordInfo {
            configured: !password.is_empty(),
            updated_at: password.iter().filter_map(|p| p.created_at).max(),
        },
        passkeys: credential_infos(credentials_of(containers, KC_PASSKEY)),
        otp: credential_infos(credentials_of(containers, KC_OTP)),
        passkey_available: credentials_of(containers, KC_PASSKEY).is_some(),
        otp_available: credentials_of(containers, KC_OTP).is_some(),
    }
}

fn from_secs(secs: i64) -> Option<DateTime<Utc>> {
    (secs > 0)
        .then(|| DateTime::from_timestamp(secs, 0))
        .flatten()
}

fn device_info(d: KcDevice) -> DeviceInfo {
    DeviceInfo {
        os: d.os.filter(|s| !s.is_empty() && s != "Other"),
        os_version: d.os_version.filter(|s| !s.is_empty() && s != "Unknown"),
        device: d.device.filter(|s| !s.is_empty() && s != "Other"),
        mobile: d.mobile,
        current: d.current,
        last_access_at: from_secs(d.last_access),
        sessions: d
            .sessions
            .into_iter()
            .map(|s| SessionInfo {
                id: s.id,
                ip_address: s.ip_address,
                browser: s.browser.filter(|b| !b.is_empty()),
                started_at: from_secs(s.started),
                last_access_at: from_secs(s.last_access),
                expires_at: from_secs(s.expires),
                current: s.current,
                apps: s
                    .clients
                    .into_iter()
                    .map(|c| {
                        c.client_name
                            .filter(|n| !n.is_empty())
                            .unwrap_or(c.client_id)
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn containers(v: serde_json::Value) -> Vec<KcCredentialContainer> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn overview_maps_password_passkeys_and_otp() {
        let c = containers(json!([
            { "type": "password", "removeable": false, "userCredentialMetadatas": [
                { "credential": { "id": "p1", "type": "password", "createdDate": 1_700_000_000_000_i64 } }
            ]},
            { "type": "otp", "removeable": true, "userCredentialMetadatas": [] },
            { "type": "webauthn-passwordless", "removeable": true, "userCredentialMetadatas": [
                { "credential": { "id": "k1", "userLabel": "MacBook", "createdDate": 1_710_000_000_000_i64 } },
                { "credential": { "id": "k2", "userLabel": "" } }
            ]}
        ]));
        let o = security_overview(&c);
        assert!(o.password.configured);
        assert_eq!(
            o.password.updated_at.unwrap().timestamp_millis(),
            1_700_000_000_000
        );
        assert!(o.passkey_available && o.otp_available);
        assert!(o.otp.is_empty());
        assert_eq!(o.passkeys.len(), 2);
        assert_eq!(o.passkeys[0].label.as_deref(), Some("MacBook"));
        assert_eq!(o.passkeys[1].label, None, "空の名前は None に倒す");
        assert_eq!(o.passkeys[1].created_at, None);
    }

    #[test]
    fn overview_without_passkey_type_marks_unavailable() {
        let o = security_overview(&containers(json!([
            { "type": "password", "userCredentialMetadatas": [] }
        ])));
        assert!(!o.password.configured);
        assert!(!o.passkey_available);
        assert!(!o.otp_available);
    }

    #[test]
    fn device_info_drops_unknown_placeholders() {
        let d: KcDevice = serde_json::from_value(json!({
            "os": "Other", "osVersion": "Unknown", "device": "Other",
            "lastAccess": 1_792_468_923, "current": true, "mobile": false,
            "sessions": [{
                "id": "s1", "ipAddress": "10.0.0.1", "started": 1_792_468_000,
                "lastAccess": 1_792_468_923, "expires": 0, "browser": "Chrome/141",
                "current": true,
                "clients": [{ "clientId": "shiki-web", "clientName": "" }, { "clientId": "x", "clientName": "X App" }]
            }]
        }))
        .unwrap();
        let info = device_info(d);
        assert_eq!(info.os, None);
        assert_eq!(info.os_version, None);
        assert_eq!(info.device, None);
        assert!(info.current);
        let s = &info.sessions[0];
        assert_eq!(s.expires_at, None, "0 は未設定扱い");
        assert_eq!(s.apps, vec!["shiki-web".to_string(), "X App".to_string()]);
        assert_eq!(s.browser.as_deref(), Some("Chrome/141"));
    }

    #[test]
    fn account_errors_map_to_api_errors() {
        assert!(matches!(
            account_error(KeycloakAccountError::Status {
                status: 404,
                body: String::new()
            }),
            ApiError::NotFound
        ));
        assert!(matches!(
            account_error(KeycloakAccountError::Status {
                status: 401,
                body: String::new()
            }),
            ApiError::ServiceUnavailable(_)
        ));
        assert!(matches!(
            account_error(KeycloakAccountError::Transport("x".into())),
            ApiError::ServiceUnavailable(_)
        ));
    }
}
