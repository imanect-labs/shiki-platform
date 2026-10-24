//! `/me/chatgpt` — 本人の ChatGPT プランの接続（設定 → 接続済みアプリ・#854）。
//!
//! プレビュー環境だけで有効にする個人利用の機能（`llm.chatgpt.enabled`）。接続は環境に 1 件で、
//! 連携した本人だけがそのプラン枠で GPT モデルを使える（照合は llm-gateway が生成の前に行う）。
//!
//! OpenAI の戻り先は `127.0.0.1` 固定でサーバに届かないため、ログインは 2 段:
//! 1. `POST /me/chatgpt/login` で認可 URL を受け取り、ブラウザで開いて許可する。
//! 2. ブラウザが戻された `http://127.0.0.1:…/auth/callback?…` の URL を
//!    `POST /me/chatgpt/callback` に貼る。サーバが自分の PKCE verifier で交換する。
//!
//! アクセス・リフレッシュトークンはサーバ側のファイルにだけ置き、応答には含めない（再ログインの認可 URL
//! には、OpenAI の手順どおり前回の ID トークンが `id_token_hint` として載る）。連携・解除は監査に残す。

use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::{
    config::chatgpt_model_id,
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};
use llm_gateway::{ChatGptAccount, ChatGptConnection, ChatGptError, ChatGptStatus};
use storage::audit::{AuditAction, AuditEntry, Decision};

/// 接続の状態（本人から見た）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatGptConnectionState {
    /// この環境では ChatGPT 連携を有効にしていない。
    Unavailable,
    NotConnected,
    Connected,
    /// 連携していたが、再ログインが必要。
    NeedsReauth,
    /// 別の利用者が連携している。
    OtherUser,
}

/// 接続中のアカウントで使えるモデル。
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatGptModelInfo {
    /// ChatGPT 側のモデル ID（`llm.chatgpt.models` に書く値）。
    pub slug: String,
    pub display_name: String,
    /// 本人のモデル選択に出ているか（`llm.chatgpt.models` に含まれ、組織で公開されている）。
    pub enabled: bool,
}

/// `GET /me/chatgpt` のレスポンス（秘密値は含まない）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatGptStatusResponse {
    pub state: ChatGptConnectionState,
    /// 接続中の ChatGPT アカウントのメール（本人にだけ返す）。
    pub email: Option<String>,
    pub connected_at: Option<DateTime<Utc>>,
    /// 接続時に取得した、このアカウントで使えるモデル。
    pub models: Vec<ChatGptModelInfo>,
}

/// `POST /me/chatgpt/login` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatGptLoginResponse {
    /// ブラウザで開く ChatGPT の認可 URL（15 分以内に戻り先 URL を貼る）。
    pub authorize_url: String,
}

/// `POST /me/chatgpt/callback` のリクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ChatGptCallbackRequest {
    /// 許可のあとブラウザが開いた `http://127.0.0.1:…/auth/callback?…` の URL。
    pub callback_url: String,
}

/// 有効なら接続を返す（無効な環境では `None`）。
fn account(state: &AppState) -> Option<&std::sync::Arc<ChatGptAccount>> {
    state.llm.as_ref().and_then(|gw| gw.chatgpt())
}

fn require_account(state: &AppState) -> Result<&std::sync::Arc<ChatGptAccount>, ApiError> {
    account(state).ok_or(ApiError::NotFound)
}

/// 利用者に出せる文言は `detail` に載せる（入力・手順の誤りと競合だけ）。
fn api_error(e: ChatGptError) -> ApiError {
    match e {
        ChatGptError::Invalid(detail) => ApiError::UnprocessableJson(json!({
            "status": 400,
            "title": "Bad Request",
            "detail": detail,
        })),
        ChatGptError::OtherUser => ApiError::ConflictJson(json!({
            "status": 409,
            "title": "Conflict",
            "detail": ChatGptError::OtherUser.to_string(),
        })),
        ChatGptError::Upstream(e) => ApiError::ServiceUnavailable(format!("chatgpt auth: {e}")),
        ChatGptError::Storage(e) => ApiError::Internal(format!("chatgpt credentials: {e}")),
    }
}

