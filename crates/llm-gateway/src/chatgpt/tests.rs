//! ChatGPT プラン接続の結線テスト（認証サーバ・推論 API をローカル axum モックで立てる）。
//!
//! ログイン（認可 URL → 貼られた戻り先 URL → コード交換）・保存・リフレッシュ（置き換え・失効）・
//! 切断（失効の送信）・連携者以外の拒否と、プランの形での推論呼び出しを確かめる。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures::StreamExt;
use serde_json::{json, Value};

use super::*;
use crate::model::{GenerateRequest, Message, Role, StreamDelta};
use crate::provider::LlmProvider;

#[derive(Default)]
struct Mock {
    /// トークンエンドポイントが順に返す応答。
    token_replies: VecDeque<(StatusCode, Value)>,
    token_forms: Vec<HashMap<String, String>>,
    revoked: Vec<HashMap<String, String>>,
    responses: Vec<(HeaderMap, Value)>,
}

type Shared = Arc<Mutex<Mock>>;

async fn token(
    State(m): State<Shared>,
    Form(f): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let mut m = m.lock().unwrap();
    m.token_forms.push(f);
    let (status, body) = m
        .token_replies
        .pop_front()
        .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, json!({})));
    (status, Json(body))
}

async fn spawn_mock() -> (String, Shared) {
    let shared: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let revoke_url = format!("{base}/oauth/revoke");
    let app = Router::new()
        .route("/api/accounts/oauth/token", post(token))
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let revoke_url = revoke_url.clone();
                async move { Json(json!({ "revocation_endpoint": revoke_url })) }
            }),
        )
        .route(
            "/oauth/revoke",
            post(|State(m): State<Shared>, Form(f): Form<HashMap<String, String>>| async move {
                m.lock().unwrap().revoked.push(f);
                StatusCode::OK
            }),
        )
        .route(
            "/v1/models",
            get(|| async {
                Json(json!({"models": [
                    {"slug": "gpt-6.1-sol", "display_name": "GPT-6.1 Sol", "visibility": "list"}
                ]}))
            }),
        )
        .route(
            "/v1/responses",
            post(|State(m): State<Shared>, headers: HeaderMap, Json(body): Json<Value>| async move {
                m.lock().unwrap().responses.push((headers, body));
                (
                    [("content-type", "text/event-stream")],
                    concat!(
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
                    ),
                )
            }),
        )
        .with_state(shared.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, shared)
}

fn ctx(id: &str) -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("t".into()),
        },
        "o".into(),
        "t".into(),
    )
}

fn id_token(client_id: &str, nonce: &str, sub: &str) -> String {
    let enc = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    format!(
        "{}.{}.sig",
        enc(json!({"alg": "RS256"})),
        enc(json!({
            "iss": "https://auth.openai.com", "aud": client_id, "sub": sub,
            "exp": Utc::now().timestamp() + 600, "nonce": nonce, "email": "me@example.com"
        }))
    )
}

fn token_reply(access: &str, refresh: &str, expires_in: i64, id_token: Option<String>) -> Value {
    let mut v = json!({
        "access_token": access, "refresh_token": refresh, "expires_in": expires_in,
        "token_type": "Bearer",
        "scope": "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
    });
    if let Some(t) = id_token {
        v["id_token"] = json!(t);
    }
    v
}

fn query(url: &str) -> HashMap<String, String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

