//! `/me/custom-instructions` — 本人のカスタム指示（設定 → パーソナライズ・#900）。
//!
//! 読み書きできるのは本人の分だけ（範囲は `AuthContext`・`ChatStore` が決める）。保存した内容は
//! 生成ワーカーが run の実行主体の分を読み、system プロンプトへ足す（chat::custom_instructions）。
//! 変更は監査に残す（本文は残さず、文字数と口調だけ）。

use axum::{extract::State, Json};
use chat::{ChatError, CustomInstructions};
use serde_json::json;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

use super::chat::chat_store;

/// 本人のカスタム指示（未登録なら空）。
#[utoipa::path(
    get, path = "/me/custom-instructions",
    responses(
        (status = 200, description = "本人のカスタム指示", body = CustomInstructions),
        (status = 401, description = "未認証"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_custom_instructions(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<CustomInstructions>, ApiError> {
    Ok(Json(
        chat_store(&state)?.get_custom_instructions(&ctx).await?,
    ))
}

/// 本人のカスタム指示を置き換える（すべて空にすると未設定に戻る）。保存後の値を返す。
#[utoipa::path(
    put, path = "/me/custom-instructions", request_body = CustomInstructions,
    responses(
        (status = 200, description = "保存したカスタム指示（前後の空白などを整えた値）", body = CustomInstructions),
        (status = 400, description = "上限（各 1500 文字）を超えた（`detail` に画面へ出せる文言）"),
        (status = 401, description = "未認証"),
        (status = 503, description = "chat 無効"),
    ),
    security(("session" = [])),
)]
pub async fn put_custom_instructions(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(body): Json<CustomInstructions>,
) -> Result<Json<CustomInstructions>, ApiError> {
    let saved = chat_store(&state)?
        .put_custom_instructions(&ctx, body, trace.as_deref())
        .await
        .map_err(|e| match e {
            // 入力の誤り（上限超過）は本人に見せる文言なので `detail` に載せる。
            ChatError::Invalid(detail) => ApiError::UnprocessableJson(json!({
                "status": 400,
                "title": "Bad Request",
                "detail": detail,
            })),
            other => other.into(),
        })?;
    Ok(Json(saved))
}
