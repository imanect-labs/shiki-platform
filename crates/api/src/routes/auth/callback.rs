//! `GET /auth/callback` — code を受け、サーバ側で token 交換しセッションを作る。

use std::time::Duration;

use axum::{
    extract::{Query, State},
    response::Redirect,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use authz::{AuthContext, Principal};

use super::{
    action::{normalize_action_status, AccountAction},
    build_cookie,
    membership_audit::{MembershipAudit, MembershipSource},
    org_member, parse_flow, removal_cookie, role_member, FLOW_COOKIE,
};

/// ログインの往復で、組織の member の同期を待つ長さ（[`org_member`]・過ぎたら裏で続ける）。
const ORG_MEMBER_WAIT: Duration = Duration::from_secs(3);
/// sub が利用者の id に使えない人を戻すログイン画面（D-205・`web/src/app/login/page.tsx` が文言を出す）。
const ACCOUNT_UNUSABLE_URL: &str = "/login?error=account_unusable";
/// sub が使えない人の Keycloak の SSO セッションを終えるのを待つ長さ（過ぎたら終えずに戻す）。
const END_SSO_WAIT: Duration = Duration::from_secs(3);
use crate::{
    config::AuthConfig,
    error::ApiError,
    extract::{group_layout, resolve_tenant_id},
    middleware::{auth::verify_access_token, claims},
    oidc,
    session::{
        decode_session_cookie, encode_session_cookie, new_opaque_token, SessionRecord, CSRF_COOKIE,
        SESSION_COOKIE,
    },
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    /// アカウント操作（`kc_action`）の結果（success / cancelled / error）。
    kc_action_status: Option<String>,
}

/// OIDC callback。state 検証 → token 交換 → access token 検証 → セッション発行。
pub async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<(CookieJar, Redirect), ApiError> {
    if let Some(err) = query.error {
        tracing::warn!(%err, "IdP がエラーを返した");
        // 設定画面から始めたアカウント操作の失敗は、state 一致を確かめたうえで設定画面へ戻す
        // （JSON の 401 を見せない）。それ以外のログイン失敗は従来どおり 401。
        if let Some(back) = action_error_redirect(&jar, query.state.as_deref()) {
            let jar = jar.add(removal_cookie(FLOW_COOKIE, state.config.session.secure));
            return Ok((jar, Redirect::to(&back)));
        }
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
    // Keycloak SSO セッション id（backchannel logout の逆引き鍵・#91）。principal 生成で
    // クレームが move されるため先に取り出す。
    let keycloak_sid = verified.sid.clone();
    // sub が利用者の id に使えない人（D-205）は、タプルもセッションも作らずに戻し、JSON の 401 は見せない。
    // ふつうのログインでは Keycloak の SSO セッションも終える（principal_or_refuse）。
    let refused = principal_or_refuse(
        &state,
        verified,
        flow.action.as_ref(),
        tokens.refresh_token.as_deref(),
    )
    .await;
    let principal = match refused {
        Ok(principal) => principal,
        Err(back) => {
            let jar = jar.add(removal_cookie(FLOW_COOKIE, state.config.session.secure));
            return Ok((jar, Redirect::to(&back)));
        }
    };
    let tenant_id = resolve_tenant_id(&principal, &state.config.auth)?;

    // アカウント操作の往復で、Keycloak 側の SSO セッションが始めたユーザーと別人だった場合
    // （同じブラウザで別アカウントに入り直した等）は、セッションを差し替えずに設定画面へ戻す。
    if let Some(back) = action_subject_mismatch(flow.action.as_ref(), &principal.id) {
        let jar = jar.add(removal_cookie(FLOW_COOKIE, state.config.session.secure));
        return Ok((jar, Redirect::to(&back)));
    }

    refuse_retired_tenant(&state, &tenant_id).await?;

    // 組織の member（D-130・org_member）は、ログインの往復の中でそろえてから戻す。
    sync_org_member_before_redirect(&state, &principal, &tenant_id).await;

    // role provisioning（#76・claim 同期）: IdP の roles/groups（AD 部署を含む）を
    // OpenFGA の role メンバーシップタプルへ同期する。**login パスから切り離した detached タスク**で
    // 実行する: (1) login レイテンシに provisioning I/O を載せない (2) provisioning の失敗/遅延が
    // login を壊さない（best-effort・fail-safe。欠落は access を減らす方向）。メンバーシップは
    // 次リクエストまでに反映される想定（eventual。厳密な同期完了は要件ではない）。
    tokio::spawn(provision_roles(
        state.clone(),
        principal.clone(),
        tenant_id.clone(),
        MembershipSource::Login,
    ));

    // 同じブラウザの旧セッション（アカウント操作の往復・再ログイン）。新セッションの保存に
    // 成功してから捨てる（先に消すと、保存失敗時に旧セッションだけ失ってログアウト状態になる）。
    let old_session = jar
        .get(SESSION_COOKIE)
        .and_then(|c| decode_session_cookie(c.value()))
        .map(|(sid, tenant)| (sid.to_owned(), tenant.to_owned()));

    // セッション本体を作成（トークンはサーバ側のみに保持）。
    let session_id = new_opaque_token();
    let csrf_token = new_opaque_token();
    let now = chrono::Utc::now().timestamp();
    let record = SessionRecord {
        principal,
        tenant_id: tenant_id.to_string(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        id_token: tokens.id_token,
        access_expires_at: now + tokens.expires_in,
        csrf_token: csrf_token.clone(),
        keycloak_sid,
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

    // Cookie は下で上書きされるため、残すとストアに孤児レコードが TTL まで残る。best-effort。
    if let Some((old_sid, old_tenant)) = &old_session {
        if let Err(e) = state.sessions.delete(old_tenant, old_sid).await {
            tracing::warn!(error = %e, "旧セッションの削除に失敗（TTL で消える）");
        }
    }

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

    // 設定画面から始めた操作は結果付きで設定画面へ、通常ログインはトップへ戻す。
    let target = flow.action.map_or_else(
        || "/".to_string(),
        |action| action.return_url(normalize_action_status(query.kc_action_status.as_deref())),
    );
    Ok((jar, Redirect::to(&target)))
}

/// 撤去中/撤去済みテナントの新規ログインを断る（SAAS.2。セッション失効と IdP ユーザー削除の間に完了するログイン
/// 競合を塞ぐ）。レジストリ未登録（dev fixture 等）は許可する。
///
/// レジストリが読めない場合は**警告して継続**（fail-open）: これは purge 中の狭い競合を塞ぐ二次ベルトであり、purge
/// 後はデータ面（FGA/DB/オブジェクト）が全て deny するため、インフラ断でログイン全体を巻き添えにしない方を選ぶ。
async fn refuse_retired_tenant(
    state: &AppState,
    tenant_id: &authz::TenantId,
) -> Result<(), ApiError> {
    match state.tenants.get(tenant_id).await {
        Ok(Some(t)) if t.status != storage::TenantStatus::Active => {
            tracing::warn!(%tenant_id, status = t.status.as_str(), "撤去中/済みテナントのログインを拒否");
            Err(ApiError::Unauthorized)
        }
        Ok(_) => Ok(()),
        Err(e) => {
            // fail-open は意図的（purge 後はデータ面が全 deny する二次ベルト）。ただし
            // 撤去中テナントのログインを一時的に許す窓になるため、この警告は監視対象にし
            // アラートを張ること（#91 L-6・レジストリ DB 障害の早期検知）。
            tracing::warn!(%tenant_id, error = %e, "tenant レジストリ照会に失敗（fail-open で継続・要監視）");
            Ok(())
        }
    }
}

/// 組織の member（[`org_member`]・D-130）を、ログインの往復の中でそろえる。
///
/// 戻った直後の `/me` と、続けて開くアプリのゲートウェイが、どちらもこのタプルで組織のメンバーかを判定するため、
/// リダイレクトの前に待つ。待つのは [`ORG_MEMBER_WAIT`] までで、過ぎたら裏で続けさせてログインを進める（OpenFGA の
/// 遅れでログインを止めない）。付け外しは監査に残す（[`super::membership_audit`]・仕事はログイン）。
async fn sync_org_member_before_redirect(
    state: &AppState,
    principal: &Principal,
    tenant_id: &authz::TenantId,
) {
    let sync = tokio::spawn({
        let authz = state.authz.clone();
        let audit = state.audit.clone();
        let principal = principal.clone();
        let tenant_id = tenant_id.clone();
        let layout = group_layout(&state.config.auth);
        async move {
            org_member::sync_org_member_audited(
                &*authz,
                &*audit,
                MembershipSource::Login,
                &principal,
                &tenant_id,
                layout,
            )
            .await
        }
    });
    if tokio::time::timeout(ORG_MEMBER_WAIT, sync).await.is_err() {
        tracing::warn!("組織の member の同期が時間内に終わりません（裏で続ける）");
    }
}

/// アカウント操作の開始ユーザーと認証後のユーザーが食い違えば、設定画面への error の戻り先を返す。
fn action_subject_mismatch(action: Option<&AccountAction>, principal_id: &str) -> Option<String> {
    let action = action?;
    if action.subject.as_deref().is_some_and(|s| s != principal_id) {
        tracing::warn!("アカウント操作の開始ユーザーと認証後のユーザーが一致しない");
        return Some(action.return_url("error"));
    }
    None
}

/// 検証したクレームから主体を組む（[`claims::principal_from_claims`]）。sub が利用者の id に使えなければ（D-205）、
/// 主体の代わりに戻り先を返す。
///
/// ふつうのログインはログイン画面（[`ACCOUNT_UNUSABLE_URL`]）。ログイン画面は自動で送り直さず、同じアカウントでの
/// 再試行のボタンも出さない（同じ sub で同じ失敗を繰り返すだけなので）。出すのは「別のアカウントでログイン」で、
/// [`principal_or_refuse`] が Keycloak の SSO セッションを終えておく。設定画面から始めたアカウント操作は、始めた人と
/// 別の人が戻ってきたのと同じく、設定画面へ error で戻す（その人のセッションはそのまま残る）。
fn principal_or_unusable(
    verified: claims::Claims,
    action: Option<&AccountAction>,
) -> Result<Principal, String> {
    claims::principal_from_claims(verified).map_err(|e| {
        tracing::warn!(error = %e, "sub が利用者の id に使えない人のログインを断る");
        action.map_or_else(
            || ACCOUNT_UNUSABLE_URL.to_string(),
            |action| action.return_url("error"),
        )
    })
}

/// [`principal_or_unusable`] で主体を組み、sub が使えずに断るときは戻り先を返す。ふつうのログイン（`action` が無い）で
/// 断るときは、受け取った `refresh_token` で Keycloak の SSO セッションも終えてから返す（[`end_unusable_sso_session`]）。
async fn principal_or_refuse(
    state: &AppState,
    verified: claims::Claims,
    action: Option<&AccountAction>,
    refresh_token: Option<&str>,
) -> Result<Principal, String> {
    let principal = principal_or_unusable(verified, action);
    if principal.is_err() && action.is_none() {
        end_unusable_sso_session(state, refresh_token).await;
    }
    principal
}

/// sub が使えずに断った人の Keycloak の SSO セッションを、受け取った refresh token でサーバ側から終える（best-effort）。
///
/// 終えないと、ログイン画面を開き直しても Keycloak が同じ人を黙って通して同じ画面に戻り、共有の端末でほかの人が
/// ログインできない（Import を有効にした後も SSO の Cookie が切れるまで続く）。ブラウザのログアウト
/// （`logout.rs`・end-session へのフル遷移）は `id_token_hint` を出さないので Keycloak が確認の画面を挟み、戻り先も
/// `post_logout_redirect_uri` に限られるので、ここはトークンを持っているサーバ側で終える
/// （[`oidc::end_sso_session`]）。終えられなくてもログイン画面へは戻す（ログイン画面の「別のアカウントでログイン」は、
/// そのときは同じ画面に戻る）。アカウント操作の往復は、始めた人のセッションを残すので終えない。
async fn end_unusable_sso_session(state: &AppState, refresh_token: Option<&str>) {
    let Some(refresh_token) = refresh_token else {
        tracing::warn!("refresh token が無いので、断った人の SSO セッションを終えられません");
        return;
    };
    let ended = tokio::time::timeout(
        END_SSO_WAIT,
        oidc::end_sso_session(&state.http, &state.config.auth, refresh_token),
    )
    .await;
    match ended {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "断った人の Keycloak の SSO セッションを終えられません");
        }
        Err(_) => {
            tracing::warn!("断った人の Keycloak の SSO セッションを終えるのが間に合いません");
        }
    }
}

/// IdP エラー時、相関 Cookie がアカウント操作で state も一致すれば設定画面の戻り先を返す。
fn action_error_redirect(jar: &CookieJar, returned_state: Option<&str>) -> Option<String> {
    let flow = jar.get(FLOW_COOKIE).and_then(|c| parse_flow(c.value()))?;
    let action = flow.action?;
    (returned_state? == flow.state).then(|| action.return_url("error"))
}

/// セッションの更新（アクセストークンの寿命ごと・`middleware/session.rs`）で、組織の member（[`org_member`]・
/// D-130）とロールのメンバーシップ（[`provision_roles`]）を claims にそろえる（detached・best-effort）。
///
/// 組織を移った人・所属グループから外れた人の member は、ここか次のログインで外れる。付け外しは監査に残す
/// （[`super::membership_audit`]・仕事はセッションの更新）。
pub(crate) async fn sync_memberships(
    state: AppState,
    principal: Principal,
    tenant_id: authz::TenantId,
) {
    org_member::sync_org_member_audited(
        &*state.authz,
        &*state.audit,
        MembershipSource::SessionRefresh,
        &principal,
        &tenant_id,
        group_layout(&state.config.auth),
    )
    .await;
    provision_roles(
        state,
        principal,
        tenant_id,
        MembershipSource::SessionRefresh,
    )
    .await;
}

/// IdP claim（roles ＋ groups）由来のロール/部署メンバーシップを OpenFGA へ **diff 同期**する
/// （#76 / reconciliation #89）。
///
/// - **正本 = IdP claims**: role メンバーの実効判定は OpenFGA の
///   `role:<tenant>|<id>#member@user:<tenant>|<sub>` タプルだが、その内容は claims が正。
///   ログイン毎に「あるべき集合（claims）」と「現在の直接タプル」を突合し、
///   **不足を付与・余剰を剥奪**する（部署異動/ロール剥奪が次ログインで反映される）。
///   さらにセッション **refresh 周期でも同期**する（`middleware/session.rs`・#91 H-1）:
///   ログイン時のみだと、refresh で延命される長寿命セッションが剥奪済み role を再ログイン
///   まで保持し続けるため、失効窓を access token 寿命（≒refresh 周期）まで縮める。
/// - **groups = 部署**: AD/Entra フェデレートで OU/部署が `groups` claim に載る
///   （例 `/tenants/acme/acme/eng`）。テナントルートからの相対パス（`acme/eng`）を role id にする
///   （[`authz::org_group::tenant_relative_groups`]・他テナント配下は捨てる・#811）。
///   **org そのもののグループ（`/tenants/<tenant>/{org}`）は除外**
///   （org は organization タプルの責務で、role 化するとノイズになる）。
/// - **テナント内に閉じる**: 現在集合は自テナント名前空間の直接タプルのみ抽出。他テナントの
///   タプルには読み書きとも触れない。
/// - **表示用**: 共有ダイアログ用に `directory_role` も upsert する（タプルが既にあるロールの欠けた行も戻す・BUG-270）。
/// - **best-effort**: 失敗しても login を止めない。付与失敗は access が減る方向（fail-safe）、
///   剥奪失敗は次回ログインで再収束。
/// - **監査**: 付け外しはシステムを主体にした監査に残す（`source` はログインかセッションの更新・
///   [`super::membership_audit`]・GAP-78）。
// groups claim からの role 収束は「抽出→現在集合との差分→付与/剥奪→表示用 upsert」の
// 分岐が本質的に多い。best-effort の各失敗を握り潰す都合で cognitive complexity が高くなるが、
// 収束ロジックを一望できる利点を優先してグランドファーザ許容する。
#[allow(clippy::cognitive_complexity)]
pub(crate) async fn provision_roles(
    state: AppState,
    principal: Principal,
    tenant_id: authz::TenantId,
    source: MembershipSource,
) {
    // あるべき集合（claims 由来・正規化済み）。
    let (org, sourced) = desired_from_claims(&principal, &tenant_id, &state.config.auth);
    let ctx = AuthContext::new(principal, org.clone(), tenant_id.clone());
    let desired: Vec<String> = sourced.iter().map(|(id, _)| id.clone()).collect();

    // ディレクトリへの登録（共有の相手検索・AI 活用分析の在籍者）。人だけ（workflow 主体は除く）。
    // 失敗しても認可には影響しない（次回のログイン・refresh で再試行）。
    if ctx.principal().kind == authz::PrincipalKind::User {
        match state
            .directory
            .ensure_user(
                &ctx.principal().id,
                &tenant_id,
                &org,
                ctx.principal().email.as_deref(),
            )
            .await
        {
            // 新しく載った人の過去の利用も分析に入るよう、その組織の集計を遡ってやり直させる
            // （次の定期実行で 1 回だけ。在籍者だけを数えるので、載る前の日は 0 人で作られている）。
            Ok(true) => state.insights.request_backfill(&tenant_id, &org).await,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "directory_user の登録に失敗（共有の相手検索・分析の在籍者に影響）");
            }
        }
    }

    // AI 活用分析の部署集計用の写し（#751・PIT-18）。失敗しても認可には影響しない（次回同期で収束）。
    if let Err(e) = state
        .directory
        .replace_user_roles(&tenant_id, &org, &ctx.principal().id, &sourced)
        .await
    {
        tracing::warn!(error = %e, "directory_user_role の更新に失敗（分析の部署集計のみに影響）");
    }

    // ロールの直接タプルと台帳をあるべき集合にそろえる（剥奪・付与・台帳の欠けの復元・BUG-270）。付け外しは監査に残す。
    let audit = MembershipAudit::new(&*state.audit, source, ctx.tenant(), &ctx.principal().id);
    let changed =
        role_member::sync_roles(&*state.authz, &state.directory, &audit, &ctx, &desired).await;
    // ロールが変わったら構造化データの SSE 購読へ知らせる（TTL を待たずに材料を引き直す・PIT-70 ③）。
    if changed {
        if let Err(e) = state.data.notify_authz_changed(&ctx, None).await {
            tracing::warn!(error = %e, "SSE へのロール変更の通知に失敗（購読は TTL で引き直す）");
        }
    }
}

