//! 回答の本文の ```ui ブロック（`@imanect/yui`）のボタンを読む純関数（#604）。
//!
//! 図は本文の文字のまま保存され、ボタンの id は `{ブロックの識別子}:{a1}`（[`yui::action_key`]）。
//! サーバーは押されるたびに**保存済みの本文から**ブロックを読み直す（画面が送るのは id と値と
//! 冪等キーだけ）。ここはその読み取りと、会話に送る文・記録用の値の表示を組み立てる。
//! 実行（認可・台帳・起動）は [`crate::store::ChatStore::invoke_yui_action`]。

use serde_json::{Map, Value};
use uuid::Uuid;
use yui::{ActionBlock, ActionTarget, AppAction, Figure};

use crate::model::{ContentBlock, UiActionValue};

/// 1 メッセージで束縛を解決するワークフローのボタンの上限（解決の往復を抑える）。
pub const MAX_WORKFLOW_BUTTONS: usize = 50;

/// 回答の確定時に解決した、ワークフローを起動するボタンの束縛。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiWorkflowBinding {
    pub action_id: String,
    pub workflow_id: Uuid,
    pub version: i64,
    pub workflow_name: String,
}

/// 本文に操作を持つ図がありそうか（安い前判定。確かめるのは [`action_blocks`]）。
pub fn may_have_yui(content: &[ContentBlock]) -> bool {
    content.iter().any(|b| match b {
        ContentBlock::Text { text } => {
            let t = text.to_ascii_lowercase();
            t.contains("```ui")
                || t.contains("```yui")
                || t.contains("~~~ui")
                || t.contains("~~~yui")
        }
        _ => false,
    })
}

/// 本文の、操作を持つ図のブロック（トップレベルで閉じていて、エラーの無いもの・[`yui::extract_action_blocks`]）。
///
/// 本文の文字のブロックは 1 つずつ読む（画面も 1 つずつ Markdown として描くので、ブロックを
/// またいだフェンスは図にならない）。
pub fn action_blocks(content: &[ContentBlock]) -> Vec<ActionBlock> {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        })
        .flat_map(|t| yui::extract_action_blocks(t))
        .collect()
}

/// ワークフローを起動するボタン（id とワークフローの名前・重複は除く・上限まで）。
pub fn workflow_buttons(content: &[ContentBlock]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for block in action_blocks(content) {
        for action in yui::figure_actions(&block.figure) {
            let ActionTarget::Workflow { name } = &action.target else {
                continue;
            };
            let key = yui::action_key(&block.hash, &action.id);
            if out.iter().any(|(k, _)| *k == key) {
                continue;
            }
            if out.len() >= MAX_WORKFLOW_BUTTONS {
                return out;
            }
            out.push((key, name.clone()));
        }
    }
    out
}

/// id のボタンを探す（同じソースのブロックが 2 つあれば同じ id になり、先のものを返す。
/// 中身が同じなので、どちらを押しても同じ操作になる）。本文の文字のブロックは 1 つずつ読む。
pub fn find_action(content: &[ContentBlock], action_id: &str) -> Option<(ActionBlock, AppAction)> {
    yui::find_action(
        content.iter().filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        }),
        action_id,
    )
}

pub use yui::{error_text, valid_idempotency_key};

/// 送った値の表示（部品の表示名と表示用の値・記録と会話に送る文に使う）。
pub fn summary(
    figure: &Figure,
    action_id: &str,
    values: &Map<String, Value>,
) -> Vec<UiActionValue> {
    yui::summary(figure, action_id, values)
        .into_iter()
        .map(|v| UiActionValue {
            label: v.label,
            value: v.value,
        })
        .collect()
}

