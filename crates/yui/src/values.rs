//! ボタンと、ボタンが送る値の検証（`@imanect/yui` の app/values.ts の移植。文言も TS 版と同じ）。

use crate::ir::{ActionTarget, AppAction, AppField, AskQuestion, FieldKind, Figure};
use crate::text::{js_len, js_num_str, js_number, json_num, nfkc, re, uniq};
use serde::Serialize;
use serde_json::{Map, Value};

/// 1 行の文字・複数行の文字・選択の「その他」・自由記述の上限（UTF-16 の単位）。
const TEXT_MAX: usize = 1000;
const TEXTAREA_MAX: usize = 10_000;
const OTHER_MAX: usize = 200;
const FREE_MAX: usize = 2000;

/// 検証の結果。`values` は実行先に渡す値（そのボタンの部品だけ。空の任意項目は含めない）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub ok: bool,
    pub values: Map<String, Value>,
    pub errors: Map<String, Value>,
}

/// 図のボタン（ホストが束縛を作る対象）。ask・plan は会話に送るボタンだけ。
pub fn figure_actions(f: &Figure) -> Vec<AppAction> {
    match f {
        Figure::App { sections, .. } => sections
            .iter()
            .flat_map(|s| s.actions.iter().cloned())
            .collect(),
        Figure::Ask {
            questions,
            submit_label,
            ..
        } => {
            if questions.is_empty() {
                Vec::new()
            } else {
                vec![AppAction {
                    id: "a1".into(),
                    label: submit_label.clone().unwrap_or_else(|| "回答する".into()),
                    target: ActionTarget::Chat,
                    confirm: false,
                }]
            }
        }
        Figure::Plan {
            revise,
            approve_label,
            ..
        } => {
            let mut v = vec![AppAction {
                id: "approve".into(),
                label: approve_label
                    .clone()
                    .unwrap_or_else(|| "この計画で進める".into()),
                target: ActionTarget::Chat,
                confirm: false,
            }];
            if *revise {
                v.push(AppAction {
                    id: "revise".into(),
                    label: "修正を依頼".into(),
                    target: ActionTarget::Chat,
                    confirm: false,
                });
            }
            v
        }
    }
}

fn is_empty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    }
}

re!(DATE, r"^([0-9]{4})-([0-9]{2})-([0-9]{2})$");
re!(MONTH, r"^([0-9]{4})-([0-9]{2})$");
re!(
    RANGE,
    r"^([0-9]{4}-[0-9]{2}-[0-9]{2})/([0-9]{4}-[0-9]{2}-[0-9]{2})$"
);

fn valid_date(text: &str, month: bool) -> bool {
    let found = if month {
        crate::text::caps(&MONTH, text)
    } else {
        crate::text::caps(&DATE, text)
    };
    let Some(found) = found else { return false };
    let num = |i: usize| found.get(i).and_then(|g| g.as_str().parse::<u32>().ok());
    let (Some(year), Some(mon)) = (num(1), num(2)) else {
        return false;
    };
    let day = if month { 1 } else { num(3).unwrap_or(0) };
    if !(1..=12).contains(&mon) || day == 0 {
        return false;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match mon {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    day <= days
}

/// JavaScript の `Number(raw)`（文字・数値・真偽）。
fn to_number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => js_number(s),
        Value::Bool(b) => f64::from(u8::from(*b)),
        _ => f64::NAN,
    }
}

fn err(ja: bool, jp: &str, en: &str) -> Result<Option<Value>, String> {
    Err(if ja { jp.to_string() } else { en.to_string() })
}

