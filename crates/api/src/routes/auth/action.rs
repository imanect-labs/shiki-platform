//! `GET /auth/action` — アカウントのセキュリティ操作を Keycloak の画面で行わせる。
//!
//! パスキー登録・認証アプリ（OTP）設定・パスワード変更・資格情報の削除は、Keycloak の
//! Application Initiated Action（`kc_action`）として authorize 経由で開始する。Keycloak が
//! 必要に応じて再認証（max_age）と確認画面を挟むため、セッション Cookie を盗まれただけでは
//! MFA を外されない。完了（またはキャンセル）後は callback が新しいセッションを発行し、
//! `/settings/security?action=…&status=…` へ戻す。

use axum::{
    extract::{Query, State},
    response::Redirect,
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::login::authorize_redirect;
use crate::{error::ApiError, extract::AuthContextExt, state::AppState};

/// 設定画面から始めるアカウント操作（閉じた語彙）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountActionKind {
    /// パスキー（discoverable credential）を登録する。
    Passkey,
    /// 認証アプリ（TOTP）を設定する。
    Otp,
    /// パスワードを変更する。
    Password,
    /// 登録済みのパスキー／認証アプリを削除する（`credential_id` 必須）。
    DeleteCredential,
}

/// 相関 Cookie に控えるアカウント操作（callback が戻り先の判断に使う）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAction {
    pub kind: AccountActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    /// 操作を始めたユーザー（principal id）。callback で認証後のユーザーと照合し、
    /// Keycloak の SSO セッションが別ユーザーのものだった場合にセッションを差し替えない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl AccountAction {
    /// Keycloak の `kc_action` 値。
    pub(super) fn kc_action(&self) -> String {
        match self.kind {
            AccountActionKind::Passkey => "webauthn-register-passwordless".into(),
            AccountActionKind::Otp => "CONFIGURE_TOTP".into(),
            AccountActionKind::Password => "UPDATE_PASSWORD".into(),
            AccountActionKind::DeleteCredential => format!(
                "delete_credential:{}",
                self.credential_id.as_deref().unwrap_or_default()
            ),
        }
    }

    /// 完了後に戻す設定画面の URL（同一オリジンの固定パス。クエリは閉じた語彙のみ）。
    pub(super) fn return_url(&self, status: &str) -> String {
        let action = match self.kind {
            AccountActionKind::Passkey => "passkey",
            AccountActionKind::Otp => "otp",
            AccountActionKind::Password => "password",
            AccountActionKind::DeleteCredential => "delete_credential",
        };
        format!("/settings/security?action={action}&status={status}")
    }
}

/// Keycloak が返す `kc_action_status` を閉じた語彙に丸める（未知値は error 扱い）。
pub(super) fn normalize_action_status(raw: Option<&str>) -> &'static str {
    match raw {
        Some("success") => "success",
        Some("cancelled") => "cancelled",
        _ => "error",
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AccountActionQuery {
    /// 行う操作。
    pub action: AccountActionKind,
    /// 削除対象の資格情報 ID（`delete_credential` のときのみ）。
    pub credential_id: Option<String>,
}

/// Keycloak の資格情報 ID は UUID。`kc_action` に埋め込むので UUID 以外は受け付けない。
fn validate_credential_id(id: Option<&str>) -> Result<String, ApiError> {
    let id = id.ok_or_else(|| ApiError::BadRequest("credential_id が必要です".into()))?;
    uuid::Uuid::parse_str(id)
        .map(|u| u.to_string())
        .map_err(|_| ApiError::BadRequest("credential_id が不正です".into()))
}

/// アカウント操作を開始する（Keycloak の画面へ 302）。ログイン済みのときだけ使える。
#[utoipa::path(
    get,
    path = "/auth/action",
    params(AccountActionQuery),
    responses(
        (status = 303, description = "Keycloak の操作画面へリダイレクト"),
        (status = 400, description = "操作または credential_id が不正"),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn start_account_action(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    jar: CookieJar,
    Query(query): Query<AccountActionQuery>,
) -> Result<(CookieJar, Redirect), ApiError> {
    let credential_id = match query.action {
        AccountActionKind::DeleteCredential => {
            Some(validate_credential_id(query.credential_id.as_deref())?)
        }
        _ => None,
    };
    tracing::info!(user = %ctx.principal.id, action = ?query.action, "アカウント操作を開始");
    authorize_redirect(
        &state,
        jar,
        Some(AccountAction {
            kind: query.action,
            credential_id,
            subject: Some(ctx.principal.id.clone()),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(kind: AccountActionKind, id: Option<&str>) -> AccountAction {
        AccountAction {
            kind,
            credential_id: id.map(str::to_string),
            subject: None,
        }
    }

    #[test]
    fn kc_action_values() {
        assert_eq!(
            action(AccountActionKind::Passkey, None).kc_action(),
            "webauthn-register-passwordless"
        );
        assert_eq!(
            action(AccountActionKind::Otp, None).kc_action(),
            "CONFIGURE_TOTP"
        );
        assert_eq!(
            action(AccountActionKind::Password, None).kc_action(),
            "UPDATE_PASSWORD"
        );
        assert_eq!(
            action(
                AccountActionKind::DeleteCredential,
                Some("9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d")
            )
            .kc_action(),
            "delete_credential:9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d"
        );
    }

    #[test]
    fn return_url_is_fixed_settings_path() {
        assert_eq!(
            action(AccountActionKind::Otp, None).return_url("success"),
            "/settings/security?action=otp&status=success"
        );
        assert_eq!(
            action(AccountActionKind::DeleteCredential, None).return_url("cancelled"),
            "/settings/security?action=delete_credential&status=cancelled"
        );
    }

    #[test]
    fn action_status_is_closed_vocabulary() {
        assert_eq!(normalize_action_status(Some("success")), "success");
        assert_eq!(normalize_action_status(Some("cancelled")), "cancelled");
        assert_eq!(normalize_action_status(Some("error")), "error");
        assert_eq!(normalize_action_status(Some("<script>")), "error");
        assert_eq!(normalize_action_status(None), "error");
    }

    #[test]
    fn credential_id_must_be_uuid() {
        assert!(validate_credential_id(None).is_err());
        assert!(validate_credential_id(Some("abc")).is_err());
        assert!(validate_credential_id(Some("../../x")).is_err());
        assert_eq!(
            validate_credential_id(Some("9B1DEB4D-3B7D-4BAD-9BDD-2B0D7B3DCB6D")).unwrap(),
            "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d"
        );
    }
}