/// 会話に送るボタン（ask・plan・`-> chat`）の、利用者の発話として送る文。
///
/// 値はサーバーで検証した後のもので組み立てる（画面の組み立てた文は受け取らない）。
pub fn chat_text(figure: &Figure, action: &AppAction, values: &Map<String, Value>) -> String {
    let items = summary(figure, &action.id, values);
    match figure {
        Figure::App { base, sections } => {
            let section = sections
                .iter()
                .find(|s| s.actions.iter().any(|a| a.id == action.id));
            let title = section
                .and_then(|s| s.title.clone())
                .filter(|t| !t.is_empty())
                .or_else(|| Some(base.title.clone()).filter(|t| !t.is_empty()));
            let mut lines = vec![match title {
                Some(t) => format!("【{t}】{}", action.label),
                None => action.label.clone(),
            }];
            lines.extend(items.iter().map(|v| format!("{}: {}", v.label, v.value)));
            lines.join("\n")
        }
        Figure::Ask { .. } => items
            .iter()
            .map(|v| format!("{}\n→ {}", v.label, v.value))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Figure::Plan { .. } => match items.first() {
            Some(c) if action.id == "revise" => format!("計画を修正してください。\n{}", c.value),
            _ => "この計画で進めてください。".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(t: &str) -> ContentBlock {
        ContentBlock::Text { text: t.into() }
    }

    const APP: &str = "```ui\napp 経費\n## 申請\n金額 = number 必須 円 as amount\n区分 = choice 交通費 | 交際費 as category\nbutton 申請する -> workflow expense\nbutton 相談する -> chat\n```";

    #[test]
    fn finds_buttons_and_workflow_bindings() {
        let content = vec![
            text("前置き"),
            text(APP),
            ContentBlock::GenerativeUi { spec: json!({}) },
        ];
        assert!(may_have_yui(&content));
        let buttons = workflow_buttons(&content);
        assert_eq!(buttons.len(), 1);
        assert!(buttons[0].0.ends_with(":a1"));
        assert_eq!(buttons[0].1, "expense");
        let (block, action) = find_action(&content, &buttons[0].0).unwrap();
        assert_eq!(action.label, "申請する");
        assert!(matches!(block.figure, Figure::App { .. }));
        // 別のブロックの識別子・無い id・形の違う id は見つからない
        assert!(find_action(&content, "0000000000000000:a1").is_none());
        assert!(find_action(&content, &format!("{}:a9", block.hash)).is_none());
        assert!(find_action(&content, "a1").is_none());
        assert!(!may_have_yui(&[text("```rust\nfn main() {}\n```")]));
    }

    #[test]
    fn broken_figures_have_no_buttons() {
        // 項目もボタンも無い app はエラーになり、押せない
        let content = vec![text("```ui\napp 空\n```")];
        assert!(action_blocks(&content).is_empty());
    }

    #[test]
    fn chat_text_uses_labels_and_display_values() {
        let content = vec![text(APP)];
        let hash = &action_blocks(&content)[0].hash;
        let (block, action) = find_action(&content, &format!("{hash}:a2")).unwrap();
        let values = json!({ "amount": 12000, "category": "交通費" });
        let text = chat_text(&block.figure, &action, values.as_object().unwrap());
        assert_eq!(text, "【申請】相談する\n金額: 12,000円\n区分: 交通費");
    }

    #[test]
    fn ask_and_plan_texts() {
        let ask = vec![text(
            "```ui\nask 確認\n? 地域は\n- 関東\n- 関西\n? 予算 multi\n- 低\n- 高\n```",
        )];
        let (block, action) =
            find_action(&ask, &format!("{}:a1", action_blocks(&ask)[0].hash)).unwrap();
        let values = json!({ "q1": "関東", "q2": ["低", "高"] });
        assert_eq!(
            chat_text(&block.figure, &action, values.as_object().unwrap()),
            "地域は\n→ 関東\n\n予算\n→ 低、高"
        );
        let plan = vec![text("```ui\nplan 計画\n1. 調べる\n2. まとめる\n```")];
        let hash = action_blocks(&plan)[0].hash.clone();
        let (block, approve) = find_action(&plan, &format!("{hash}:approve")).unwrap();
        assert_eq!(
            chat_text(&block.figure, &approve, &Map::new()),
            "この計画で進めてください。"
        );
        let (_, revise) = find_action(&plan, &format!("{hash}:revise")).unwrap();
        let c = json!({ "comment": "3 を足して" });
        assert_eq!(
            chat_text(&block.figure, &revise, c.as_object().unwrap()),
            "計画を修正してください。\n3 を足して"
        );
    }

    #[test]
    fn errors_are_labelled() {
        let content = vec![text(APP)];
        let block = &action_blocks(&content)[0];
        let errors = json!({ "amount": "入力してください", "": "ボタンが見つかりません" });
        let t = error_text(&block.figure, errors.as_object().unwrap());
        assert!(t.contains("金額: 入力してください"));
        assert!(t.contains("ボタンが見つかりません"));
    }

    #[test]
    fn idempotency_keys() {
        assert!(valid_idempotency_key(
            "0b6c5f9e-3a8c-4a36-9d0e-5b0c7d7f1a2b"
        ));
        assert!(valid_idempotency_key("abcdEFGH_123"));
        assert!(!valid_idempotency_key("short"));
        assert!(!valid_idempotency_key("has space 12345"));
        assert!(!valid_idempotency_key(&"a".repeat(65)));
    }
}