/// 1 つの部品の値を検証する。正しければ正規化した値（空の任意項目は `None`）、誤りなら文言。
pub(crate) fn check_field(
    field: &AppField,
    raw: Option<&Value>,
    ja: bool,
) -> Result<Option<Value>, String> {
    if is_empty(raw) {
        return if field.required {
            err(ja, "入力してください", "Required")
        } else {
            Ok(None)
        };
    }
    let Some(raw) = raw else { return Ok(None) };
    match field.kind {
        FieldKind::Text | FieldKind::Textarea => {
            let Value::String(s) = raw else {
                return err(ja, "文字で入力してください", "Must be text");
            };
            let max = if field.kind == FieldKind::Text {
                TEXT_MAX
            } else {
                TEXTAREA_MAX
            };
            if js_len(s) > max {
                return Err(if ja {
                    format!("{max} 文字までです")
                } else {
                    format!("Up to {max} characters")
                });
            }
            if field.kind == FieldKind::Text && (s.contains('\r') || s.contains('\n')) {
                return err(ja, "改行は入れられません", "No line breaks");
            }
            Ok(Some(raw.clone()))
        }
        FieldKind::Number | FieldKind::Slider | FieldKind::Count => {
            let n = match raw {
                Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                Value::String(s) if !s.trim().is_empty() => js_number(&nfkc(s).replace(',', "")),
                _ => f64::NAN,
            };
            if !n.is_finite() {
                return err(ja, "数値で入力してください", "Must be a number");
            }
            if field.kind == FieldKind::Count && n.fract() != 0.0 {
                return err(ja, "整数で入力してください", "Must be a whole number");
            }
            if let Some(min) = field.min.filter(|m| n < *m) {
                return Err(if ja {
                    format!("{} 以上にしてください", js_num_str(min))
                } else {
                    format!("Must be at least {}", js_num_str(min))
                });
            }
            if let Some(max) = field.max.filter(|m| n > *m) {
                return Err(if ja {
                    format!("{} 以下にしてください", js_num_str(max))
                } else {
                    format!("Must be at most {}", js_num_str(max))
                });
            }
            Ok(Some(json_num(n)))
        }
        FieldKind::Choice => check_choice(field, raw, ja),
        FieldKind::Rating => {
            let max = field.max.unwrap_or(5.0);
            let n = to_number(raw);
            if n.fract() != 0.0 || !n.is_finite() || n < 1.0 || n > max {
                return Err(if ja {
                    format!("1〜{} で選んでください", js_num_str(max))
                } else {
                    format!("Pick 1 to {}", js_num_str(max))
                });
            }
            Ok(Some(json_num(n)))
        }
        FieldKind::Daterange => {
            let Value::String(s) = raw else {
                return err(ja, "期間が正しくありません", "Invalid date range");
            };
            let Some(c) = crate::text::caps(&RANGE, s) else {
                return err(ja, "期間が正しくありません", "Invalid date range");
            };
            let (a, b) = (crate::text::cap(&c, 1), crate::text::cap(&c, 2));
            if !valid_date(&a, false) || !valid_date(&b, false) {
                return err(ja, "期間が正しくありません", "Invalid date range");
            }
            if a > b {
                return err(
                    ja,
                    "終わりの日を始まりの日以降にしてください",
                    "End must be on or after start",
                );
            }
            Ok(Some(raw.clone()))
        }
        FieldKind::Date | FieldKind::Month => match raw {
            Value::String(s) if valid_date(s, field.kind == FieldKind::Month) => {
                Ok(Some(raw.clone()))
            }
            _ => err(ja, "日付が正しくありません", "Invalid date"),
        },
        FieldKind::Toggle => match raw {
            Value::Bool(_) => Ok(Some(raw.clone())),
            _ => err(ja, "オンかオフにしてください", "Must be on or off"),
        },
    }
}

fn check_choice(field: &AppField, raw: &Value, ja: bool) -> Result<Option<Value>, String> {
    let allowed: Vec<&str> = field
        .options
        .iter()
        .flatten()
        .map(|o| o.value.as_str())
        .collect();
    let other = field.other == Some(true);
    // 「その他」を受けるなら、選択肢に無い文（200 文字まで）を 1 つまで受ける
    let free = |v: &str| other && !v.trim().is_empty() && js_len(v) <= OTHER_MAX;
    let pick = "選択肢から選んでください";
    if field.multi == Some(true) {
        let Value::Array(items) = raw else {
            return err(ja, pick, "Pick from the options");
        };
        let mut strs = Vec::new();
        for v in items {
            match v {
                Value::String(s) if allowed.contains(&s.as_str()) || free(s) => {
                    strs.push(s.clone());
                }
                _ => return err(ja, pick, "Pick from the options"),
            }
        }
        if strs
            .iter()
            .filter(|s| !allowed.contains(&s.as_str()))
            .count()
            > 1
        {
            return err(ja, "その他は 1 つまでです", "Only one other answer");
        }
        let norm: Vec<String> = strs
            .iter()
            .map(|s| {
                if allowed.contains(&s.as_str()) {
                    s.clone()
                } else {
                    s.trim().to_string()
                }
            })
            .collect();
        return Ok(Some(Value::from(uniq(&norm))));
    }
    match raw {
        Value::String(s) if allowed.contains(&s.as_str()) => Ok(Some(raw.clone())),
        Value::String(s) if free(s) => Ok(Some(Value::from(s.trim()))),
        _ => err(ja, pick, "Pick from the options"),
    }
}

