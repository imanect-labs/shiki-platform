//! UI アクション実行 API（Task 6.5）。
//!
//! クライアントは `action_id + params` のみ送れる。束縛定義は**保存済み検証済みの
//! generative_ui ブロック**からサーバが引き、`gui::ActionDispatcher` が照合・本人認可・
//! 監査を行う（アンビエント権限なし）。

use axum::{
    extract::{Path, State},
    Json,
};
use chat::ContentBlock;
use gui::{ActionError, ActionSource, UiSpecDoc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// アクション実行リクエスト（これ以外は送れない＝束縛はサーバが引く）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UiActionRequest {
    pub action_id: String,
    /// アクションのパラメータ（フォーム値・ワークフロー入力）。
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
}

/// アクション実行レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct UiActionResponse {
    /// 束縛種別ごとの結果（handler: 実行結果 / tool: content / workflow: run_id）。
    #[schema(value_type = Object)]
    pub result: serde_json::Value,
}

pub(crate) fn map_action_err(e: ActionError) -> ApiError {
    match e {
        ActionError::NotFound => ApiError::NotFound,
        ActionError::Forbidden => ApiError::Forbidden,
        ActionError::Invalid(m) => ApiError::BadRequest(m),
        ActionError::Unavailable(m) => ApiError::ServiceUnavailable(m),
        ActionError::Internal(m) => ApiError::Internal(m),
    }
}

/// チャットメッセージ内 generative_ui ブロックのアクションを実行する。
#[utoipa::path(
    post,
    path = "/threads/{thread_id}/messages/{message_id}/ui-actions",
    params(
        ("thread_id" = Uuid, Path, description = "スレッド ID"),
        ("message_id" = Uuid, Path, description = "メッセージ ID"),
    ),
    request_body = UiActionRequest,
    responses(
        (status = 200, description = "実行した", body = UiActionResponse),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "対象が見つからない（未宣言アクション含む）"),
        (status = 503, description = "チャットまたは束縛先が無効"),
    ),
    security(("session" = [])),
)]
pub async fn invoke_chat_ui_action(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((thread_id, message_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UiActionRequest>,
) -> Result<Json<UiActionResponse>, ApiError> {
    let chat = state
        .chat
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("チャットが無効です".into()))?;

    // thread viewer 認可つきでメッセージを引く（存在秘匿は store 側の 404/403）。
    let message = chat
        .get_message(&ctx, thread_id, message_id, trace.as_deref())
        .await?;
    let source = ActionSource::ChatMessage {
        thread_id,
        message_id,
    };

    // メッセージ内の検証済み generative_ui ブロックから action_id を持つ文書を探す。
    // 保存経路（emit_ui → 検証 → 永続化）を通った本文のみが存在するため、パース失敗は
    // 想定外データとして黙って読み飛ばす（実行面を fail-closed に保つ）。
    let doc = message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::GenerativeUi { spec } => {
                serde_json::from_value::<UiSpecDoc>(spec.clone()).ok()
            }
            _ => None,
        })
        .find(|doc| doc.actions.iter().any(|a| a.id() == req.action_id));

    let Some(doc) = doc else {
        // 未宣言アクション（またはUIブロックなし）: Deny 監査を残して存在秘匿の 404。
        state
            .ui_actions
            .deny(
                &ctx,
                &source,
                &req.action_id,
                "undeclared_action",
                trace.as_deref(),
            )
            .await;
        return Err(ApiError::NotFound);
    };

    let result = state
        .ui_actions
        .dispatch(
            &ctx,
            &source,
            &doc,
            &req.action_id,
            req.params,
            trace.as_deref(),
        )
        .await
        .map_err(map_action_err)?;
    Ok(Json(UiActionResponse { result }))
}
