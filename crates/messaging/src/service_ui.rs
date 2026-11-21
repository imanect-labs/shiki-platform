//! エージェントの回答の ```ui（Yui）のボタンを押したときの発言を組む（design §4.14・#604 の職員間メッセージ版）。
//!
//! チャットと同じく、押されたボタンと値は**保存済みの回答から読み直して**検証し、送る文もサーバーが組む
//! （画面の組み立てた文は受け取らない）。押した結果は、押した人の発言としてそのスレッドに書かれ、
//! 押した人のエージェントが続けて答える（権限は押した人のまま・増えない）。
//!
//! - 押せるのは**回答したエージェントの持ち主だけ**（会話の続きは持ち主とそのエージェントのもの）。
//! - 押せるのは `-> chat` のボタンだけ。ワークフローを起動するボタンは拒否する（チャンネルの発言は
//!   ほかの参加者が書いたもので、回答に紛れ込んだボタンを押した人の権限で起動させない）。

use authz::AuthContext;
use chat::model::{ContentBlock, Mention};
use serde_json::{Map, Value};
use storage::db_tenant::begin_tenant_tx;
use uuid::Uuid;
use yui::ActionTarget;

use crate::store_message::{self as sm, MessageFilter};
use crate::{AuthorKind, Error, MessagingService, NewMessage};

impl MessagingService {
    /// 回答 `message` の図のボタン `action_key`（`{ブロックの識別子}:{a1}`）を `values` で押したときに、
    /// スレッドへ書く発言を組む。投稿とエージェントの呼び出しは呼び出し側（通常の投稿と同じ経路）が行う。
    ///
    /// 戻り値は（チャンネル, 発言）。発言は回答と同じスレッドへの返信で、自分のエージェントへのメンションを含む。
    ///
    /// # Errors
    ///
    /// 回答が無い・読めない（`NotFound`）、持ち主でない（`Forbidden`）、ボタンが無い（`NotFound`）、
    /// ワークフローのボタン・値が不正（`Invalid`）。
    pub async fn ui_action_reply(
        &self,
        ctx: &AuthContext,
        message: Uuid,
        action_key: &str,
        values: &Map<String, Value>,
    ) -> Result<(Uuid, NewMessage), Error> {
        let me = ctx.principal().id.clone();
        let mut tx = begin_tenant_tx(&self.pool, ctx.tenant()).await?;
        let ids = [message];
        // 呼び出した人が読める発言だけ（参加前の境界・権限は通常の読取と同じ関数で掛かる）。
        let row = sm::fetch_messages(&mut tx, ctx, &me, MessageFilter::Ids { ids: &ids })
            .await?
            .into_iter()
            .next()
            .ok_or(Error::NotFound)?;
        tx.commit().await?;
        if row.deleted_at.is_some() || row.expired_at.is_some() {
            return Err(Error::NotFound);
        }
        if row.author_kind != AuthorKind::Assistant.as_str() || row.author_id != me {
            return Err(Error::Forbidden(
                "自分のエージェントの回答のボタンだけ押せます",
            ));
        }
        let (block, action) =
            chat::yui_actions::find_action(&row.blocks.0, action_key).ok_or(Error::NotFound)?;
        if !matches!(action.target, ActionTarget::Chat) {
            return Err(Error::Invalid(
                "チャンネルではワークフローを起動できません（チャットで実行してください）".into(),
            ));
        }
        let check = yui::validate_action_values(&block.figure, &action.id, values, "ja");
        if !check.ok {
            return Err(Error::Invalid(chat::yui_actions::error_text(
                &block.figure,
                &check.errors,
            )));
        }
        let text = chat::yui_actions::chat_text(&block.figure, &action, &check.values);
        Ok((
            row.channel_id,
            NewMessage {
                parent_id: Some(row.parent_id.unwrap_or(row.id)),
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: format!(" {text}"),
                    },
                ],
                agent_private: false,
            },
        ))
    }
}