/// claims から org と「あるべき role（部署由来か）」を求める（[`provision_roles`] の純粋部分）。
///
/// 決め方は定期の照合（D-136・D-140）と同じ [`role_member::desired_roles`]（単一定義）。org は `AuthContext` と
/// 同じ [`crate::extract::resolve_org`] の規則で決まる。
fn desired_from_claims(
    principal: &Principal,
    tenant_id: &str,
    auth: &AuthConfig,
) -> (String, Vec<(String, bool)>) {
    let desired = role_member::desired_roles(
        &principal.roles,
        &principal.groups,
        tenant_id,
        group_layout(auth),
    );
    (desired.org, desired.roles)
}

/// claims（roles ＋ groups）から「あるべき role id 集合」を作る（[`role_member::role_sources`] の id だけ・テスト用）。
#[cfg(test)]
fn desired_role_ids(roles: &[String], departments: &[String], org: &str) -> Vec<String> {
    role_member::role_sources(roles, departments, org)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{desired_from_claims, desired_role_ids, principal_or_unusable};
    use crate::config::{AuthConfig, Tenancy};
    use crate::routes::auth::action::{AccountAction, AccountActionKind};

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn desired_roles_merge_roles_and_groups() {
        // roles と groups（先頭 / 除去）を統合し、重複を除く。
        let got = desired_role_ids(
            &v(&["engineering", "sales"]),
            &v(&["acme/eng", "acme"]),
            "acme",
        );
        assert_eq!(got, v(&["engineering", "sales", "acme/eng"]));
    }

    #[test]
    fn desired_roles_exclude_org_group() {
        // org そのもののグループは role 化しない。
        let got = desired_role_ids(&[], &v(&["acme"]), "acme");
        assert!(got.is_empty());
    }

    #[test]
    fn desired_roles_keep_role_named_like_org() {
        // roles claim に org と同名のロールがあっても正当なロールとして残す
        // （groups 由来の org のみ除外。誤剥奪防止・Codex #90 指摘）。
        let got = desired_role_ids(&v(&["acme"]), &v(&["acme"]), "acme");
        assert_eq!(got, v(&["acme"]));
    }

    #[test]
    fn department_roles_stay_in_own_tenant() {
        // `/tenants/<自テナント>/...` だけが部署 role になり、id は従来どおりテナント相対（#811）。
        // 他テナント配下（`/tenants/b/b/eng`）と、multi ではテナント無修飾の旧 group は role にしない。
        let groups = v(&[
            "/tenants/a/a",
            "/tenants/a/a/eng",
            "/tenants/b/b/eng",
            "/tenants/a2/a/sales",
            "/a/legacy",
        ]);
        let departments = authz::org_group::tenant_relative_groups(
            &groups,
            "a",
            authz::GroupLayout::TenantScoped,
        );
        let org = authz::org_group::resolve_org(&groups, "a", authz::GroupLayout::TenantScoped);
        assert_eq!(org.as_deref(), Some("a"));
        assert_eq!(desired_role_ids(&[], &departments, "a"), v(&["a/eng"]));
    }

    fn auth(tenancy: Tenancy) -> AuthConfig {
        AuthConfig {
            issuer: "http://localhost/realms/shiki".into(),
            internal_base_url: None,
            jwks_uri: None,
            audience: "shiki-api".into(),
            jwks_ttl_secs: 300,
            client_id: "shiki-web".into(),
            client_secret: None,
            redirect_uri: "http://localhost:3000/auth/callback".into(),
            post_logout_redirect_uri: "http://localhost:3000/".into(),
            scopes: "openid profile".into(),
            tenancy,
            tenant_id: Some("default".into()),
            provisioner_client_id: None,
            provisioner_client_secret: None,
            admin_base_url: None,
        }
    }

    fn principal(groups: &[&str], roles: &[&str]) -> authz::Principal {
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "u1".into(),
            email: None,
            groups: v(groups),
            roles: v(roles),
            tenant_id: Some("t1".into()),
        }
    }

    #[test]
    fn provision_roles_departments_in_multi_mode() {
        // multi: `/tenants/t1/...` の部署だけが（部署由来の）role になる。他テナント配下・
        // 無修飾の旧 group は org にも role にもならない。
        let p = principal(
            &[
                "/tenants/evil/aaa/x",
                "/tenants/t1/acme",
                "/tenants/t1/acme/eng",
                "/acme/legacy",
            ],
            &["sales"],
        );
        let (org, sourced) = desired_from_claims(&p, "t1", &auth(Tenancy::Multi));
        assert_eq!(org, "acme");
        assert_eq!(
            sourced,
            vec![("sales".to_string(), false), ("acme/eng".to_string(), true)]
        );
        // 自テナント配下が無ければ部署 role は無く、org は既定。
        let p = principal(&["/acme/eng", "/tenants/evil/acme/eng"], &[]);
        let (org, sourced) = desired_from_claims(&p, "t1", &auth(Tenancy::Multi));
        assert_eq!(org, "default");
        assert!(sourced.is_empty());
        // single は従来の直下 group を部署にする（オンプレは従来どおり）。
        let (org, sourced) = desired_from_claims(&p, "default", &auth(Tenancy::Single));
        assert_eq!(org, "acme");
        assert_eq!(sourced, vec![("acme/eng".to_string(), true)]);
    }

    #[test]
    fn unusable_sub_goes_back_to_the_login_page_or_the_settings() {
        let claims = |sub: &str| -> crate::middleware::claims::Claims {
            serde_json::from_value(serde_json::json!({ "sub": sub })).unwrap()
        };
        // UUID の sub はそのまま主体になる。
        let uuid = "0b6f3a52-8f1e-4c1a-9d1e-2f7a2c1b9e10";
        assert_eq!(principal_or_unusable(claims(uuid), None).unwrap().id, uuid);
        // ふつうのログインはログイン画面へ（画面が「このアカウントは使えません」を出す・D-205）。
        assert_eq!(
            principal_or_unusable(claims("f:ldap:bob"), None).unwrap_err(),
            "/login?error=account_unusable"
        );
        // 設定画面から始めたアカウント操作は、設定画面へ error で戻す（始めた人のセッションは残る）。
        let action = AccountAction {
            kind: AccountActionKind::Passkey,
            credential_id: None,
            subject: Some("u1".into()),
        };
        assert_eq!(
            principal_or_unusable(claims("a|b"), Some(&action)).unwrap_err(),
            "/settings/security?action=passkey&status=error"
        );
    }

    #[test]
    fn desired_roles_reject_forbidden_chars() {
        // FGA 構造文字を含む claim は同期しない（識別子注入防御）。
        let got = desired_role_ids(&v(&["a:b", "c#d", "e|f", " ", "ok"]), &[], "org");
        assert_eq!(got, v(&["ok"]));
    }
}
