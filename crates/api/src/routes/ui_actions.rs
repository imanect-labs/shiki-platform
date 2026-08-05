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

/// 同じ `action_id` を宣言する UI ブロックが複数あった（実行対象を決められない）。
#[derive(Debug)]
struct Ambiguous;

/// メッセージ内の検証済み generative_ui ブロックから `action_id` を宣言する文書を選ぶ。
///
/// 保存経路（emit_ui → 検証 → 永続化）を通った本文のみが存在するため、パース失敗は
/// 想定外データとして黙って読み飛ばす（実行面を fail-closed に保つ）。
///
/// action id の一意性を保証しているのは**スペック 1 件の中だけ**で、1 メッセージには複数の
/// generative_ui ブロックが載る。別々のカードが `submit` のような一般的な id を再利用して
/// いると「どの束縛を実行するか」も「どのカードを送信済みにするか」（#410 の実行台帳）も
/// 決まらない。先頭に当てて黙って進むと**押していないカードの束縛が動く**ので、曖昧さは
/// 実行前に拒否する（fail-closed）。
fn declaring_doc(
    content: &[ContentBlock],
    action_id: &str,
) -> Result<Option<UiSpecDoc>, Ambiguous> {
    let mut found: Option<UiSpecDoc> = None;
    for block in content {
        let ContentBlock::GenerativeUi { spec } = block else {
            continue;
        };
        let Ok(doc) = serde_json::from_value::<UiSpecDoc>(spec.clone()) else {
            continue;
        };
        if !doc.actions.iter().any(|a| a.id() == action_id) {
            continue;
        }
        if found.is_some() {
            return Err(Ambiguous);
        }
        found = Some(doc);
    }
    Ok(found)
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
        (status = 409, description = "この操作は送信済み（単発アクションの二重送信）"),
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

    let Ok(doc) = declaring_doc(&message.content, &req.action_id) else {
        state
            .ui_actions
            .deny(
                &ctx,
                &source,
                &req.action_id,
                "ambiguous_action",
                trace.as_deref(),
            )
            .await;
        return Err(ApiError::BadRequest(
            "このメッセージには同じ id のアクションが複数あり、実行対象を決められません".into(),
        ));
    };

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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ui(action_id: &str) -> ContentBlock {
        ContentBlock::GenerativeUi {
            spec: serde_json::json!({
                "version": 1,
                "actions": [{ "type": "handler", "id": action_id, "handler": "chat.submit" }],
                "root": { "component": "text", "text": "こんにちは" },
            }),
        }
    }

    #[test]
    fn declaring_doc_finds_the_only_block_that_declares_it() {
        let content = vec![
            ContentBlock::Text {
                text: "本文".into(),
            },
            ui("submit"),
        ];
        let doc = declaring_doc(&content, "submit").unwrap().unwrap();
        assert_eq!(doc.actions[0].id(), "submit");
        assert!(declaring_doc(&content, "other").unwrap().is_none());
    }

    /// 1 メッセージに複数のカードが載り、同じ id を再利用していたら実行しない（#410）。
    /// 先頭に当てて進むと、押していないカードの束縛が動いてしまう。
    #[test]
    fn duplicate_action_id_across_blocks_is_ambiguous() {
        let content = vec![ui("submit"), ui("submit")];
        assert!(declaring_doc(&content, "submit").is_err());
        // 壊れたスペックは読み飛ばす（曖昧扱いにしない）。
        let content = vec![
            ContentBlock::GenerativeUi {
                spec: serde_json::json!({ "こわれている": true }),
            },
            ui("submit"),
        ];
        assert!(declaring_doc(&content, "submit").unwrap().is_some());
    }
}
