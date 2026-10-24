//! ChatGPT プラン（Sign in with ChatGPT の plan usage）での GPT 利用（#854・プレビュー環境の個人利用）。
//!
//! 利用者が自分の ChatGPT Plus / Pro のプラン枠で GPT モデルを使うための接続を、プロセスに 1 件だけ持つ。
//!
//! - **連携した本人だけが使える**: 資格情報は連携者（tenant_id ＋ org ＋ principal）に紐づけ、
//!   gateway が呼び出し主体と照合する（[`ChatGptAccount::ensure_owner`]）。他の利用者や
//!   ワークフロー等の実行主体からは使えない（プラン枠を他人に消費させない）。
//! - **アクセス・リフレッシュトークンはサーバ側だけに置く**: 設定したパスのファイル（[`store`]・
//!   0600）に置き、ブラウザ・API 応答・ログへ出さない。再ログインでは OpenAI の手順どおり、前回の
//!   ID トークンを `id_token_hint` として認可 URL に載せる（本人のブラウザ経由で認可サーバへ渡る。
//!   アカウントの識別用で、API の認可には使えない）。
//! - **リフレッシュは直列化**: リフレッシュトークンは更新のたびに置き換わり、古い値を使うと
//!   失効する。1 プロセスで 1 つの [`ChatGptAccount`] を共有し、内部の Mutex で直列にする
//!   （複数レプリカで同じファイルを共有する構成は想定しない）。リフレッシュの HTTP を待つ間も
//!   このロックを持つので、全利用者が通る照合・表示（[`ChatGptAccount::is_owner`]・
//!   [`ChatGptAccount::status`]）は秘密を含まない写し（`View`）だけを読み、このロックを取らない。
//! - **ログインは URL の貼り付け**: OpenAI の戻り先は `127.0.0.1` 固定で、リモートのサーバには
//!   届かない。ブラウザが戻された URL を利用者に貼ってもらい、サーバ側に持つ PKCE verifier で
//!   交換する（[`oauth`]）。

mod oauth;
mod provider;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use authz::{AuthContext, PrincipalKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::provider::LlmError;
use oauth::{AuthorizeParams, TokenError, DYNAMIC_CLIENT_ID};
use store::{AccountInfo, Credentials, Tokens};

pub(crate) use provider::ChatGptPlanProvider;

/// ChatGPT プランへ振るプロバイダの ID（モデルの `provider` が指す）。
pub const CHATGPT_PROVIDER_ID: &str = "chatgpt";

/// ログインを始めてから、戻り先 URL を貼るまでの猶予。
const PENDING_TTL: Duration = Duration::from_mins(15);
/// アクセストークンの失効までこれを切ったらリフレッシュする。
const REFRESH_MARGIN: chrono::Duration = chrono::Duration::minutes(5);
/// リフレッシュが一時障害で失敗したら、トークンが有効な間はこの間隔を置いて試し直す
/// （本人の生成のたびに認証サーバを待たせない）。
const REFRESH_RETRY_AFTER: Duration = Duration::from_mins(1);

/// 接続の設定。
#[derive(Debug, Clone)]
pub struct ChatGptSettings {
    /// 資格情報ファイルのパス。
    pub credential_path: PathBuf,
    /// 認証サーバ（既定 `https://auth.openai.com`）。テストで差し替える。
    pub auth_base: String,
    /// 推論 API（既定 `https://api.openai.com/v1`）。テストで差し替える。
    pub api_base: String,
}

impl ChatGptSettings {
    /// 既定の接続先で組む。
    pub fn new(credential_path: impl Into<PathBuf>) -> Self {
        ChatGptSettings {
            credential_path: credential_path.into(),
            auth_base: "https://auth.openai.com".to_string(),
            api_base: oauth::RESOURCE.to_string(),
        }
    }
}

/// このアカウントで選べるモデル（`/v1/models` の `slug` と表示名）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatGptModel {
    pub slug: String,
    pub display_name: String,
}