/// `selectable` は本人のモデル選択に出ている slug（[`selectable_slugs`]）。
fn response(selectable: &[String], status: ChatGptStatus) -> ChatGptStatusResponse {
    ChatGptStatusResponse {
        state: match status.connection {
            ChatGptConnection::NotConnected => ChatGptConnectionState::NotConnected,
            ChatGptConnection::Connected => ChatGptConnectionState::Connected,
            ChatGptConnection::NeedsReauth => ChatGptConnectionState::NeedsReauth,
            ChatGptConnection::OtherUser => ChatGptConnectionState::OtherUser,
        },
        email: status.email,
        connected_at: status.connected_at,
        models: status
            .models
            .into_iter()
            .map(|m| ChatGptModelInfo {
                enabled: selectable.iter().any(|c| c == &m.slug),
                slug: m.slug,
                display_name: m.display_name,
            })
            .collect(),
    }
}

/// 本人のモデル選択に出ている slug（`llm.chatgpt.models` のうち、組織で公開されているもの）。
/// 接続していなければ組織の設定は読まない。表示の補助なので、組織の設定を読めなければ空を返す
/// （接続の結果や状態の表示そのものを失敗させない）。
async fn selectable_slugs(
    state: &AppState,
    ctx: &authz::AuthContext,
    status: &ChatGptStatus,
) -> Vec<String> {
    let Some(gw) = state.llm.as_ref() else {
        return Vec::new();
    };
    if status.connection != ChatGptConnection::Connected || status.models.is_empty() {
        return Vec::new();
    }
    let settings = match gw.settings_for(ctx).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "ChatGPT のモデルの公開状態を読めません（未公開として返す）");
            return Vec::new();
        }
    };
    state
        .config
        .llm
        .chatgpt
        .models
        .iter()
        .filter(|slug| settings.is_enabled(gw.catalog(), &chatgpt_model_id(slug)))
        .cloned()
        .collect()
}

/// 連携・解除を監査に残す（組織のデータを個人のアカウントへ送る委任の記録）。
/// 監査は済んだ操作の記録なので、失敗しても結果は変えず、欠落をログで顕在化する。
async fn audit(
    state: &AppState,
    ctx: &authz::AuthContext,
    trace: Option<&str>,
    action: AuditAction,
    metadata: serde_json::Value,
) {
    let object_id = format!("{}|{}|{}", ctx.tenant_id, ctx.org, ctx.principal.id);
    if let Err(e) = state
        .audit
        .record(
            ctx,
            AuditEntry {
                action,
                object_type: "chatgpt_connection",
                object_id: &object_id,
                decision: Decision::Allow,
                trace_id: trace,
                metadata,
            },
        )
        .await
    {
        tracing::error!(error = %e, %action, "ChatGPT 連携の監査記録に失敗");
    }
}

