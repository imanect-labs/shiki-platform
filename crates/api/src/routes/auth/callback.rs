//! `GET /auth/callback` — code を受け、サーバ側で token 交換しセッションを作る。

use std::time::Duration;

use axum::{
    extract::{Query, State},
    response::Redirect,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use authz::{AuthContext, Principal, Relation};

use super::{build_cookie, parse_flow, removal_cookie, FLOW_COOKIE};
use crate::{
    error::ApiError,
    extract::{resolve_org, resolve_tenant_id},
    middleware::{auth::verify_access_token, claims},
    oidc,
    session::{
        encode_session_cookie, new_opaque_token, SessionRecord, CSRF_COOKIE, SESSION_COOKIE,
    },
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// OIDC callback。state 検証 → token 交換 → access token 検証 → セッション発行。
pub async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<(CookieJar, Redirect), ApiError> {
    if let Some(err) = query.error {
        tracing::warn!(%err, "IdP がエラーを返した");
        return Err(ApiError::Unauthorized);
    }
    let code = query.code.ok_or(ApiError::Unauthorized)?;
    let returned_state = query.state.ok_or(ApiError::Unauthorized)?;

    // 相関 Cookie（state + PKCE verifier）を検証。state 不一致は CSRF/リプレイとして拒否。
    let flow = jar
        .get(FLOW_COOKIE)
        .and_then(|c| parse_flow(c.value()))
        .ok_or(ApiError::Unauthorized)?;
    if flow.state != returned_state {
        tracing::warn!("OIDC state 不一致（CSRF/リプレイの疑い）");
        return Err(ApiError::Unauthorized);
    }

    // code↔token 交換はサーバ側で実施（ブラウザにトークンを出さない）。
    let tokens =
        oidc::exchange_code(&state.http, &state.config.auth, &code, &flow.verifier).await?;

    // 受領した access token を JWKS で検証してクレームを得る。
    let verified = verify_access_token(&state, &tokens.access_token).await?;
    let principal = claims::principal_from_claims(verified);
    let tenant_id = resolve_tenant_id(&principal, &state.config.auth)?;

    // role provisioning（#76・claim 同期）: IdP の roles/groups（AD 部署を含む）を
    // OpenFGA の role メンバーシップタプルへ同期する。**login パスから切り離した detached タスク**で
    // 実行する: (1) login レイテンシに provisioning I/O を載せない (2) provisioning の失敗/遅延が
    // login を壊さない（best-effort・fail-safe。欠落は access を減らす方向）。メンバーシップは
    // 次リクエストまでに反映される想定（eventual。厳密な同期完了は要件ではない）。
    tokio::spawn(provision_roles(
        state.clone(),
        principal.clone(),
        tenant_id.clone(),
    ));

    // セッション本体を作成（トークンはサーバ側のみに保持）。
    let session_id = new_opaque_token();
    let csrf_token = new_opaque_token();
    let now = chrono::Utc::now().timestamp();
    let record = SessionRecord {
        principal,
        tenant_id: tenant_id.clone(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        id_token: tokens.id_token,
        access_expires_at: now + tokens.expires_in,
        csrf_token: csrf_token.clone(),
    };
    let ttl_secs = state.config.session.ttl_secs;
    state
        .sessions
        .put(
            &tenant_id,
            &session_id,
            &record,
            Duration::from_secs(ttl_secs),
        )
        .await?;

    // Cookie: セッション(httpOnly) + CSRF(JS 読取可) を発行し、相関 Cookie を破棄。
    let secure = state.config.session.secure;
    let max_age = ttl_secs as i64;
    let jar = jar
        .add(build_cookie(
            SESSION_COOKIE,
            // 後続リクエストが Cookie だけからテナントスコープを解決できるよう束ねる
            // （multi テナントの session 引きに必須。single でも一貫して付与する）。
            encode_session_cookie(&session_id, &tenant_id),
            true,
            secure,
            max_age,
        ))
        .add(build_cookie(
            CSRF_COOKIE,
            csrf_token,
            false,
            secure,
            max_age,
        ))
        .add(removal_cookie(FLOW_COOKIE, secure));

    Ok((jar, Redirect::to("/")))
}

/// IdP claim（roles ＋ groups）由来のロール/部署メンバーシップを OpenFGA へ冪等同期する（#76）。
///
/// - **正本**: role メンバーの実効判定は OpenFGA の `role:<tenant>|<id>#member@user:<tenant>|<sub>`
///   タプル（`AuthContext::ns()` で名前空間化）。本関数がその claim→タプルの同期経路。
/// - **groups = 部署**: AD/Entra を Keycloak にフェデレートすると OU/部署が `groups` claim に載る
///   （例: `/acme/eng`）。先頭 `/` を除いたパスを role id として同期する（org 単位の group も含む）。
/// - **表示用**: 共有ダイアログのオートコンプリート用に `directory_role` も upsert する。
/// - **best-effort**: 失敗しても login を止めない（欠落は access を減らす方向＝fail-safe）。
/// - ⚠️ **加算同期のみ**: 部署/ロールから外れた際のタプル失効（reconciliation）は未実装。
///   GA 前の必須フォロー（本番の SCIM/group フル同期は SK.6）。それまではセッション TTL と
///   再ログインで徐々に追従する前提。
async fn provision_roles(state: AppState, principal: Principal, tenant_id: String) {
    let org = resolve_org(&principal);
    let ctx = AuthContext::new(principal, org.clone(), tenant_id.clone());
    let subject = ctx.subject();

    // roles claim（フラットなロール名）＋ groups claim（先頭 `/` を除いた部署パス）を role id として扱う。
    let role_ids = ctx.principal.roles.iter().cloned().chain(
        ctx.principal
            .groups
            .iter()
            .map(|g| g.trim_start_matches('/').to_string()),
    );
    for role_id in role_ids {
        let role_id = role_id.trim();
        // 名前空間/型区切りを含む claim は同期対象から除外（識別子を壊さない・共有検証と同ルール）。
        if role_id.is_empty()
            || role_id.contains(':')
            || role_id.contains('#')
            || role_id.contains(authz::TENANT_SEP)
        {
            continue;
        }
        // メンバーシップタプル: object=role:<tenant>|<id>, relation=member, subject=user:<tenant>|<sub>。
        if let Err(e) = state
            .authz
            .write_tuple(&subject, Relation::Member, &ctx.ns().role(role_id))
            .await
        {
            tracing::warn!(role = %role_id, error = %e, "role メンバーシップ同期に失敗（best-effort・login は継続）");
            continue;
        }
        // 共有ダイアログ表示用の射影（display_name は当面 role id そのもの）。
        if let Err(e) = state
            .directory
            .upsert_role(role_id, &tenant_id, &org, role_id)
            .await
        {
            tracing::warn!(role = %role_id, error = %e, "directory_role の upsert に失敗（best-effort）");
        }
    }
}
