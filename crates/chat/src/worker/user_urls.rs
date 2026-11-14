//! ユーザーがこの会話で書いた URL を拾う（`browser` の出所の初期値・docs/agent/browser.md §6.2）。

use crate::model::ContentBlock;

/// ユーザーがこの会話で書いた URL（`browser` が事前許可で開ける出所の初期値・docs/agent/browser.md §6.2）。
///
/// ユーザーが**自分で書いた**本文（`Text`）だけから拾う。選択範囲・添付・ツールの結果は出所にしない
/// （文書やページの中の URL を「ユーザーが書いた」扱いにすると、事前許可で開ける範囲が広がる）。
/// 上限があるので**新しい発話から**拾う（古い発話の URL が多くても、今書いた URL が漏れないように）。
pub(super) fn user_urls(msgs: &[crate::model::Message], skip: uuid::Uuid) -> Vec<url::Url> {
    agent_core::tools::browser::user_urls(
        msgs.iter()
            .rev()
            .filter(|m| m.role == crate::model::Role::User && m.id != skip)
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::user_urls;
    use crate::model::{ContentBlock, Message, Role};

    fn msg(role: Role, content: Vec<ContentBlock>) -> Message {
        Message {
            id: uuid::Uuid::new_v4(),
            role,
            content,
            agent_mode: true,
            parent_id: None,
            sibling_ids: Vec::new(),
            invoked_actions: Vec::new(),
            bound_actions: Vec::new(),
            ui_runs: Vec::new(),
            created_at: chrono::Utc::now(),
        }
    }

    fn text(t: &str) -> ContentBlock {
        ContentBlock::Text { text: t.into() }
    }

    /// 古い発話に URL が上限以上あっても、新しい発話の URL は漏れない（CodeRabbit 指摘）。
    #[test]
    fn newest_messages_win_when_the_cap_is_reached() {
        let many: String = (0..100)
            .map(|i| format!("https://old{i}.example/ "))
            .collect();
        let msgs = vec![
            msg(Role::User, vec![text(&many)]),
            msg(Role::User, vec![text("これも見て https://new.example/")]),
        ];
        let urls = user_urls(&msgs, uuid::Uuid::nil());
        assert_eq!(urls.len(), 64);
        assert_eq!(urls[0].as_str(), "https://new.example/");
    }

    /// 出所にするのはユーザーが書いた本文だけ（assistant の発話・生成対象・本文以外のブロックは拾わない）。
    #[test]
    fn user_urls_come_only_from_user_text() {
        let pending = msg(Role::User, vec![text("https://pending.example/")]);
        let msgs = vec![
            msg(
                Role::User,
                vec![
                    text("ここを見て https://a.example/x"),
                    ContentBlock::FileRef {
                        node_id: "https://file.example/".into(),
                        name: "https://name.example/".into(),
                    },
                ],
            ),
            msg(Role::Assistant, vec![text("https://assistant.example/")]),
            pending.clone(),
        ];
        let urls = user_urls(&msgs, pending.id);
        let got: Vec<&str> = urls.iter().map(url::Url::as_str).collect();
        assert_eq!(got, ["https://a.example/x"]);
    }
}
