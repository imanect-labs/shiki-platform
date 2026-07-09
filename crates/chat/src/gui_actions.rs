//! generative UI アクションの chat 側ハンドラ（Task 6.5 の②）。
//!
//! `chat.submit` はフォーム値を整形テキストとしてスレッドへ投稿する。認可は
//! [`ChatStore::post_message`]（editor 要求＋監査）の既存チョークポイントに委ねる
//! （このハンドラ自身は権限を持たない・昇格しない）。

use gui::{ActionError, ActionHandler, ActionSource, HandlerKind};

use crate::store::ChatStore;
use crate::ChatError;

/// フォーム送信をスレッド投稿へ写すハンドラ。
pub struct ChatSubmitHandler {
    store: ChatStore,
}

impl ChatSubmitHandler {
    pub fn new(store: ChatStore) -> Self {
        ChatSubmitHandler { store }
    }
}

#[async_trait::async_trait]
impl ActionHandler for ChatSubmitHandler {
    fn kind(&self) -> HandlerKind {
        HandlerKind::ChatSubmit
    }

    async fn invoke(
        &self,
        ctx: &authz::AuthContext,
        source: &ActionSource,
        params: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<serde_json::Value, ActionError> {
        // chat.submit は「そのスレッドの UI ブロック」からのみ意味を持つ（ミニアプリは対象外）。
        let ActionSource::ChatMessage { thread_id, .. } = source else {
            return Err(ActionError::Invalid(
                "chat.submit はチャット内 UI からのみ実行できます".into(),
            ));
        };
        let text = format_form_text(&params);
        if text.is_empty() {
            return Err(ActionError::Invalid("フォーム値が空です".into()));
        }
        let result = self
            .store
            .post_message(ctx, *thread_id, &text, &[], None, false, trace_id)
            .await
            .map_err(map_chat_err)?;
        Ok(serde_json::json!({
            "run_id": result.run_id,
            "message_id": result.user_message_id,
        }))
    }
}

/// フォーム値（object）を「キー: 値」の複数行テキストへ整形する（キー順で安定）。
fn format_form_text(params: &serde_json::Value) -> String {
    match params {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            keys.iter()
                .filter_map(|k| {
                    let v = &map[k.as_str()];
                    let rendered = match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => return None,
                        other => other.to_string(),
                    };
                    if rendered.trim().is_empty() {
                        None
                    } else {
                        Some(format!("{k}: {rendered}"))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        serde_json::Value::String(s) => s.trim().to_string(),
        _ => String::new(),
    }
}

fn map_chat_err(e: ChatError) -> ActionError {
    match e {
        ChatError::NotFound => ActionError::NotFound,
        ChatError::Forbidden => ActionError::Forbidden,
        ChatError::Invalid(m) => ActionError::Invalid(m),
        ChatError::Unavailable(m) => ActionError::Unavailable(m),
        ChatError::Internal(m) => ActionError::Internal(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_text_is_stable_and_skips_empty() {
        let text = format_form_text(&serde_json::json!({
            "b_rating": 5, "a_comment": "良い", "empty": "  ", "none": null
        }));
        assert_eq!(text, "a_comment: 良い\nb_rating: 5");
    }

    #[test]
    fn plain_string_params_pass_through() {
        assert_eq!(
            format_form_text(&serde_json::json!("こんにちは ")),
            "こんにちは"
        );
        assert_eq!(format_form_text(&serde_json::json!(42)), "");
    }
}
