//! ChatGPT プラン（Sign in with ChatGPT の plan usage）での GPT 利用（#854・プレビュー環境の個人利用）。
//!
//! 利用者が自分の ChatGPT Plus / Pro のプラン枠で GPT モデルを使うための接続を、プロセスに 1 件だけ持つ。
//!
//! - **連携した本人だけが使える**: 資格情報は連携者（tenant_id ＋ org ＋ principal）に紐づけ、
//!   gateway が呼び出し主体と照合する（[`ChatGptAccount::ensure_owner`]）。他の利用者や
//!   ワークフロー等の実行主体からは使えない（プラン枠を他人に消費させない）。
//! - **トークンはサーバ側だけに置く**: 設定したパスのファイル（[`store`]・0600）に置き、
//!   ブラウザ・API 応答・ログへ出さない。
//! - **リフレッシュは直列化**: リフレッシュトークンは更新のたびに置き換わり、古い値を使うと
//!   失効する。1 プロセスで 1 つの [`ChatGptAccount`] を共有し、内部の Mutex で直列にする
//!   （複数レプリカで同じファイルを共有する構成は想定しない）。
//! - **ログインは URL の貼り付け**: OpenAI の戻り先は `127.0.0.1` 固定で、リモートのサーバには
//!   届かない。ブラウザが戻された URL を利用者に貼ってもらい、サーバ側に持つ PKCE verifier で
//!   交換する（[`oauth`]）。

mod oauth;
mod provider;
mod store;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    owner: Owner,
    state: String,
    nonce: String,
    verifier: String,
    /// 認可に使ったクライアント ID（初回は [`DYNAMIC_CLIENT_ID`]）。
    client_id: String,
    started: Instant,
}

/// ChatGPT プランの接続（プロセスで 1 つを共有する）。
pub struct ChatGptAccount {
    http: reqwest::Client,
    settings: ChatGptSettings,
    creds: tokio::sync::Mutex<Credentials>,
    pending: std::sync::Mutex<Option<PendingLogin>>,
}

