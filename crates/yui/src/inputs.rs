//! 画面の部品と、ボタンの実行先のワークフローの入力の突き合わせ（`@imanect/yui` の editor/check.ts の
//! `checkInputs` の移植。文言も TS 版と同じ）。
//!
//! ホストが入力のスキーマから手がかり（[`InputHint`]）を作って渡す。手がかりの無い（読めない）ワークフローは
//! 突き合わせない（入力が無いのではなく、分からないだけ）。結果はどれも注意（保存は止めない）。

use crate::ir::{ActionTarget, AppSection, FieldKind, Figure};

/// ワークフローの入力 1 つの手がかり（TS 版の `WorkflowInputHint` のうち、突き合わせに使うもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputHint {
    pub key: String,
    pub label: Option<String>,
    /// 受ける部品の種類（分からなければ `None`・種類は照らさない）。
    pub kind: Option<FieldKind>,
    pub required: bool,
}

/// ボタンの実行先に選べるワークフロー（TS 版の `WorkflowChoice`）。`inputs` が `None` は読めないワークフロー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowInputs {
    pub name: String,
    pub inputs: Option<Vec<InputHint>>,
}

/// 突き合わせの注意 1 件（場所は機能・部品・ボタンの番号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputProblem {
    /// TS 版と同じコード（`missing_input`・`input_type`・`input_required`・`unknown_input`）。
    pub code: &'static str,
    pub message: String,
    pub section: usize,
    pub field: Option<usize>,
    pub action: Option<usize>,
}

/// app の図の機能ごとに、ボタンの実行先の入力と部品を突き合わせる（app でない図は空）。
#[must_use]
pub fn check_inputs(f: &Figure, workflows: &[WorkflowInputs]) -> Vec<InputProblem> {
    let Figure::App { sections, .. } = f else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if workflows.is_empty() {
        return out;
    }
    for (si, s) in sections.iter().enumerate() {
        check_section(s, si, workflows, &mut out);
    }
    out
}