/// 接続の状態（呼び出した本人から見た）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatGptConnection {
    /// 未接続（だれも連携していない）。
    NotConnected,
    /// 本人が接続済み。
    Connected,
    /// 本人が連携していたが、リフレッシュが失効した（再ログインが要る）。
    NeedsReauth,
    /// 別の利用者が連携している（この環境では使えない）。
    OtherUser,
}

/// 接続の状態（設定画面の表示用・秘密を含まない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptStatus {
    pub connection: ChatGptConnection,
    /// ChatGPT アカウントのメール（本人にだけ見せる）。
    pub email: Option<String>,
    pub connected_at: Option<DateTime<Utc>>,
    /// 接続時に取得した、このアカウントで使えるモデル。
    pub models: Vec<ChatGptModel>,
}

/// 接続操作のエラー（API 層で HTTP ステータスへ写す）。
#[derive(Debug, thiserror::Error)]
pub enum ChatGptError {
    /// 利用者の入力や手順の誤り（400）。文言はそのまま利用者へ出す。
    #[error("{0}")]
    Invalid(String),
    /// 別の利用者が連携している（409）。
    #[error("この環境の ChatGPT 接続は別の利用者が使っています。")]
    OtherUser,
    /// 認証サーバに到達できない・想定外の応答（502）。
    #[error("ChatGPT の認証サーバとの通信に失敗しました: {0}")]
    Upstream(String),
    /// 資格情報ファイルを保存できない（500）。
    #[error("ChatGPT の資格情報を保存できませんでした: {0}")]
    Storage(String),
}

/// 連携した本人（このプラン枠を使える唯一の主体）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Owner {
    tenant_id: String,
    org: String,
    principal_id: String,
}

impl Owner {
    /// 対話する本人（ユーザー）だけが連携・利用できる。
    fn of(ctx: &AuthContext) -> Option<Owner> {
        (ctx.principal.kind == PrincipalKind::User).then(|| Owner {
            tenant_id: ctx.tenant_id.clone(),
            org: ctx.org.clone(),
            principal_id: ctx.principal.id.clone(),
        })
    }

    fn matches(&self, ctx: &AuthContext) -> bool {
        Owner::of(ctx).is_some_and(|o| &o == self)
    }
}

/// ログインの途中状態（戻り先 URL を貼るまでサーバ側に持つ）。
struct PendingLogin {
    state: String,
    nonce: String,
    verifier: String,
    /// 認可に使ったクライアント ID（初回は [`DYNAMIC_CLIENT_ID`]）。
    client_id: String,
    started: Instant,
}

/// 照合・表示に使う写し（秘密を含まない）。資格情報を書き換えるたびに作り直す。
#[derive(Clone, Default)]
struct View {
    owner: Option<Owner>,
    /// 使えるトークンがあるか（無ければ未接続か再ログイン待ち）。
    connected: bool,
    email: Option<String>,
    connected_at: Option<DateTime<Utc>>,
    models: Vec<ChatGptModel>,
}

impl View {
    fn of(creds: &Credentials) -> View {
        View {
            owner: creds.owner.clone(),
            connected: creds.tokens.is_some(),
            email: creds.account.as_ref().and_then(|a| a.email.clone()),
            connected_at: creds.connected_at,
            models: creds.models.clone(),
        }
    }
}

/// ChatGPT プランの接続（プロセスで 1 つを共有する）。
pub struct ChatGptAccount {
    http: reqwest::Client,
    settings: ChatGptSettings,
    /// 資格情報。書き換えとリフレッシュを直列にする（リフレッシュの HTTP を待つ間も持つ）。
    creds: tokio::sync::Mutex<Credentials>,
    /// 照合・表示用の写し（全利用者のモデル解決が読むので、上のロックと分ける）。
    view: std::sync::RwLock<View>,
    /// ログインの途中状態（利用者ごと。他人の途中状態を上書きしない）。
    pending: std::sync::Mutex<HashMap<Owner, PendingLogin>>,
    /// 直近のリフレッシュが一時障害で失敗した時刻。
    refresh_failed_at: std::sync::Mutex<Option<Instant>>,
}

