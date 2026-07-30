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
        let ActionSource::ChatMessage {
            thread_id,
            message_id,
        } = source
        else {
            return Err(ActionError::Invalid(
                "chat.submit はチャット内 UI からのみ実行できます".into(),
            ));
        };
        let text = format_form_text(&params);
        if text.is_empty() {
            return Err(ActionError::Invalid("フォーム値が空です".into()));
        }
        // **カードを出した run のモードを継ぐ**（#387）。質問カード/計画カードは自律 run の
        // 途中で出るため、ここで非自律に落とすと続きが max_steps=6・plan ツール無しの制約版に
        // なり「質問 → 計画 → 実行」が途中で別物になる。引けない（run 消失等）ときは
        // 従来どおり非自律で投稿する（能力を勝手に増やさない方向のフォールバック）。
        let autonomous = self
            .store
            .message_run_autonomous(*thread_id, *message_id, &ctx.tenant_id)
            .await
            .map_err(map_chat_err)?
            .unwrap_or(false);
        let result = self
            .store
            .post_message(
                ctx,
                *thread_id,
                &text,
                &[],
                None,
                None,
                autonomous,
                // カードからの回答は「そのカードを出した run の続き」であって新しい
                // コマンド起動ではない。run 単位 skill は thread ピン経由で継がれる。
                &[],
                trace_id,
            )
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
