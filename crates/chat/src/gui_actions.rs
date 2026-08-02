//! generative UI アクションの chat 側ハンドラ（Task 6.5 の②）。
//!
//! `chat.submit` はフォーム値を整形テキストとしてスレッドへ投稿する。認可は
//! [`ChatStore::post_message`]（editor 要求＋監査）の既存チョークポイントに委ねる
//! （このハンドラ自身は権限を持たない・昇格しない）。

use gui::{ActionError, ActionHandler, ActionLedger, ActionSource, HandlerKind};
use uuid::Uuid;

use crate::store::ChatStore;
use crate::ChatError;

/// 単発 UI アクションの実行台帳（#410・`ui_action_invocation`）。
///
/// 「押した」という事実をチャット側の一級の状態として持ち、二重送信の拒否とカードの
/// 「送信済み」表示の両方をここから引く。ミニアプリ由来（[`ActionSource::MiniApp`]）は
/// 何度でも実行できる UI なので対象外（そもそも単発束縛の `chat.submit` が使えない）。
pub struct ChatActionLedger {
    store: ChatStore,
}

impl ChatActionLedger {
    pub fn new(store: ChatStore) -> Self {
        ChatActionLedger { store }
    }

    /// チャット由来の発生源だけを取り出す（ミニアプリ由来は台帳を持たない）。
    fn chat_source(source: &ActionSource) -> Option<(Uuid, Uuid)> {
        match source {
            ActionSource::ChatMessage {
                thread_id,
                message_id,
            } => Some((*thread_id, *message_id)),
            ActionSource::MiniApp { .. } => None,
        }
    }
}

#[async_trait::async_trait]
impl ActionLedger for ChatActionLedger {
    async fn claim(
        &self,
        ctx: &authz::AuthContext,
        source: &ActionSource,
        action_id: &str,
    ) -> Result<bool, ActionError> {
        // 単発束縛（chat.submit）はチャット内 UI からしか意味を持たない。想定外の
        // 発生源で「確保できた」ことにせず、実行前に落とす（fail-closed）。
        let Some((thread_id, message_id)) = Self::chat_source(source) else {
            return Err(ActionError::Invalid(
                "この操作はチャット内 UI からのみ実行できます".into(),
            ));
        };
        self.store
            .claim_ui_action(ctx, thread_id, message_id, action_id)
            .await
            .map_err(map_chat_err)
    }

    async fn release(&self, ctx: &authz::AuthContext, source: &ActionSource, action_id: &str) {
        let Some((thread_id, message_id)) = Self::chat_source(source) else {
            return;
        };
        if let Err(e) = self
            .store
            .release_ui_action(ctx, thread_id, message_id, action_id)
            .await
        {
            // 解放できないと押し直せないままになるが、実行自体は失敗しているので
            // 会話は壊れない。人が追えるようにログだけ残す。
            tracing::warn!(error = %e, action_id, "UI アクションの確保解除に失敗");
        }
    }

    async fn attach_run(
        &self,
        ctx: &authz::AuthContext,
        source: &ActionSource,
        action_id: &str,
        run_id: Uuid,
    ) {
        let Some((thread_id, message_id)) = Self::chat_source(source) else {
            return;
        };
        if let Err(e) = self
            .store
            .attach_ui_action_run(ctx, thread_id, message_id, action_id, run_id)
            .await
        {
            tracing::warn!(error = %e, action_id, "UI アクション実行台帳への run 紐づけに失敗");
        }
    }
}

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
        // **カードを出した run の生成材料を継ぐ**（#387・#402）。質問カード/計画カードは
        // 自律 run の途中で出るため、ここで非自律に落とすと続きが max_steps=6・plan ツール無しの
        // 制約版になり「質問 → 計画 → 実行」が途中で別物になる。skill ピンも同じで、落とすと
        // スラッシュコマンド起動の手順書とフェーズ宣言が 2 ターン目から消える。
        // 引けない（run 消失等）ときは従来どおり非自律・ピン無しで投稿する
        // （能力を勝手に増やさない方向のフォールバック）。
        let (autonomous, skill_pins) = self
            .store
            .message_run_context(ctx, *thread_id, *message_id, trace_id)
            .await
            .map_err(map_chat_err)?
            .unwrap_or_default();
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
                // カードからの回答は「そのカードを出した run の続き」。run 単位 skill
                // （スラッシュコマンド起動）は thread ピンに無いので、ここで明示的に継ぐ。
                &skill_pins,
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
        // 利用枠超過も「待てば通る」＝一時障害として扱う（文言はそのまま出す）。
        ChatError::Unavailable(m) | ChatError::RateLimited(m) => ActionError::Unavailable(m),
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