impl std::fmt::Debug for ChatGptAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // トークンは出さない。
        f.debug_struct("ChatGptAccount")
            .field("credential_path", &self.settings.credential_path)
            .finish_non_exhaustive()
    }
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
            creds: tokio::sync::Mutex::new(creds),
            pending: std::sync::Mutex::new(None),
        }
    }

    /// 推論 API のベース URL。
    pub(crate) fn api_base(&self) -> &str {
        &self.settings.api_base
    }

    /// 呼び出した本人から見た接続状態。
    pub async fn status(&self, ctx: &AuthContext) -> ChatGptStatus {
        let creds = self.creds.lock().await;
        let connection = match &creds.owner {
            None => ChatGptConnection::NotConnected,
            Some(o) if !o.matches(ctx) => ChatGptConnection::OtherUser,
            Some(_) if creds.tokens.is_some() => ChatGptConnection::Connected,
            Some(_) => ChatGptConnection::NeedsReauth,
        };
        let mine = matches!(
            connection,
            ChatGptConnection::Connected | ChatGptConnection::NeedsReauth
        );
        ChatGptStatus {
            connection,
            email: mine
                .then(|| creds.account.as_ref().and_then(|a| a.email.clone()))
                .flatten(),
            connected_at: mine.then_some(creds.connected_at).flatten(),
            models: if mine {
                creds.models.clone()
            } else {
                Vec::new()
            },
        }
    }

    /// 呼び出した本人が連携者か。
    pub async fn is_owner(&self, ctx: &AuthContext) -> bool {
        self.creds
            .lock()
            .await
            .owner
            .as_ref()
            .is_some_and(|o| o.matches(ctx))
    }

    /// プラン枠を使ってよい主体か（gateway が生成の前に呼ぶ）。
    pub(crate) async fn ensure_owner(&self, ctx: &AuthContext) -> Result<(), LlmError> {
        if self.is_owner(ctx).await {
            Ok(())
        } else {
            Err(LlmError::Config(
                "このモデルは ChatGPT プランを連携した本人だけが使えます。".into(),
            ))
        }
    }

    /// ログインを始め、ブラウザで開く認可 URL を返す（前の途中状態は捨てる）。
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
            creds.ext_agent_host_id = Some(h.clone());
            self.save(&creds)?;
            h
        };
        // 本人の再ログイン（リフレッシュの失効など）は発行済みのクライアントで。
        let reauth = creds.owner.is_some() && creds.client_id.is_some() && creds.id_token.is_some();
        let (client_id, hint) = if reauth {
            (creds.client_id.clone(), creds.id_token.clone())
        } else {
            (None, None)
        };
        let client_id = client_id.unwrap_or_else(|| DYNAMIC_CLIENT_ID.to_string());
        let pending = PendingLogin {
            owner,
            state: oauth::random_token(),
            nonce: oauth::random_token(),
            verifier: oauth::random_token(),
            client_id,
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
        *self.pending_slot() = Some(pending);
        Ok(url)
    }

    /// 貼られた戻り先 URL で認可コードを交換し、接続を保存する。
    pub async fn complete_login(
        &self,
        ctx: &AuthContext,
        pasted_url: &str,
    ) -> Result<ChatGptStatus, ChatGptError> {
        let cb = oauth::parse_callback(pasted_url)?;
        let pending = self.take_pending(ctx, &cb.state)?;
        let client_id = if pending.client_id == DYNAMIC_CLIENT_ID {
            cb.client_id.clone().ok_or_else(|| {
                ChatGptError::Invalid(
                    "URL に client_id がありません。アドレスバーの URL を省略せずに貼ってください。"
                        .into(),
                )
            })?
        } else {
            if cb
                .client_id
                .as_ref()
                .is_some_and(|c| c != &pending.client_id)
            {
                return Err(ChatGptError::Invalid(
                    "以前の登録と異なる応答です。切断してから接続し直してください。".into(),
                ));
            }
            pending.client_id.clone()
        };

        let tokens = oauth::exchange_code(
            &self.http,
            &self.settings.auth_base,
            &client_id,
            &cb.code,
            &pending.verifier,
        )
        .await
        .map_err(|e| match e {
            TokenError::Rejected { .. } => ChatGptError::Invalid(
                "認可コードが無効か期限切れです。もう一度ログインしてください。".into(),
            ),
            TokenError::Transport(e) => ChatGptError::Upstream(e),
        })?;
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
        if creds.owner.as_ref().is_some_and(|o| o != &pending.owner) {
            return Err(ChatGptError::OtherUser);
        }
        if creds.owner.is_some() && creds.account.as_ref().is_some_and(|a| a.sub != claims.sub) {
            return Err(ChatGptError::Invalid(
                "以前と別の ChatGPT アカウントです。切断してから接続し直してください。".into(),
            ));
        }
        creds.client_id = Some(client_id);
        creds.owner = Some(pending.owner);
        creds.account = Some(AccountInfo {
            sub: claims.sub,
            email: claims.email,
        });
        creds.id_token = Some(id_token);
        creds.tokens = Some(Tokens {
            access_token: tokens.access_token.clone(),
            refresh_token,
            expires_at: tokens.expires_at(now),
            earliest_refresh_at: tokens.earliest_refresh(),
        });
        creds.connected_at = Some(now);
        creds.models = models;
        self.save(&creds)?;
        drop(creds);
        Ok(self.status(ctx).await)
    }

    /// 接続を外す（リフレッシュトークンの失効は best-effort）。本人以外は外せない。
    pub async fn disconnect(&self, ctx: &AuthContext) -> Result<(), ChatGptError> {
        let mut creds = self.creds.lock().await;
        if creds.owner.as_ref().is_some_and(|o| !o.matches(ctx)) {
            return Err(ChatGptError::OtherUser);
        }
        if let (Some(client_id), Some(tokens)) = (&creds.client_id, &creds.tokens) {
            if let Err(e) = oauth::revoke(
                &self.http,
                &self.settings.auth_base,
                client_id,
                &tokens.refresh_token,
            )
            .await
            {
                tracing::warn!(error = %e, "ChatGPT のリフレッシュトークンを失効できません（接続は外します）");
            }
        }
        creds.clear_connection();
        self.save(&creds)?;
        *self.pending_slot() = None;
        Ok(())
    }

    /// 使えるアクセストークンを返す（失効が近ければ直列にリフレッシュする）。
    pub(crate) async fn access_token(&self) -> Result<String, LlmError> {
        let mut creds = self.creds.lock().await;
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
        let fresh = now + REFRESH_MARGIN < tokens.expires_at;
        let too_early = tokens.earliest_refresh_at.is_some_and(|t| now < t);
        if fresh || (too_early && now < tokens.expires_at) {
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
                creds.tokens = None;
                if let Err(e) = self.save(&creds) {
                    tracing::error!(error = %e, "ChatGPT の資格情報を保存できません");
                }
                Err(not_connected())
            }
            Err(e) => {
                tracing::warn!(error = %e, "ChatGPT のトークンをリフレッシュできません");
                if now < tokens.expires_at {
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
    }

    fn save(&self, creds: &Credentials) -> Result<(), ChatGptError> {
        store::save(&self.settings.credential_path, creds)
            .map_err(|e| ChatGptError::Storage(e.to_string()))
    }

    fn pending_slot(&self) -> std::sync::MutexGuard<'_, Option<PendingLogin>> {
        // 途中状態は値を差し替えるだけなので、毒化しても中身はそのまま使える。
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 本人の・state が一致する・期限内の途中状態を取り出す（一致しなければ残す）。
    fn take_pending(&self, ctx: &AuthContext, state: &str) -> Result<PendingLogin, ChatGptError> {
        let mut slot = self.pending_slot();
        let restart = |m: &str| {
            ChatGptError::Invalid(format!("{m}「ChatGPT でログイン」からやり直してください。"))
        };
        let Some(p) = slot.as_ref() else {
            return Err(restart("ログインの途中状態がありません。"));
        };
        if !p.owner.matches(ctx) || p.state != state {
            return Err(restart("この URL は最新のログインのものではありません。"));
        }
        if p.started.elapsed() > PENDING_TTL {
            *slot = None;
            return Err(restart("ログインの有効期限が切れました。"));
        }
        slot.take()
            .ok_or_else(|| restart("ログインの途中状態がありません。"))
    }
}

#[cfg(test)]
mod tests;