fn check_section(
    s: &AppSection,
    si: usize,
    workflows: &[WorkflowInputs],
    out: &mut Vec<InputProblem>,
) {
    // 実行先の名前（ボタンの順・重なりなし）。
    let mut names: Vec<&str> = Vec::new();
    for a in &s.actions {
        if let ActionTarget::Workflow { name } = &a.target {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
    }
    for name in names {
        let Some(inputs) = workflows
            .iter()
            .find(|w| w.name == name)
            .and_then(|w| w.inputs.as_ref())
        else {
            continue;
        };
        let ai = s
            .actions
            .iter()
            .position(|a| matches!(&a.target, ActionTarget::Workflow { name: n } if n == name));
        for inp in inputs {
            let Some(fi) = s.fields.iter().position(|x| x.key == inp.key) else {
                if inp.required {
                    out.push(InputProblem {
                        code: "missing_input",
                        message: format!(
                            "「{name}」の必須の入力「{}」（{}）に当たる部品がありません",
                            inp.label.as_deref().unwrap_or(&inp.key),
                            inp.key
                        ),
                        section: si,
                        field: None,
                        action: ai,
                    });
                }
                continue;
            };
            let x = &s.fields[fi];
            if let Some(want) = inp.kind {
                if group(want) != group(x.kind) {
                    out.push(InputProblem {
                        code: "input_type",
                        message: format!(
                            "「{name}」の「{}」は{}を受けます（この部品は{}）",
                            inp.key,
                            type_name(want),
                            type_name(x.kind)
                        ),
                        section: si,
                        field: Some(fi),
                        action: None,
                    });
                }
            }
            if inp.required && !x.required {
                out.push(InputProblem {
                    code: "input_required",
                    message: format!("「{name}」では必須の入力です。「必須」にしてください"),
                    section: si,
                    field: Some(fi),
                    action: None,
                });
            }
        }
        for (fi, x) in s.fields.iter().enumerate() {
            if !inputs.iter().any(|i| i.key == x.key) {
                out.push(InputProblem {
                    code: "unknown_input",
                    message: format!(
                        "「{name}」に「{}」という入力はありません（送っても使われないか、断られます）",
                        x.key
                    ),
                    section: si,
                    field: Some(fi),
                    action: None,
                });
            }
        }
    }
}

/// 種類のまとまり（数を受ける部品どうし・文字を受ける部品どうしは同じとみなす）。
fn group(k: FieldKind) -> FieldKind {
    match k {
        FieldKind::Number | FieldKind::Slider | FieldKind::Count | FieldKind::Rating => {
            FieldKind::Number
        }
        FieldKind::Text | FieldKind::Textarea => FieldKind::Text,
        other => other,
    }
}

fn type_name(k: FieldKind) -> &'static str {
    match k {
        FieldKind::Text => "文字",
        FieldKind::Textarea => "文章",
        FieldKind::Number | FieldKind::Slider => "数値",
        FieldKind::Count => "数",
        FieldKind::Choice => "選択",
        FieldKind::Date => "日付",
        FieldKind::Daterange => "期間",
        FieldKind::Month => "年月",
        FieldKind::Toggle => "オン・オフ",
        FieldKind::Rating => "評価",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_block, Kind};

    fn app(source: &str) -> Figure {
        parse_block(source, Some(Kind::App)).figure.unwrap()
    }

    fn hint(key: &str, kind: Option<FieldKind>, required: bool) -> InputHint {
        InputHint {
            key: key.into(),
            label: Some(format!("{key}の見出し")),
            kind,
            required,
        }
    }

    fn found(f: &Figure, workflows: &[WorkflowInputs]) -> Vec<(&'static str, String)> {
        check_inputs(f, workflows)
            .into_iter()
            .map(|p| (p.code, p.message))
            .collect()
    }

    /// TS 版と同じ 4 つの注意（必須の入力が無い・種類が違う・必須でない・無い入力）を同じ文言で返す。
    #[test]
    fn inputs_are_matched_against_the_fields_like_the_ts_checker() {
        let f = app("app 申請\n金額 = text as amount\n区分 = choice 交通費 | 交際費 as kind\nメモ = textarea as memo\nbutton 申請する -> workflow expense\n");
        let wf = |inputs| {
            vec![WorkflowInputs {
                name: "expense".into(),
                inputs,
            }]
        };
        let inputs = vec![
            hint("amount", Some(FieldKind::Number), true),
            hint("kind", Some(FieldKind::Choice), false),
            hint("title", Some(FieldKind::Text), true),
            hint("memo", Some(FieldKind::Text), false),
        ];
        assert_eq!(
            found(&f, &wf(Some(inputs))),
            vec![
                (
                    "input_type",
                    "「expense」の「amount」は数値を受けます（この部品は文字）".to_string()
                ),
                (
                    "input_required",
                    "「expense」では必須の入力です。「必須」にしてください".to_string()
                ),
                (
                    "missing_input",
                    "「expense」の必須の入力「titleの見出し」（title）に当たる部品がありません"
                        .to_string()
                ),
            ]
        );
        // 読めないワークフロー（手がかりが無い）は言わない。束ねていないワークフローも言わない。
        assert!(found(&f, &wf(None)).is_empty());
        assert!(found(
            &f,
            &[WorkflowInputs {
                name: "other".into(),
                inputs: Some(vec![hint("x", None, true)])
            }]
        )
        .is_empty());
        // 無い入力。
        assert_eq!(
            found(&f, &wf(Some(vec![hint("amount", None, false)]))),
            vec![
                ("unknown_input", "「expense」に「kind」という入力はありません（送っても使われないか、断られます）".to_string()),
                ("unknown_input", "「expense」に「memo」という入力はありません（送っても使われないか、断られます）".to_string()),
            ]
        );
    }
}
