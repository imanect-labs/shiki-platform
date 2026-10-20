//! ボタンの読み取りと、送った値の表示・誤りの文（会話とミニアプリで共有・#604）。
//!
//! どちらも保存済みの Markdown（会話は回答の本文、ミニアプリは画面）からボタンを読み直し、
//! 画面から届いた値を [`crate::validate_action_values`] で検証してから実行する。ここはその
//! 読み取りと、記録・誤りの表示に使う文を組み立てる純関数。

use serde_json::{Map, Value};

use crate::{display_value, extract_action_blocks, figure_actions, ActionBlock, AppAction, Figure};

/// 送った値の 1 項目（部品の表示名と表示用の値）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownValue {
    pub label: String,
    pub value: String,
}

/// Markdown の本文から id（`{ブロックの識別子}:{a1}`）のボタンを探す。
///
/// 同じソースのブロックが 2 つあれば同じ id になり、先のものを返す（中身が同じなので、
/// どちらを押しても同じ操作になる）。
pub fn find_action<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    action_key: &str,
) -> Option<(ActionBlock, AppAction)> {
    let (hash, id) = action_key.split_once(':')?;
    texts
        .into_iter()
        .flat_map(extract_action_blocks)
        .filter(|b| b.hash == hash)
        .find_map(|b| {
            let action = figure_actions(&b.figure).into_iter().find(|a| a.id == id)?;
            Some((b, action))
        })
}

/// 冪等キーの形（英数字・`-`・`_` の 8〜64 文字。UUID がそのまま通る）。
pub fn valid_idempotency_key(key: &str) -> bool {
    (8..=64).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn answer_text(v: &Value) -> String {
    match v {
        Value::Array(items) => items
            .iter()
            .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
            .collect::<Vec<_>>()
            .join("、"),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 送った値の表示（部品の表示名と表示用の値・記録と会話に送る文に使う）。
pub fn summary(figure: &Figure, action_id: &str, values: &Map<String, Value>) -> Vec<ShownValue> {
    match figure {
        Figure::App { sections, .. } => sections
            .iter()
            .find(|s| s.actions.iter().any(|a| a.id == action_id))
            .map(|s| {
                s.fields
                    .iter()
                    .filter_map(|f| {
                        let v = values.get(&f.key)?;
                        Some(ShownValue {
                            label: f.label.clone(),
                            value: display_value(f, Some(v), true),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Figure::Ask { questions, .. } => questions
            .iter()
            .filter_map(|q| {
                Some(ShownValue {
                    label: q.text.clone(),
                    value: answer_text(values.get(&q.id)?),
                })
            })
            .collect(),
        Figure::Plan { .. } => values
            .get("comment")
            .map(|c| ShownValue {
                label: "修正の内容".into(),
                value: answer_text(c),
            })
            .into_iter()
            .collect(),
    }
}

/// 値の検証の誤りを、部品の表示名つきの文にする（400 の本文）。
pub fn error_text(figure: &Figure, errors: &Map<String, Value>) -> String {
    let label = |key: &str| -> Option<String> {
        if key.is_empty() {
            return None;
        }
        match figure {
            Figure::App { sections, .. } => sections
                .iter()
                .flat_map(|s| s.fields.iter())
                .find(|f| f.key == key)
                .map(|f| f.label.clone()),
            Figure::Ask { questions, .. } => questions
                .iter()
                .find(|q| q.id == key)
                .map(|q| q.text.clone()),
            Figure::Plan { .. } => Some("修正の内容".into()),
        }
    };
    errors
        .iter()
        .map(|(k, v)| {
            let msg = v.as_str().unwrap_or("値が正しくありません");
            match label(k) {
                Some(l) => format!("{l}: {msg}"),
                None => msg.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const APP: &str = "```ui\napp 経費\n## 申請\n金額 = number 必須 円 as amount\n区分 = choice 交通費 | 交際費 as category\nbutton 申請する -> workflow expense\n```";

    #[test]
    fn finds_the_button_across_texts() {
        let (block, action) = find_action(["前置き", APP], &first_key()).unwrap();
        assert_eq!(action.label, "申請する");
        assert!(matches!(block.figure, Figure::App { .. }));
        assert!(find_action([APP], "0000000000000000:a1").is_none());
        assert!(find_action([APP], &format!("{}:a9", block.hash)).is_none());
        assert!(find_action([APP], "a1").is_none());
    }

    fn first_key() -> String {
        let block = extract_action_blocks(APP).remove(0);
        crate::action_key(&block.hash, "a1")
    }

    #[test]
    fn summary_and_errors_use_labels() {
        let (block, _) = find_action([APP], &first_key()).unwrap();
        let values = json!({ "amount": 1200, "category": "交通費" });
        let shown = summary(&block.figure, "a1", values.as_object().unwrap());
        assert_eq!(shown[0].label, "金額");
        assert!(shown[0].value.contains("1,200"));
        assert_eq!(shown[1].value, "交通費");
        let errors = json!({ "amount": "必須です", "": "ボタンが見つかりません" });
        let text = error_text(&block.figure, errors.as_object().unwrap());
        assert!(text.contains("金額: 必須です"));
        assert!(text.contains("ボタンが見つかりません"));
    }

    #[test]
    fn idempotency_keys() {
        assert!(valid_idempotency_key(
            "0b9c1f2e-3a4d-4e5f-8a9b-0c1d2e3f4a5b"
        ));
        assert!(!valid_idempotency_key("short"));
        assert!(!valid_idempotency_key("has space in it"));
    }
}
