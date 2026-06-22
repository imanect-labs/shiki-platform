//! `GET /auth/login` — state/PKCE を生成し Keycloak の authorize へリダイレクトする。

use axum::{extract::State, response::Redirect};
use axum_extra::extract::cookie::CookieJar;

use super::{flow_cookie, FlowState};
use crate::{error::ApiError, oidc::pkce_challenge, session::new_opaque_token, state::AppState};

/// ログイン開始。相関 Cookie（state + PKCE verifier）を発行し authorize へ 302。
pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), ApiError> {
    let auth = &state.config.auth;

    let oidc_state = new_opaque_token();
    let verifier = new_opaque_token();
    let challenge = pkce_challenge(&verifier);

    let url = reqwest::Url::parse_with_params(
        &auth.authorize_endpoint(),
        &[
            ("response_type", "code"),
            ("client_id", auth.client_id.as_str()),
            ("redirect_uri", auth.redirect_uri.as_str()),
            ("scope", auth.scopes.as_str()),
            ("state", oidc_state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ],
    )
    .map_err(|e| ApiError::Internal(format!("authorize URL 構築に失敗: {e}")))?;

    let flow = FlowState {
        state: oidc_state,
        verifier,
    };
    let jar = jar.add(flow_cookie(&flow, state.config.session.secure));
    Ok((jar, Redirect::to(url.as_str())))
}
