//! 会話の図（Yui の ```ui）のボタンの実行 API（Task 6.5・#604）。
//!
//! クライアントは `action_id + params` のみ送れる。ボタンは**保存済みの回答の本文**から
//! サーバが読み直し、束縛は回答の確定時に解決したものを引く。`gui::ActionDispatcher` が
//! 本人認可・監査を行う（アンビエント権限なし）。

use axum::{
    extract::{Path, State},
    Json,
};
use gui::{ActionError, ActionSource};
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
    /// 送信ごとの冪等キー（本文の図のワークフローのボタンでは必須・英数字と `-` `_` の 8〜64 文字）。
    /// 同じキーの再送は前の起動の結果を返し、run を増やさない（#604）。
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// 本文の図（```ui）のボタンの id か（`{16 桁の 16 進}:{ボタンの id}`・#604）。
fn is_yui_action(action_id: &str) -> bool {
    action_id.split_once(':').is_some_and(|(hash, id)| {
        hash.len() == 16
            && hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            && !id.is_empty()
    })
}

/// アクション実行レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct UiActionResponse {
    /// 束縛種別ごとの結果（handler: 実行結果 / workflow: run_id /
    /// data_view: `{"kind":"data_view","result": DataViewRun}`・ミニアプリの画面の表）。
    #[schema(value_type = Object)]
    pub result: serde_json::Value,
}

/// 図のボタンの誤り。値の検証（部品の定義・ワークフローの `input_schema`）の誤りは、画面が
/// 部品の横に出せるよう文言を `errors` に載せて返す（`BadRequest` は本文が `{status,title}` に潰れる）。
/// 文言は利用者が入れた値の検証結果で、内部の情報は含まない。
pub(crate) fn map_yui_action_err(e: ActionError) -> ApiError {
    match e {
        ActionError::Invalid(m) => ApiError::UnprocessableJson(serde_json::json!({
            "status": 400,
            "title": "Bad Request",
            "errors": [{ "code": "ui_action.invalid", "message": m }],
        })),
        other => map_action_err(other),
    }
}

pub(crate) fn map_action_err(e: ActionError) -> ApiError {
    match e {
        ActionError::NotFound => ApiError::NotFound,
        // 二重送信（同じカードの同じ操作を二度実行しようとした）。クライアントは 409 を
        // 「既に送信済み」として扱い、カードを送信済み表示へ倒す（#410）。
        ActionError::AlreadyInvoked => ApiError::Conflict,
        ActionError::Forbidden => ApiError::Forbidden,
        ActionError::Invalid(m) => ApiError::BadRequest(m),
        ActionError::Unavailable(m) => ApiError::ServiceUnavailable(m),
        // 表の索引の準備中は失敗ではない待ち。data の API と同じ構造化した 409（画面が進捗を出して読み直す）。
        ActionError::IndexPreparing { field, progress } => {
            ApiError::ConflictJson(serde_json::json!({
                "status": 409,
                "title": "Conflict",
                "code": "index_preparing",
                "field": field,
                "progress": progress,
            }))
        }
        ActionError::Internal(m) => ApiError::Internal(m),
    }
}

/// 回答の本文の図のボタンを実行する。
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
        (status = 409, description = "この操作は送信済み（単発アクションの二重送信）・同じ冪等キーの起動が途中"),
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

    // 図のボタンの id でなければ宣言されていない操作（Deny 監査を残して存在秘匿の 404）。
    if !is_yui_action(&req.action_id) {
        let source = ActionSource::ChatMessage {
            thread_id,
            message_id,
        };
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
    }
    // 編集者・本文からの読み直しと値の検証・回数・記録は chat 側の 1 か所で。
    let values = match req.params {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        _ => {
            return Err(ApiError::BadRequest(
                "params は項目と値の組（object）で送ってください".into(),
            ))
        }
    };
    let result = chat
        .invoke_yui_action(
            &ctx,
            &state.ui_actions,
            thread_id,
            message_id,
            chat::YuiActionRequest {
                action_id: req.action_id,
                values,
                idempotency_key: req.idempotency_key,
            },
            trace.as_deref(),
        )
        .await
        .map_err(map_yui_action_err)?;
    Ok(Json(UiActionResponse { result }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn yui_validation_errors_carry_the_message() {
        let ApiError::UnprocessableJson(body) =
            map_yui_action_err(ActionError::Invalid("金額: 入力してください".into()))
        else {
            unreachable!("検証の誤りは本文つきの 400")
        };
        assert_eq!(body["errors"][0]["message"], "金額: 入力してください");
        assert!(matches!(
            map_yui_action_err(ActionError::AlreadyInvoked),
            ApiError::Conflict
        ));
        // 表の索引の準備中は data の API と同じ構造化した 409（失敗ではない待ち）。
        let ApiError::ConflictJson(body) = map_yui_action_err(ActionError::IndexPreparing {
            field: "dept".into(),
            progress: Some(0.25),
        }) else {
            unreachable!("索引の準備中は構造化した 409")
        };
        assert_eq!(body["code"], "index_preparing");
        assert_eq!(body["field"], "dept");
    }

    #[test]
    fn yui_action_ids_are_recognised() {
        assert!(is_yui_action("0123456789abcdef:a1"));
        assert!(is_yui_action("0123456789abcdef:approve"));
        assert!(!is_yui_action("submit"));
        assert!(!is_yui_action("0123456789ABCDEF:a1"));
        assert!(!is_yui_action("0123456789abcdef:"));
        assert!(!is_yui_action("0123456789abcde:a1"));
    }
}