/// 本人の ChatGPT プランの接続状態。
#[utoipa::path(
    get,
    path = "/me/chatgpt",
    responses(
        (status = 200, description = "接続状態（無効な環境では state=unavailable）", body = ChatGptStatusResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn get_chatgpt(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<ChatGptStatusResponse>, ApiError> {
    let Some(account) = account(&state) else {
        return Ok(Json(ChatGptStatusResponse {
            state: ChatGptConnectionState::Unavailable,
            email: None,
            connected_at: None,
            models: Vec::new(),
        }));
    };
    let status = account.status(&ctx);
    let selectable = selectable_slugs(&state, &ctx, &status).await;
    Ok(Json(response(&selectable, status)))
}

/// ChatGPT でのログインを始める（認可 URL を返す）。
#[utoipa::path(
    post,
    path = "/me/chatgpt/login",
    responses(
        (status = 200, description = "認可 URL", body = ChatGptLoginResponse),
        (status = 401, description = "未認証"),
        (status = 404, description = "この環境では無効"),
        (status = 409, description = "別の利用者が連携している"),
    ),
    security(("session" = [])),
)]
pub async fn start_chatgpt_login(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<ChatGptLoginResponse>, ApiError> {
    let authorize_url = require_account(&state)?
        .start_login(&ctx)
        .await
        .map_err(api_error)?;
    Ok(Json(ChatGptLoginResponse { authorize_url }))
}

/// ブラウザが戻された URL を受け取り、接続を完了する。
#[utoipa::path(
    post,
    path = "/me/chatgpt/callback",
    request_body = ChatGptCallbackRequest,
    responses(
        (status = 200, description = "接続した", body = ChatGptStatusResponse),
        (status = 400, description = "URL が不正・ログインの期限切れ・プラン利用を許可していない（detail に理由）"),
        (status = 401, description = "未認証"),
        (status = 404, description = "この環境では無効"),
        (status = 409, description = "別の利用者が連携している"),
        (status = 503, description = "ChatGPT の認証サーバと通信できない"),
    ),
    security(("session" = [])),
)]
pub async fn complete_chatgpt_login(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(body): Json<ChatGptCallbackRequest>,
) -> Result<Json<ChatGptStatusResponse>, ApiError> {
    let status = require_account(&state)?
        .complete_login(&ctx, &body.callback_url)
        .await
        .map_err(api_error)?;
    audit(
        &state,
        &ctx,
        trace.as_deref(),
        AuditAction::MeChatgptConnect,
        json!({ "account_email": status.email }),
    )
    .await;
    let selectable = selectable_slugs(&state, &ctx, &status).await;
    Ok(Json(response(&selectable, status)))
}

/// ChatGPT プランの接続を外す（本人だけ）。
#[utoipa::path(
    delete,
    path = "/me/chatgpt",
    responses(
        (status = 204, description = "切断した"),
        (status = 401, description = "未認証"),
        (status = 404, description = "この環境では無効"),
        (status = 409, description = "別の利用者が連携している"),
    ),
    security(("session" = [])),
)]
pub async fn disconnect_chatgpt(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
) -> Result<StatusCode, ApiError> {
    let account = require_account(&state)?;
    // 連携していない呼び出しは何もしない（監査にも残さない）。
    let linked = account.is_owner(&ctx);
    account.disconnect(&ctx).await.map_err(api_error)?;
    if linked {
        audit(
            &state,
            &ctx,
            trace.as_deref(),
            AuditAction::MeChatgptDisconnect,
            json!({}),
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use llm_gateway::ChatGptModel;

    async fn body(e: ApiError) -> (StatusCode, serde_json::Value) {
        let res = e.into_response();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn user_facing_errors_carry_their_message_and_upstream_details_do_not() {
        let (status, json) = body(api_error(ChatGptError::Invalid(
            "貼り直してください".into(),
        )))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["detail"], "貼り直してください");

        let (status, json) = body(api_error(ChatGptError::OtherUser)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(json["detail"].as_str().unwrap().contains("別の利用者"));

        // 認証サーバ・ファイルの詳細は応答に出さない。
        let (status, json) = body(api_error(ChatGptError::Upstream("token=secret".into()))).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(json.get("detail").is_none());
        let (status, _) = body(api_error(ChatGptError::Storage("/var/lib".into()))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn response_marks_models_offered_in_this_environment() {
        let res = response(
            &["gpt-a".to_string()],
            ChatGptStatus {
                connection: ChatGptConnection::Connected,
                email: Some("me@example.com".into()),
                connected_at: None,
                models: vec![
                    ChatGptModel {
                        slug: "gpt-a".into(),
                        display_name: "A".into(),
                    },
                    ChatGptModel {
                        slug: "gpt-b".into(),
                        display_name: "B".into(),
                    },
                ],
            },
        );
        assert_eq!(res.state, ChatGptConnectionState::Connected);
        assert!(res.models[0].enabled);
        assert!(!res.models[1].enabled);
    }
}