struct Fixture {
    account: Arc<ChatGptAccount>,
    mock: Shared,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn fixture() -> Fixture {
    let (base, mock) = spawn_mock().await;
    let dir = std::env::temp_dir().join(format!("shiki-chatgpt-it-{}", uuid::Uuid::new_v4()));
    let settings = ChatGptSettings {
        credential_path: dir.join("chatgpt.json"),
        auth_base: base.clone(),
        api_base: format!("{base}/v1"),
    };
    Fixture {
        account: Arc::new(ChatGptAccount::open(reqwest::Client::new(), settings)),
        mock,
        dir,
    }
}

/// 新規登録のログインを最後まで通す（アクセストークンの寿命は `expires_in` 秒）。
async fn connect(f: &Fixture, who: &AuthContext, expires_in: i64) -> ChatGptStatus {
    let url = f.account.start_login(who).await.unwrap();
    let q = query(&url);
    assert_eq!(q["client_id"], DYNAMIC_CLIENT_ID);
    f.mock.lock().unwrap().token_replies.push_back((
        StatusCode::OK,
        token_reply(
            "at-1",
            "rt-1",
            expires_in,
            Some(id_token("oaiapp_1", &q["nonce"], "acct-1")),
        ),
    ));
    let pasted = format!(
        "http://127.0.0.1:1455/auth/callback?code=code-1&state={}&client_id=oaiapp_1",
        q["state"]
    );
    f.account.complete_login(who, &pasted).await.unwrap()
}

#[tokio::test]
async fn login_exchanges_the_pasted_code_and_persists_the_connection() {
    let f = fixture().await;
    let me = ctx("me");
    let status = connect(&f, &me, 3600).await;
    assert_eq!(status.connection, ChatGptConnection::Connected);
    assert_eq!(status.email.as_deref(), Some("me@example.com"));
    assert_eq!(status.models[0].slug, "gpt-6.1-sol");

    let form = f.mock.lock().unwrap().token_forms[0].clone();
    assert_eq!(form["grant_type"], "authorization_code");
    // 交換には発行されたクライアント ID と、サーバ側に持っていた verifier を使う。
    assert_eq!(form["client_id"], "oaiapp_1");
    assert_eq!(form["code"], "code-1");
    assert_eq!(form["redirect_uri"], oauth::REDIRECT_URI);
    assert_eq!(form["resource"], oauth::RESOURCE);
    assert!(!form["code_verifier"].is_empty());

    // 再起動しても接続は残る。
    let reopened = ChatGptAccount::open(reqwest::Client::new(), f.account.settings.clone());
    assert_eq!(
        reopened.status(&me).await.connection,
        ChatGptConnection::Connected
    );
    assert_eq!(reopened.access_token().await.unwrap(), "at-1");

    // 他の利用者には連携者のアカウントを見せず、ログインも切断もさせない。
    let other = ctx("other");
    let seen = f.account.status(&other).await;
    assert_eq!(seen.connection, ChatGptConnection::OtherUser);
    assert!(seen.email.is_none() && seen.models.is_empty());
    assert!(matches!(
        f.account.start_login(&other).await,
        Err(ChatGptError::OtherUser)
    ));
    assert!(matches!(
        f.account.disconnect(&other).await,
        Err(ChatGptError::OtherUser)
    ));
    assert!(f.account.ensure_owner(&other).await.is_err());
    assert!(f.account.ensure_owner(&me).await.is_ok());
}

#[tokio::test]
async fn pasted_url_must_match_the_pending_login() {
    let f = fixture().await;
    let me = ctx("me");
    // 始めていないログインの URL は受け付けない。
    let stale = "http://127.0.0.1:1455/auth/callback?code=c&state=s&client_id=oaiapp_1";
    assert!(matches!(
        f.account.complete_login(&me, stale).await,
        Err(ChatGptError::Invalid(_))
    ));
    let url = f.account.start_login(&me).await.unwrap();
    let state = query(&url)["state"].clone();
    // state 違い（古い URL）は弾き、途中状態は残す。
    assert!(f.account.complete_login(&me, stale).await.is_err());
    // 別の利用者が本人の URL を貼っても交換しない。
    let pasted =
        format!("http://127.0.0.1:1455/auth/callback?code=c&state={state}&client_id=oaiapp_1");
    assert!(f
        .account
        .complete_login(&ctx("other"), &pasted)
        .await
        .is_err());
    // プランの利用を許可しなかった（スコープが無い）なら接続しない。
    let nonce = query(&url)["nonce"].clone();
    let mut reply = token_reply("at", "rt", 3600, Some(id_token("oaiapp_1", &nonce, "a")));
    reply["scope"] = json!("openid email");
    f.mock
        .lock()
        .unwrap()
        .token_replies
        .push_back((StatusCode::OK, reply));
    let res = f.account.complete_login(&me, &pasted).await;
    assert!(matches!(res, Err(ChatGptError::Invalid(m)) if m.contains("許可")));
    assert_eq!(
        f.account.status(&me).await.connection,
        ChatGptConnection::NotConnected
    );
    assert!(f.mock.lock().unwrap().token_forms.len() == 1);
}

#[tokio::test]
async fn refresh_rotates_tokens_and_a_dead_refresh_token_asks_for_reauth() {
    let f = fixture().await;
    let me = ctx("me");
    // 寿命 60 秒（リフレッシュの余裕 5 分を切っている）。
    connect(&f, &me, 60).await;
    f.mock
        .lock()
        .unwrap()
        .token_replies
        .push_back((StatusCode::OK, token_reply("at-2", "rt-2", 60, None)));
    assert_eq!(f.account.access_token().await.unwrap(), "at-2");
    {
        let m = f.mock.lock().unwrap();
        let form = m.token_forms.last().unwrap();
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(form["client_id"], "oaiapp_1");
        assert_eq!(form["refresh_token"], "rt-1");
        assert!(!form.contains_key("scope"));
    }
    // 置き換わったリフレッシュトークンを次に使う（ファイルにも残る）。
    f.mock.lock().unwrap().token_replies.push_back((
        StatusCode::BAD_REQUEST,
        json!({"error": "refresh_token_reused"}),
    ));
    assert!(matches!(
        f.account.access_token().await,
        Err(LlmError::Config(_))
    ));
    assert_eq!(
        f.mock.lock().unwrap().token_forms.last().unwrap()["refresh_token"],
        "rt-2"
    );
    assert_eq!(
        f.account.status(&me).await.connection,
        ChatGptConnection::NeedsReauth
    );

    // 再ログインは発行済みのクライアントと ID トークンのヒントで行う。
    let q = query(&f.account.start_login(&me).await.unwrap());
    assert_eq!(q["client_id"], "oaiapp_1");
    assert!(q.contains_key("id_token_hint"));
    assert!(!q.contains_key("agent_name_hint"));
}

#[tokio::test]
async fn a_temporary_refresh_failure_keeps_the_connection() {
    let f = fixture().await;
    let me = ctx("me");
    connect(&f, &me, 60).await;
    // 応答を積まない＝500。まだ失効していないトークンで続け、資格情報は消さない。
    assert_eq!(f.account.access_token().await.unwrap(), "at-1");
    assert_eq!(
        f.account.status(&me).await.connection,
        ChatGptConnection::Connected
    );
}

#[tokio::test]
async fn disconnect_revokes_and_forgets_all_but_the_host_id() {
    let f = fixture().await;
    let me = ctx("me");
    let host = query(&f.account.start_login(&me).await.unwrap())["ext_agent_host_id"].clone();
    connect(&f, &me, 3600).await;
    f.account.disconnect(&me).await.unwrap();
    let revoked = f.mock.lock().unwrap().revoked.clone();
    assert_eq!(revoked[0]["token"], "rt-1");
    assert_eq!(revoked[0]["token_type_hint"], "refresh_token");
    assert_eq!(revoked[0]["client_id"], "oaiapp_1");
    assert_eq!(
        f.account.status(&me).await.connection,
        ChatGptConnection::NotConnected
    );
    assert!(f.account.access_token().await.is_err());
    // 次のログインは新規登録からだが、ホスト ID は変えない。他の利用者も連携できる。
    let q = query(&f.account.start_login(&ctx("other")).await.unwrap());
    assert_eq!(q["client_id"], DYNAMIC_CLIENT_ID);
    assert_eq!(q["ext_agent_host_id"], host);
}

#[tokio::test]
async fn provider_calls_responses_with_the_owner_token_in_plan_shape() {
    let f = fixture().await;
    connect(&f, &ctx("me"), 3600).await;
    let provider = ChatGptPlanProvider::new(
        reqwest::Client::new(),
        f.account.clone(),
        "gpt-6.1-sol".into(),
        crate::config::LlmTimeouts::default(),
    );
    assert_eq!(provider.name(), "chatgpt_plan");
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.max_tokens = Some(100);
    let deltas: Vec<_> = provider.stream(&req).await.unwrap().collect().await;
    assert!(matches!(&deltas[0], Ok(StreamDelta::TextDelta { text }) if text == "ok"));
    let (headers, body) = f.mock.lock().unwrap().responses[0].clone();
    assert_eq!(headers["authorization"], "Bearer at-1");
    assert_eq!(body["model"], "gpt-6.1-sol");
    assert_eq!(
        (body["store"].clone(), body["stream"].clone()),
        (json!(false), json!(true))
    );
    assert!(body.get("max_output_tokens").is_none());
}
