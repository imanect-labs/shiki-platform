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

    // 撤去中/撤去済みテナントは新規ログインを拒否する（SAAS.2。セッション失効と IdP ユーザー
    // 削除の間に完了するログイン競合を塞ぐ）。レジストリ未登録（dev fixture 等）は許可。
    // レジストリが読めない場合は**警告して継続**（fail-open）: これは purge 中の狭い競合を塞ぐ
    // 二次ベルトであり、purge 後はデータ面（FGA/DB/オブジェクト）が全て deny するため、
    // インフラ断でログイン全体を巻き添えにしない方を選ぶ。
    match state.tenants.get(&tenant_id).await {
        Ok(Some(t)) if t.status != storage::TenantStatus::Active => {
            tracing::warn!(%tenant_id, status = t.status.as_str(), "撤去中/済みテナントのログインを拒否");
            return Err(ApiError::Unauthorized);
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(%tenant_id, error = %e, "tenant レジストリ照会に失敗（fail-open で継続）");
        }
    }

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

/// IdP claim（roles ＋ groups）由来のロール/部署メンバーシップを OpenFGA へ **diff 同期**する
/// （#76 / reconciliation #89）。
///
/// - **正本 = IdP claims**: role メンバーの実効判定は OpenFGA の
///   `role:<tenant>|<id>#member@user:<tenant>|<sub>` タプルだが、その内容は claims が正。
///   ログイン毎に「あるべき集合（claims）」と「現在の直接タプル」を突合し、
///   **不足を付与・余剰を剥奪**する（部署異動/ロール剥奪が次ログインで反映される）。
/// - **groups = 部署**: AD/Entra フェデレートで OU/部署が `groups` claim に載る（例 `/acme/eng`）。
///   先頭 `/` を除いたパスを role id にする。**org そのもののグループ（`/{org}`）は除外**
///   （org は organization タプルの責務で、role 化するとノイズになる）。
/// - **テナント内に閉じる**: 現在集合は自テナント名前空間の直接タプルのみ抽出。他テナントの
///   タプルには読み書きとも触れない。
/// - **表示用**: 共有ダイアログ用に `directory_role` も upsert する。
/// - **best-effort**: 失敗しても login を止めない。付与失敗は access が減る方向（fail-safe）、
///   剥奪失敗は次回ログインで再収束。
async fn provision_roles(state: AppState, principal: Principal, tenant_id: String) {
    let org = resolve_org(&principal);
    let ctx = AuthContext::new(principal, org.clone(), tenant_id.clone());
    let subject = ctx.subject();
    let ns = ctx.ns();

    // あるべき集合（claims 由来・正規化済み）。
    let desired = desired_role_ids(&ctx.principal.roles, &ctx.principal.groups, &org);

    // 現在の直接 role タプル（自テナント名前空間のみ・継承展開なし）。
    let current: Vec<String> = match state
        .authz
        .read_subject_objects(&subject, authz::ObjectType::Role)
        .await
    {
        Ok(objects) => objects
            .iter()
            .filter_map(|raw| raw.strip_prefix("role:"))
            .filter_map(|id_part| ns.strip_object_id(id_part))
            .map(str::to_string)
            .collect(),
        Err(e) => {
            // 現状が読めない時は**剥奪をスキップ**し付与のみ行う（誤剥奪を避ける fail-safe）。
            tracing::warn!(error = %e, "role 現在集合の取得に失敗（付与のみ実施）");
            Vec::new()
        }
    };

    // 剥奪: 現在 − あるべき（自テナント分のみ）。
    for stale in current.iter().filter(|c| !desired.contains(*c)) {
        match state
            .authz
            .delete_tuple(&subject, Relation::Member, &ns.role(stale))
            .await
        {
            Ok(true) => {
                tracing::info!(role = %stale, "role メンバーシップを剥奪（claims から消失）")
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(role = %stale, error = %e, "role 剥奪に失敗（次回ログインで再収束）")
            }
        }
    }

    // 付与: あるべき − 現在（冪等 write なので差分に限らず流しても安全だが、無駄打ちを避ける）。
    for role_id in desired.iter().filter(|d| !current.contains(*d)) {
        if let Err(e) = state
            .authz
            .write_tuple(&subject, Relation::Member, &ns.role(role_id))
            .await
        {
            tracing::warn!(role = %role_id, error = %e, "role 付与に失敗（best-effort・login は継続）");
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

/// claims（roles ＋ groups）から「あるべき role id 集合」を作る（純粋関数・テスト対象）。
///
/// - groups は先頭 `/` を除いた部署パス。**org そのもののグループ（`/{org}`）だけを除外**する
///   （org は organization タプルの責務）。roles claim に org と同名のロールがあっても
///   それは正当なロールなので除外しない（除外すると diff 同期が誤剥奪する）。
/// - 空・空白・FGA 構造文字（`: # |`）を含む値は除外（識別子を壊さない）。
fn desired_role_ids(roles: &[String], groups: &[String], org: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let candidates = roles.iter().map(|r| (r.trim().to_string(), false)).chain(
        groups
            .iter()
            .map(|g| (g.trim_start_matches('/').to_string(), true)),
    );
    for (id, from_group) in candidates {
        let id = id.trim();
        if id.is_empty()
            || (from_group && id == org)
            || id.contains(':')
            || id.contains('#')
            || id.contains(authz::TENANT_SEP)
        {
            continue;
        }
        if !out.iter().any(|existing| existing == id) {
            out.push(id.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::desired_role_ids;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn desired_roles_merge_roles_and_groups() {
        // roles と groups（先頭 / 除去）を統合し、重複を除く。
        let got = desired_role_ids(
            &v(&["engineering", "sales"]),
            &v(&["/acme/eng", "/acme"]),
            "acme",
        );
        assert_eq!(got, v(&["engineering", "sales", "acme/eng"]));
    }

    #[test]
    fn desired_roles_exclude_org_group() {
        // org そのもののグループは role 化しない。
        let got = desired_role_ids(&[], &v(&["/acme"]), "acme");
        assert!(got.is_empty());
    }

    #[test]
    fn desired_roles_keep_role_named_like_org() {
        // roles claim に org と同名のロールがあっても正当なロールとして残す
        // （groups 由来の org のみ除外。誤剥奪防止・Codex #90 指摘）。
        let got = desired_role_ids(&v(&["acme"]), &v(&["/acme"]), "acme");
        assert_eq!(got, v(&["acme"]));
    }

    #[test]
    fn desired_roles_reject_forbidden_chars() {
        // FGA 構造文字を含む claim は同期しない（識別子注入防御）。
        let got = desired_role_ids(&v(&["a:b", "c#d", "e|f", " ", "ok"]), &[], "org");
        assert_eq!(got, v(&["ok"]));
    }
}
