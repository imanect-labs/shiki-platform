//! `/me/preferences` — 本人のユーザー設定（設定 → チャット／表示・#952）。
//!
//! 読み書きできるのは本人の分だけ（範囲は `AuthContext`・`ChatStore` が決める）。型と検証は
//! `chat::preferences`。ここでは思考の深さを API の [`EffortLevel`] で出し入れするための DTO を置く
//! （`llm_gateway::Effort` は OpenAPI のスキーマを持たないため）。変更は監査に残す。

use axum::{extract::State, Json};
use chat::preferences::{DefaultModel, InputFormat, SubmitKey, TimeFormat, UserPreferences};
use chat::{AutonomousMode, ChatError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::chat::chat_store;
use super::llm_models::EffortLevel;

/// 新しいチャットの既定のモデル。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DefaultModelChoice {
    /// モデル ID（`GET /llm/models` の `id`）。`null` は「組織の既定のモデルのまま、思考の深さだけ変える」。
    #[serde(default)]
    pub model: Option<String>,
    pub effort: EffortLevel,
}

/// 本人のユーザー設定（未登録なら既定値。欠けた項目も既定値として扱う）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UserPreferencesBody {
    /// 入力欄の送信キー（既定 `enter`）。
    #[serde(default)]
    pub submit_key: SubmitKey,
    /// 入力欄の書き方（既定 `markdown`）。
    #[serde(default)]
    pub input_format: InputFormat,
    /// 新しいチャットの既定のモデル（`null` は Auto）。
    #[serde(default)]
    pub default_model: Option<DefaultModelChoice>,
    /// 新しいチャットで Web 検索を選んだ状態にする。
    #[serde(default)]
    pub web_search_by_default: bool,
    /// 新しいスレッドの承認モード（既定 `require_approval`）。`bypass` は組織が許可している場合だけ保存できる。
    #[serde(default)]
    pub default_autonomous_mode: AutonomousMode,
    /// 思考過程を開いた状態で表示する。
    #[serde(default)]
    pub expand_reasoning: bool,
    /// 日時の表示形式（既定 `absolute`）。
    #[serde(default)]
    pub time_format: TimeFormat,
}

/// `GET/PUT /me/preferences` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct UserPreferencesResponse {
    pub preferences: UserPreferencesBody,
    /// 組織ポリシで全自動（bypass）を選べるか（false なら画面は選択肢を無効化する）。
    pub bypass_allowed: bool,
}

impl From<UserPreferences> for UserPreferencesBody {
    fn from(p: UserPreferences) -> Self {
        UserPreferencesBody {
            submit_key: p.submit_key,
            input_format: p.input_format,
            default_model: p.default_model.map(|m| DefaultModelChoice {
                model: m.model,
                effort: m.effort.into(),
            }),
            web_search_by_default: p.web_search_by_default,
            default_autonomous_mode: p.default_autonomous_mode,
            expand_reasoning: p.expand_reasoning,
            time_format: p.time_format,
        }
    }
}

impl From<UserPreferencesBody> for UserPreferences {
    fn from(b: UserPreferencesBody) -> Self {
        UserPreferences {
            submit_key: b.submit_key,
            input_format: b.input_format,
            default_model: b.default_model.map(|m| DefaultModel {
                model: m.model,
                effort: m.effort.into(),
            }),
            web_search_by_default: b.web_search_by_default,
            default_autonomous_mode: b.default_autonomous_mode,
            expand_reasoning: b.expand_reasoning,
            time_format: b.time_format,
        }
    }
}

/// 本人のユーザー設定（未登録なら既定値）。
#[utoipa::path(
    get, path = "/me/preferences",
    responses(
        (status = 200, description = "本人のユーザー設定", body = UserPreferencesResponse),
        (status = 401, description = "未認証"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_preferences(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<UserPreferencesResponse>, ApiError> {
    let store = chat_store(&state)?;
    let (preferences, bypass_allowed) = tokio::try_join!(
        store.get_preferences(&ctx),
        store.autonomous_bypass_allowed(&ctx.tenant_id),
    )?;
    Ok(Json(UserPreferencesResponse {
        preferences: preferences.into(),
        bypass_allowed,
    }))
}

/// 本人のユーザー設定を置き換える。保存後の値を返す。
#[utoipa::path(
    put, path = "/me/preferences", request_body = UserPreferencesBody,
    responses(
        (status = 200, description = "保存したユーザー設定", body = UserPreferencesResponse),
        (status = 400, description = "組織ポリシで全自動が禁止されている・モデル ID が不正（`detail` に画面へ出せる文言）"),
        (status = 401, description = "未認証"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn put_preferences(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(body): Json<UserPreferencesBody>,
) -> Result<Json<UserPreferencesResponse>, ApiError> {
    let store = chat_store(&state)?;
    let saved = store
        .put_preferences(&ctx, body.into(), trace.as_deref())
        .await
        .map_err(|e| match e {
            // 入力の誤り（org ポリシ違反・不正なモデル ID）は本人に見せる文言なので `detail` に載せる。
            ChatError::Invalid(detail) => ApiError::UnprocessableJson(json!({
                "status": 400,
                "title": "Bad Request",
                "detail": detail,
            })),
            other => other.into(),
        })?;
    let bypass_allowed = store.autonomous_bypass_allowed(&ctx.tenant_id).await?;
    Ok(Json(UserPreferencesResponse {
        preferences: saved.into(),
        bypass_allowed,
    }))
}