/// ask の回答の検証。選択肢のある質問は選択肢から（その他を受けるなら自由記述も）、無い質問は自由記述。
pub(crate) fn check_answer(
    q: &AskQuestion,
    raw: Option<&Value>,
    ja: bool,
) -> Result<Option<Value>, String> {
    if is_empty(raw) {
        return if q.required {
            err(ja, "回答してください", "Required")
        } else {
            Ok(None)
        };
    }
    let Some(raw) = raw else { return Ok(None) };
    let labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
    let ok_text = |v: &Value| matches!(v, Value::String(s) if !s.trim().is_empty() && js_len(s) <= FREE_MAX && (labels.contains(&s.as_str()) || q.other || q.options.is_empty()));
    if q.options.is_empty() {
        return match raw {
            Value::String(s) if ok_text(raw) => Ok(Some(Value::from(s.trim()))),
            _ => Err(if ja {
                format!("{FREE_MAX} 文字までの文字で回答してください")
            } else {
                "Invalid answer".into()
            }),
        };
    }
    if q.multi {
        let Value::Array(items) = raw else {
            return err(ja, "選択肢から選んでください", "Pick from the options");
        };
        if !items.iter().all(ok_text) {
            return err(ja, "選択肢から選んでください", "Pick from the options");
        }
        let strs: Vec<String> = items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if strs
            .iter()
            .filter(|s| !labels.contains(&s.as_str()))
            .count()
            > 1
        {
            return err(ja, "その他は 1 つまでです", "Only one other answer");
        }
        let trimmed: Vec<String> = strs.iter().map(|s| s.trim().to_string()).collect();
        return Ok(Some(Value::from(uniq(&trimmed))));
    }
    match raw {
        Value::String(s) if ok_text(raw) => Ok(Some(Value::from(s.trim()))),
        _ => err(ja, "選択肢から選んでください", "Pick from the options"),
    }
}

/// 図のボタンが送る値を検証する（app・ask・plan）。
/// - app: そのボタンの機能の部品だけを見て、他の key は捨てる
/// - ask: 質問ごとの回答（key は q1・q2…）
/// - plan: approve は値なし、revise は comment（必須・2000 文字まで）
pub fn validate_action_values(
    f: &Figure,
    action_id: &str,
    input: &Map<String, Value>,
    locale: &str,
) -> Check {
    let ja = locale.to_lowercase().starts_with("ja");
    let mut values = Map::new();
    let mut errors = Map::new();
    if !figure_actions(f).iter().any(|a| a.id == action_id) {
        errors.insert(
            String::new(),
            Value::from(if ja {
                "ボタンが見つかりません"
            } else {
                "Unknown action"
            }),
        );
        return Check {
            ok: false,
            values,
            errors,
        };
    }
    match f {
        Figure::App { sections, .. } => {
            let Some(section) = sections
                .iter()
                .find(|s| s.actions.iter().any(|a| a.id == action_id))
            else {
                return Check {
                    ok: false,
                    values,
                    errors,
                };
            };
            for field in &section.fields {
                match check_field(field, input.get(&field.key), ja) {
                    Ok(Some(v)) => {
                        values.insert(field.key.clone(), v);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        errors.insert(field.key.clone(), Value::from(e));
                    }
                }
            }
        }
        Figure::Ask { questions, .. } => {
            for q in questions {
                match check_answer(q, input.get(&q.id), ja) {
                    Ok(Some(v)) => {
                        values.insert(q.id.clone(), v);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        errors.insert(q.id.clone(), Value::from(e));
                    }
                }
            }
        }
        Figure::Plan { .. } => {
            if action_id == "revise" {
                match input.get("comment") {
                    Some(Value::String(c)) if !c.trim().is_empty() && js_len(c) <= FREE_MAX => {
                        values.insert("comment".into(), Value::from(c.trim()));
                    }
                    Some(Value::String(c)) if !c.trim().is_empty() => {
                        errors.insert(
                            "comment".into(),
                            Value::from(if ja {
                                format!("{FREE_MAX} 文字までです")
                            } else {
                                format!("Up to {FREE_MAX} characters")
                            }),
                        );
                    }
                    _ => {
                        errors.insert(
                            "comment".into(),
                            Value::from(if ja {
                                "修正してほしい点を書いてください"
                            } else {
                                "Describe the change"
                            }),
                        );
                    }
                }
            }
        }
    }
    Check {
        ok: errors.is_empty(),
        values,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leap_years_and_month_lengths() {
        assert!(valid_date("2024-02-29", false));
        assert!(!valid_date("2025-02-29", false));
        assert!(!valid_date("2025-04-31", false));
        assert!(valid_date("2025-12", true));
        assert!(!valid_date("2025-13", true));
    }

    #[test]
    fn patterns_compile() {
        for r in [&DATE, &MONTH, &RANGE] {
            assert!(r.is_some());
        }
        assert!(crate::text::is_match(&DATE, "2025-01-01"));
    }
}