impl std::fmt::Debug for ChatGptAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // トークンは出さない。
        f.debug_struct("ChatGptAccount")
            .field("credential_path", &self.settings.credential_path)
            .finish_non_exhaustive()
    }
}

/// 毒化しても中身はそのまま使える（値を差し替えるだけの状態）。
fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ChatGptAccount {
    /// 資格情報ファイルを読んで組む。読めないファイルは未接続として始める（LLM 全体は止めない）。
    pub fn open(http: reqwest::Client, settings: ChatGptSettings) -> Self {
        let creds = match store::load(&settings.credential_path) {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    path = %settings.credential_path.display(),
                    "ChatGPT の資格情報ファイルを読めません。未接続として始めます"
                );
                Credentials::default()
            }
        };
        ChatGptAccount {
            http,
            settings,
            view: std::sync::RwLock::new(View::of(&creds)),
            creds: tokio::sync::Mutex::new(creds),
            pending: std::sync::Mutex::new(HashMap::new()),
            refresh_failed_at: std::sync::Mutex::new(None),
        }
    }

    /// 推論 API のベース URL。
    pub(crate) fn api_base(&self) -> &str {
        &self.settings.api_base
    }

    fn view(&self) -> View {
        self.view
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 資格情報を保存してから、メモリと写しを差し替える（保存に失敗したら何も変えない）。
    fn commit(&self, creds: &mut Credentials, next: Credentials) -> Result<(), ChatGptError> {
        self.save(&next)?;
        *self
            .view
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = View::of(&next);
        *creds = next;
        Ok(())
    }

    /// 呼び出した本人から見た接続状態（資格情報のロックを取らない）。
    pub fn status(&self, ctx: &AuthContext) -> ChatGptStatus {
        let view = self.view();
        let connection = match &view.owner {
            None => ChatGptConnection::NotConnected,
            Some(o) if !o.matches(ctx) => ChatGptConnection::OtherUser,
            Some(_) if view.connected => ChatGptConnection::Connected,
            Some(_) => ChatGptConnection::NeedsReauth,
        };
        if !matches!(
            connection,
            ChatGptConnection::Connected | ChatGptConnection::NeedsReauth
        ) {
            return ChatGptStatus {
                connection,
                email: None,
                connected_at: None,
                models: Vec::new(),
            };
        }
        ChatGptStatus {
            connection,
            email: view.email,
            connected_at: view.connected_at,
            models: view.models,
        }
    }

    /// 呼び出した本人が連携者か（資格情報のロックを取らない）。
    pub fn is_owner(&self, ctx: &AuthContext) -> bool {
        self.view().owner.as_ref().is_some_and(|o| o.matches(ctx))
    }

    /// プラン枠を使ってよい主体か（gateway が生成の前に呼ぶ）。
    pub(crate) fn ensure_owner(&self, ctx: &AuthContext) -> Result<(), LlmError> {
        if self.is_owner(ctx) {
            Ok(())
        } else {
            Err(LlmError::Config(
                "このモデルは ChatGPT プランを連携した本人だけが使えます。".into(),
            ))
        }
    }

    /// ログインを始め、ブラウザで開く認可 URL を返す（本人の前の途中状態は捨てる）。
    pub async fn start_login(&self, ctx: &AuthContext) -> Result<String, ChatGptError> {
        let owner = Owner::of(ctx).ok_or_else(|| {
            ChatGptError::Invalid("ユーザーとしてログインしているときだけ接続できます。".into())
        })?;
        let mut creds = self.creds.lock().await;
        if creds.owner.as_ref().is_some_and(|o| o != &owner) {
            return Err(ChatGptError::OtherUser);
        }
        // ホスト ID は初回ログインの前に決めて保存し、以後は変えない。
        let host_id = if let Some(h) = &creds.ext_agent_host_id {
            h.clone()
        } else {
            let h = format!("urn:uuid:{}", uuid::Uuid::new_v4());
            let mut next = creds.clone();
            next.ext_agent_host_id = Some(h.clone());
            self.commit(&mut creds, next)?;
            h
        };
        // 本人の再ログイン（リフレッシュの失効など）は発行済みのクライアントで。
        let reauth = creds.owner.is_some() && creds.client_id.is_some() && creds.id_token.is_some();
        let (client_id, hint) = if reauth {
            (creds.client_id.clone(), creds.id_token.clone())
        } else {
            (None, None)
        };
        drop(creds);
        let pending = PendingLogin {
            state: oauth::random_token(),
            nonce: oauth::random_token(),
            verifier: oauth::random_token(),
            client_id: client_id.unwrap_or_else(|| DYNAMIC_CLIENT_ID.to_string()),
            started: Instant::now(),
        };
        let url = oauth::authorize_url(
            &self.settings.auth_base,
            &AuthorizeParams {
                client_id: &pending.client_id,
                host_id: &host_id,
                id_token_hint: hint.as_deref(),
                state: &pending.state,
                nonce: &pending.nonce,
                verifier: &pending.verifier,
            },
        )?;
        let mut slots = lock(&self.pending);
        slots.retain(|_, p| p.started.elapsed() <= PENDING_TTL);
        slots.insert(owner, pending);
        Ok(url)
    }

    /// 貼られた戻り先 URL で認可コードを交換し、接続を保存する。
    pub async fn complete_login(
        &self,
        ctx: &AuthContext,
        pasted_url: &str,
    ) -> Result<ChatGptStatus, ChatGptError> {
        let cb = oauth::parse_callback(pasted_url)?;
        let (owner, pending, client_id) = self.take_pending(ctx, &cb)?;

        let tokens = match oauth::exchange_code(
            &self.http,
            &self.settings.auth_base,
            &client_id,
            &cb.code,
            &pending.verifier,
        )
        .await
        {
            Ok(t) => t,
            Err(TokenError::Rejected { .. }) => {
                return Err(ChatGptError::Invalid(
                    "認可コードが無効か期限切れです。もう一度ログインしてください。".into(),
                ));
            }
            Err(TokenError::Transport(e)) => {
                // 認証サーバに届いていない一時障害。コードはまだ使えるので、同じ URL で試し直せるよう戻す。
                lock(&self.pending).entry(owner).or_insert(pending);
                return Err(ChatGptError::Upstream(e));
            }
        };
        if !tokens.grants_plan_usage() {
            return Err(ChatGptError::Invalid(
                "ChatGPT プランの利用が許可されませんでした。Plus / Pro のアカウントで、プランの利用を許可してください。"
                    .into(),
            ));
        }
        let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
            ChatGptError::Upstream("リフレッシュトークンが返りませんでした".into())
        })?;
        let id_token = tokens
            .id_token
            .clone()
            .ok_or_else(|| ChatGptError::Upstream("ID トークンが返りませんでした".into()))?;
        let now = Utc::now();
        let claims = oauth::id_claims(&id_token, &client_id, &pending.nonce, now)
            .map_err(|e| ChatGptError::Upstream(format!("ID トークンを確認できません（{e}）")))?;
        let models = oauth::list_models(&self.http, &self.settings.api_base, &tokens.access_token)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "ChatGPT のモデル一覧を取得できません");
                Vec::new()
            });

        let mut creds = self.creds.lock().await;
        // 途中で別の利用者が連携した・別の ChatGPT アカウントで再ログインした場合は上書きしない。
        if creds.owner.as_ref().is_some_and(|o| o != &owner) {
            return Err(ChatGptError::OtherUser);
        }
        if creds.owner.is_some() && creds.account.as_ref().is_some_and(|a| a.sub != claims.sub) {
            return Err(ChatGptError::Invalid(
                "以前と別の ChatGPT アカウントです。連携を解除してから接続し直してください。"
                    .into(),
            ));
        }
        let mut next = creds.clone();
        next.client_id = Some(client_id);
        next.owner = Some(owner);
        next.account = Some(AccountInfo {
            sub: claims.sub,
            email: claims.email,
        });
        next.id_token = Some(id_token);
        next.tokens = Some(Tokens {
            access_token: tokens.access_token.clone(),
            refresh_token,
            expires_at: tokens.expires_at(now),
            earliest_refresh_at: tokens.earliest_refresh(),
        });
        next.connected_at = Some(now);
        next.models = models;
        self.commit(&mut creds, next)?;
        drop(creds);
        *lock(&self.refresh_failed_at) = None;
        Ok(self.status(ctx))
    }

    /// 接続を外す。本人以外は外せない。先に外して保存し、リフレッシュトークンの失効は
    /// 裏で best-effort に送る（認証サーバが遅くても切断そのものは待たせない・打ち切られない）。
    pub async fn disconnect(&self, ctx: &AuthContext) -> Result<(), ChatGptError> {
        let mut creds = self.creds.lock().await;
        if creds.owner.as_ref().is_some_and(|o| !o.matches(ctx)) {
            return Err(ChatGptError::OtherUser);
        }
        let revoke = creds
            .client_id
            .clone()
            .zip(creds.tokens.as_ref().map(|t| t.refresh_token.clone()));
        let mut next = creds.clone();
        next.clear_connection();
        self.commit(&mut creds, next)?;
        drop(creds);
        if let Some(owner) = Owner::of(ctx) {
            lock(&self.pending).remove(&owner);
        }
        if let Some((client_id, refresh_token)) = revoke {
            let (http, auth_base) = (self.http.clone(), self.settings.auth_base.clone());
            tokio::spawn(async move {
                if let Err(e) = oauth::revoke(&http, &auth_base, &client_id, &refresh_token).await {
                    tracing::warn!(error = %e, "ChatGPT のリフレッシュトークンを失効できません（接続は外しました）");
                }
            });
        }
        Ok(())
    }

    /// 呼び出し主体のアクセストークンを返す（失効が近ければ直列にリフレッシュする）。
    ///
    /// 連携者の照合とトークンの取り出しは、資格情報のロックの中で一度に行う（照合と取得の間に
    /// 連携者が入れ替わっても、別人のトークンで生成しない）。処理は独立したタスクで行い、呼び出し側が
    /// 生成を止めて future を捨てても、始めたリフレッシュは応答の受信と保存まで終える（ローテーション
    /// 済みのリフレッシュトークンを失って再ログインが要る状態にしない）。
    pub(crate) async fn access_token(
        self: &std::sync::Arc<Self>,
        ctx: &AuthContext,
    ) -> Result<String, LlmError> {
        let (account, ctx) = (std::sync::Arc::clone(self), ctx.clone());
        tokio::spawn(async move { account.access_token_now(&ctx).await })
            .await
            .map_err(|e| LlmError::Internal(format!("chatgpt token task: {e}")))?
    }

    async fn access_token_now(&self, ctx: &AuthContext) -> Result<String, LlmError> {
        let mut creds = self.creds.lock().await;
        if !creds.owner.as_ref().is_some_and(|o| o.matches(ctx)) {
            return Err(LlmError::Config(
                "このモデルは ChatGPT プランを連携した本人だけが使えます。".into(),
            ));
        }
        let not_connected = || {
            LlmError::Config(
                "ChatGPT の再ログインが必要です。設定 → 接続済みアプリから接続してください。"
                    .into(),
            )
        };
        let (Some(tokens), Some(client_id)) = (creds.tokens.clone(), creds.client_id.clone())
        else {
            return Err(not_connected());
        };
        let now = Utc::now();
        let valid = now < tokens.expires_at;
        let fresh = now + REFRESH_MARGIN < tokens.expires_at;
        let too_early = tokens.earliest_refresh_at.is_some_and(|t| now < t);
        let backing_off =
            lock(&self.refresh_failed_at).is_some_and(|t| t.elapsed() < REFRESH_RETRY_AFTER);
        if fresh || (valid && (too_early || backing_off)) {
            return Ok(tokens.access_token);
        }
        match oauth::refresh(
            &self.http,
            &self.settings.auth_base,
            &client_id,
            &tokens.refresh_token,
        )
        .await
        {
            Ok(t) => {
                *lock(&self.refresh_failed_at) = None;
                let access = t.access_token.clone();
                creds.tokens = Some(Tokens {
                    access_token: t.access_token.clone(),
                    refresh_token: t.refresh_token.clone().unwrap_or(tokens.refresh_token),
                    expires_at: t.expires_at(now),
                    earliest_refresh_at: t.earliest_refresh(),
                });
                if let Err(e) = self.save(&creds) {
                    // メモリ上の新しいトークンで続ける（再起動までは使える）。
                    tracing::error!(error = %e, "リフレッシュした ChatGPT のトークンを保存できません");
                }
                Ok(access)
            }
            Err(e) if e.is_terminal() => {
                tracing::warn!(error = %e, "ChatGPT のリフレッシュトークンが失効しました");
                let mut next = creds.clone();
                next.tokens = None;
                if let Err(e) = self.commit(&mut creds, next) {
                    tracing::error!(error = %e, "ChatGPT の資格情報を保存できません");
                }
                Err(not_connected())
            }
            Err(e) => {
                tracing::warn!(error = %e, "ChatGPT のトークンをリフレッシュできません");
                *lock(&self.refresh_failed_at) = Some(Instant::now());
                if valid {
                    Ok(tokens.access_token)
                } else {
                    Err(LlmError::Unavailable(format!("chatgpt token refresh: {e}")))
                }
            }
        }
    }

    /// 推論 API が 401 を返した。次の呼び出しでリフレッシュさせる。
    pub(crate) async fn invalidate_access_token(&self) {
        if let Some(t) = self.creds.lock().await.tokens.as_mut() {
            t.expires_at = Utc::now();
            t.earliest_refresh_at = None;
        }
        *lock(&self.refresh_failed_at) = None;
    }

    fn save(&self, creds: &Credentials) -> Result<(), ChatGptError> {
        store::save(&self.settings.credential_path, creds)
            .map_err(|e| ChatGptError::Storage(e.to_string()))
    }

    /// 本人の・state が一致する・期限内の途中状態を、戻り先の検証を済ませてから取り出す
    /// （貼り間違いで捨てない。一致しなければ残す）。交換に使うクライアント ID も返す。
    fn take_pending(
        &self,
        ctx: &AuthContext,
        cb: &oauth::Callback,
    ) -> Result<(Owner, PendingLogin, String), ChatGptError> {
        let restart = |m: &str| {
            ChatGptError::Invalid(format!("{m}「ChatGPT でログイン」からやり直してください。"))
        };
        let owner = Owner::of(ctx).ok_or_else(|| restart("ログインの途中状態がありません。"))?;
        let mut slots = lock(&self.pending);
        let Some(p) = slots.get(&owner) else {
            return Err(restart("ログインの途中状態がありません。"));
        };
        if p.state != cb.state {
            return Err(restart("この URL は最新のログインのものではありません。"));
        }
        if p.started.elapsed() > PENDING_TTL {
            slots.remove(&owner);
            return Err(restart("ログインの有効期限が切れました。"));
        }
        let client_id = if p.client_id == DYNAMIC_CLIENT_ID {
            cb.client_id.clone().ok_or_else(|| {
                ChatGptError::Invalid(
                    "URL に client_id がありません。アドレスバーの URL を省略せずに貼ってください。"
                        .into(),
                )
            })?
        } else {
            if cb.client_id.as_ref().is_some_and(|c| c != &p.client_id) {
                return Err(ChatGptError::Invalid(
                    "以前の登録と異なる応答です。連携を解除してから接続し直してください。".into(),
                ));
            }
            p.client_id.clone()
        };
        let pending = slots
            .remove(&owner)
            .ok_or_else(|| restart("ログインの途中状態がありません。"))?;
        Ok((owner, pending, client_id))
    }
}

#[cfg(test)]
mod tests;
