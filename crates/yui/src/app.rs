//! app: 入力部品とボタンを並べた画面（`@imanect/yui` の kinds/app.ts の移植）。

use crate::ir::{ActionTarget, AppAction, AppField, AppSection, Base, ChoiceOption, FieldKind};
use crate::text::{
    cap, caps, clip, is_match, js_len, js_number, js_slice, nfkc, parse_option, re, replace_first,
    strip_list_marker, Diags, Line,
};
use crate::text::{
    APP_CHOICE_OPTIONS, APP_FIELDS, APP_LABEL, APP_SECTIONS, APP_SECTION_ACTIONS,
    APP_SECTION_FIELDS,
};
use serde_json::Value;

re!(FIELD, r"^([^=＝]+?)\s*[=＝]\s*(\S+)\s*(.*)$");
re!(
    BUTTON,
    r"(?i)^(?:button|ボタン)\s+(.+?)\s*(?:->|→|=>)\s*(.+)$"
);
re!(
    RANGE,
    r"^(-?[0-9]+(?:\.[0-9]+)?)\s*(?:\.\.|〜|~|～)\s*(-?[0-9]+(?:\.[0-9]+)?)\s*"
);
// 片側だけの範囲（`1..`・`..100`）。読まずに警告する（単位に紛れ込ませない）。
re!(
    HALF_RANGE,
    r"^(?:-?[0-9]+(?:\.[0-9]+)?\s*(?:\.\.|〜|~|～)|(?:\.\.|〜|~|～)\s*-?[0-9]+(?:\.[0-9]+)?)\s*"
);
re!(KEY, r"(^|\s)as\s+([A-Za-z_][A-Za-z0-9_.\-]{0,63})(\s|$)");
re!(REQUIRED, r"(?i)(^|\s)(?:必須|required)(\s|$)");
re!(MULTI, r"(?i)(^|\s)(?:multi|複数)(\s|$)");
re!(OTHER_OPT, r"(?i)(^|\s)(?:other|その他を受ける)(\s|$)");
re!(
    VALUED,
    r"(?i)(?:^|\s)(例|e\.g\.|placeholder|初期値?|initial|default|step|刻み)\s+"
);
re!(HEADING, r"^#{1,6}\s+(.+)$");
re!(QUOTE, r"^>\s?(.*)$");
re!(DATE_RANGE_WORD, r"(?i)^\s*range(\s|$)");
re!(CONFIRM, r"(?i)\s+(confirm|確認)$");
re!(CHAT, r"(?i)^(chat|会話|チャット)$");
re!(WORKFLOW, r"(?i)^(?:workflow|ワークフロー)\s*[:：]?\s*(.+)$");
re!(BAD_NAME, r#"[:/\\<>"'`{}]"#);
re!(OTHER_VALUE, r"(?i)^(その他|other)$");
re!(KV, r"^(.+?)\s*[=＝]\s*(.+)$");
re!(RATING_MAX, r"^([0-9]{1,2})(?-u:\b)");
re!(RANGE_SEP, r"\s*(?:〜|~|～)\s*");
re!(DATE, r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$");
re!(MONTH, r"^[0-9]{4}-[0-9]{2}$");
re!(
    DATE_PAIR,
    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}/[0-9]{4}-[0-9]{2}-[0-9]{2}$"
);
re!(ON, r"(?i)^(オン|on|1|true|はい)$");

/// 選択肢・初期値を区切る。直前が `\` の区切りは文字として残し、`\|` `\｜` は `|` `｜` に戻す
/// （TS 版の `(?<!\\)[|｜]` と `unescapeBar` と同じ）。
fn split_unescaped(s: &str, seps: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for ch in s.chars() {
        if seps.contains(&ch) && prev != Some('\\') {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
        prev = Some(ch);
    }
    out.push(cur);
    out.into_iter()
        .map(|p| p.trim().replace("\\|", "|").replace("\\｜", "｜"))
        .collect()
}

/// 入力の部品の種類 1 つ（パーサーと案内 [`crate::app_guide`] が読む単一の表）。
#[derive(Debug, Clone, Copy)]
pub struct FieldKindSpec {
    pub kind: FieldKind,
    /// 書ける語（先頭が代表の名前）。NFKC・小文字にした語と比べる。
    pub words: &'static [&'static str],
    /// オプションの書き方（案内に載せる）。
    pub syntax: &'static str,
    /// 1 行の例（`app` の図の中で診断 0 件になることをテストで確かめる）。
    pub example: &'static str,
}

/// 入力の部品の種類の表（Yui の app の部品・`@imanect/yui` の kinds/app.ts と同じ語）。
pub const FIELD_KINDS: [FieldKindSpec; 11] = [
    FieldKindSpec {
        kind: FieldKind::Text,
        words: &["text", "文字", "テキスト"],
        syntax: "1 行の文字。`例 …` で入力例、`初期 …` で初期値",
        example: "件名 = text 必須 例 4月 大阪出張の交通費 as title",
    },
    FieldKindSpec {
        kind: FieldKind::Textarea,
        words: &["textarea", "longtext", "文章"],
        syntax: "複数行の文章。`例 …`・`初期 …` は text と同じ",
        example: "内容 = textarea 例 経緯を書いてください as body",
    },
    FieldKindSpec {
        kind: FieldKind::Number,
        words: &["number", "数値"],
        syntax: "数値。`最小..最大` で範囲（両方を書く）、続けて単位、`step 刻み`、`初期 数`",
        example: "金額 = number 1..1000000 円 必須 as amount",
    },
    FieldKindSpec {
        kind: FieldKind::Slider,
        words: &["slider", "スライダー"],
        syntax: "つまみで選ぶ数値。`最小..最大` が必須、続けて単位、`step 刻み`、`初期 数`",
        example: "割合 = slider 0..100 % step 5 初期 50 as ratio",
    },
    FieldKindSpec {
        kind: FieldKind::Count,
        words: &["count", "stepper"],
        syntax: "＋・－で数える整数。`最小..最大` が必須、`初期 数`",
        example: "人数 = count 1..20 人 初期 1 as people",
    },
    FieldKindSpec {
        kind: FieldKind::Choice,
        words: &["choice", "select", "選択"],
        syntax: "`a | b | c` で選択肢（2 つ以上）。`表示名=値`、`multi` で複数選択（値は配列）、選択肢に `その他` を入れると自由記述も受ける、`初期 a`。選択肢の中の縦棒は `\\|`",
        example: "区分 = choice 交通費 | 出張費 | 備品 必須 as category",
    },
    FieldKindSpec {
        kind: FieldKind::Date,
        words: &["date", "日付"],
        syntax: "日付（YYYY-MM-DD）。`初期 2026-04-01`。`date range` と書くと期間",
        example: "日付 = date 必須 as day",
    },
    FieldKindSpec {
        kind: FieldKind::Month,
        words: &["month", "年月"],
        syntax: "年月（YYYY-MM）。`初期 2026-04`",
        example: "対象月 = month 必須 as month",
    },
    FieldKindSpec {
        kind: FieldKind::Daterange,
        words: &["daterange", "期間"],
        syntax: "期間（値は `開始/終了` の文字）。`初期 2026-04-01〜2026-04-30`",
        example: "期間 = daterange as period",
    },
    FieldKindSpec {
        kind: FieldKind::Rating,
        words: &["rating", "評価"],
        syntax: "星の評価（1〜最大の数値）。`rating 5` で最大（2〜10・既定 5）、`初期 数`",
        example: "満足度 = rating 5 as score",
    },
    FieldKindSpec {
        kind: FieldKind::Toggle,
        words: &["toggle", "switch", "checkbox", "オンオフ"],
        syntax: "オン・オフ（値は true / false）。`初期 オン`",
        example: "立替 = toggle 初期 オン as advance",
    },
];

fn field_kind(word: &str) -> Option<FieldKind> {
    let find = |s: &str| {
        FIELD_KINDS
            .iter()
            .find(|k| k.words.contains(&s))
            .map(|k| k.kind)
    };
    find(&nfkc(word).to_lowercase()).or_else(|| find(word))
}

/// 種類の代表の名前の一覧（`text / textarea / …`・誤りの文と案内で使う）。
pub(crate) fn kind_names(sep: &str) -> String {
    FIELD_KINDS
        .iter()
        .map(|k| k.words[0])
        .collect::<Vec<_>>()
        .join(sep)
}

fn label(text: &str, line: usize, diags: &mut Diags) -> String {
    let t = text.trim();
    if js_len(t) <= APP_LABEL {
        return t.to_string();
    }
    diags.add(
        line,
        "warning",
        "text_too_long",
        format!("ラベルは {APP_LABEL} 文字までです"),
    );
    format!("{}…", js_slice(t, APP_LABEL - 1))
}

#[allow(clippy::too_many_lines)]
pub(crate) fn parse_app(base: &mut Base, body: &[Line], diags: &mut Diags) -> Vec<AppSection> {
    let mut sections: Vec<AppSection> = Vec::new();
    // 今の機能（sections の添字）。None は「まだ無い」
    let mut current: Option<usize> = None;
    let mut field_count = 0;
    let mut action_count = 0;
    let mut skipping = false;
    for line in body {
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = parse_option(trimmed) {
            if k == "note" {
                base.note = Some(v);
            } else {
                base.src = Some(v);
            }
            continue;
        }
        if let Some(c) = caps(&HEADING, trimmed) {
            let title = cap(&c, 1);
            // 中身の無い（見出しだけの）機能は見出しを付け替える
            if let Some(prev) = current.and_then(|i| sections.get_mut(i)) {
                if prev.fields.is_empty()
                    && prev.actions.is_empty()
                    && prev.desc.as_deref().is_none_or(str::is_empty)
                {
                    prev.title = Some(label(&title, line.no, diags));
                    continue;
                }
            }
            if sections.len() >= APP_SECTIONS {
                diags.add_once(
                    line.no,
                    "warning",
                    "limit_exceeded",
                    format!("機能は {APP_SECTIONS} 個までです"),
                );
                skipping = true;
                continue;
            }
            skipping = false;
            sections.push(AppSection {
                title: Some(label(&title, line.no, diags)),
                desc: None,
                fields: Vec::new(),
                actions: Vec::new(),
            });
            current = Some(sections.len() - 1);
            continue;
        }
        if skipping {
            continue;
        }
        let idx = if let Some(i) = current {
            i
        } else {
            sections.push(AppSection {
                title: None,
                desc: None,
                fields: Vec::new(),
                actions: Vec::new(),
            });
            sections.len() - 1
        };
        if let Some(c) = caps(&QUOTE, trimmed) {
            current = Some(idx);
            let q = cap(&c, 1).trim().to_string();
            let s = &mut sections[idx];
            let joined = match &s.desc {
                Some(d) if !d.is_empty() => format!("{d}\n{q}"),
                _ => q,
            };
            s.desc = Some(clip(&joined, line.no, diags));
            continue;
        }
        let (raw, _) = strip_list_marker(trimmed);
        if let Some(c) = caps(&BUTTON, &raw) {
            current = Some(idx);
            if sections[idx].actions.len() >= APP_SECTION_ACTIONS {
                diags.add(
                    line.no,
                    "warning",
                    "limit_exceeded",
                    format!("1 つの機能のボタンは {APP_SECTION_ACTIONS} 個までです"),
                );
                continue;
            }
            action_count += 1;
            match read_action(&cap(&c, 1), &cap(&c, 2), action_count, line.no, diags) {
                Some(a) => sections[idx].actions.push(a),
                None => action_count -= 1,
            }
            continue;
        }
        if let Some(c) = caps(&FIELD, &raw) {
            current = Some(idx);
            if field_count >= APP_FIELDS || sections[idx].fields.len() >= APP_SECTION_FIELDS {
                diags.add_once(line.no, "warning", "limit_exceeded", format!("入力は 1 つの機能に {APP_SECTION_FIELDS} 個、全体で {APP_FIELDS} 個までです"));
                continue;
            }
            let Some(field) = read_field(
                nfkc(&cap(&c, 1)).trim(),
                &cap(&c, 2),
                &cap(&c, 3),
                line.no,
                diags,
            ) else {
                continue;
            };
            if sections[idx].fields.iter().any(|x| x.key == field.key) {
                diags.add(
                    line.no,
                    "warning",
                    "unrecognized_line",
                    format!("「{}」は同じ機能に既にあります", field.key),
                );
                continue;
            }
            sections[idx].fields.push(field);
            field_count += 1;
            continue;
        }
        current = Some(idx);
        diags.add(
            line.no,
            "warning",
            "unrecognized_line",
            format!("解釈できない行を無視しました: {trimmed}"),
        );
    }
    // 入力もボタンも無い機能は落とす
    sections.retain(|s| !s.fields.is_empty() || !s.actions.is_empty());
    sections
}

fn read_action(
    text: &str,
    target: &str,
    n: usize,
    line: usize,
    diags: &mut Diags,
) -> Option<AppAction> {
    let mut t = target.trim().to_string();
    let mut confirm = None;
    if let Some(m) = CONFIRM.as_ref().and_then(|r| r.find(&t)) {
        confirm = Some(true);
        t = t[..m.start()].trim().to_string();
    }
    let id = format!("a{n}");
    if is_match(&CHAT, &t) {
        return Some(AppAction {
            id,
            label: label(text, line, diags),
            target: ActionTarget::Chat,
            confirm: confirm.unwrap_or(false),
        });
    }
    let name = match caps(&WORKFLOW, &t) {
        Some(c) => cap(&c, 1),
        None => t.clone(),
    };
    let name = name.trim().to_string();
    // 名前だけを受ける（URL・パス・記号は実行先にしない。解決はホストが行う）
    if name.is_empty() || is_match(&BAD_NAME, &name) || js_len(&name) > 64 {
        diags.add(
            line,
            "warning",
            "unrecognized_line",
            format!("ボタンの実行先が読めません（「-> workflow 名前」か「-> chat」）: {target}"),
        );
        return None;
    }
    Some(AppAction {
        id,
        label: label(text, line, diags),
        target: ActionTarget::Workflow { name },
        confirm: true,
    })
}

/// オプションの文字列から、値を取るオプション（例・初期・step）を取り出し、残りを返す。
fn take_valued(s: &str) -> (String, Vec<(&'static str, String)>) {
    let Some(r) = VALUED.as_ref() else {
        return (s.to_string(), Vec::new());
    };
    let marks: Vec<(&'static str, usize, usize)> = r
        .captures_iter(s)
        .filter_map(|c| {
            let whole = c.get(0)?;
            let word = c.get(1)?.as_str().to_lowercase();
            let key = match word.as_str() {
                "例" | "e.g." | "placeholder" => "例",
                "step" | "刻み" => "step",
                _ => "初期",
            };
            Some((key, whole.start(), whole.end()))
        })
        .collect();
    if marks.is_empty() {
        return (s.to_string(), Vec::new());
    }
    let mut values: Vec<(&'static str, String)> = Vec::new();
    for (i, (key, _, start)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map_or(s.len(), |m| m.1);
        let v = s[*start..end].trim().to_string();
        values.retain(|(k, _)| k != key);
        values.push((key, v));
    }
    (s[..marks[0].1].to_string(), values)
}

fn get<'a>(values: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
    values
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.as_str())
}

#[allow(clippy::too_many_lines)]
fn read_field(
    name: &str,
    type_word: &str,
    opts: &str,
    line: usize,
    diags: &mut Diags,
) -> Option<AppField> {
    let Some(mut kind) = field_kind(type_word) else {
        diags.add(
            line,
            "warning",
            "unrecognized_line",
            format!(
                "入力の種類が分かりません: {type_word}（{}）",
                kind_names(" / ")
            ),
        );
        return None;
    };
    let mut opts = opts.to_string();
    // `date range` は期間
    if kind == FieldKind::Date && is_match(&DATE_RANGE_WORD, &opts) {
        kind = FieldKind::Daterange;
        let lower = opts.trim_start().to_string();
        opts = lower.get(5..).unwrap_or("").to_string();
    }
    let mut s = format!(" {opts} ");
    let key = caps(&KEY, &s).map(|c| cap(&c, 2));
    if key.is_some() {
        s = replace_first(&KEY, &s, |c| format!(" {}", cap(c, 3)));
    }
    let required = is_match(&REQUIRED, &s);
    s = replace_first(&REQUIRED, &s, |c| format!(" {}", cap(c, 2)));
    let multi = is_match(&MULTI, &s);
    s = replace_first(&MULTI, &s, |c| format!(" {}", cap(c, 2)));
    let other = is_match(&OTHER_OPT, &s);
    s = replace_first(&OTHER_OPT, &s, |c| format!(" {}", cap(c, 2)));
    let lab = label(name, line, diags);
    let mut field = AppField {
        key: key.unwrap_or_else(|| lab.clone()),
        label: lab,
        kind,
        required,
        placeholder: None,
        initial: None,
        min: None,
        max: None,
        step: None,
        unit: None,
        options: None,
        multi: None,
        other: None,
    };

    if kind == FieldKind::Choice {
        // 選択肢は `a | b | c`（`表示名=値` も可）。例・初期は後ろに書く
        let (rest, values) = take_valued(&s);
        let parts = split_unescaped(&rest, &['|', '｜']);
        let mut options: Vec<ChoiceOption> = parts
            .into_iter()
            .filter(|o| !o.is_empty())
            .map(|o| match caps(&KV, &o) {
                Some(c) => ChoiceOption {
                    label: label(&cap(&c, 1), line, diags),
                    value: cap(&c, 2).trim().to_string(),
                },
                None => ChoiceOption {
                    label: label(&o, line, diags),
                    value: o.clone(),
                },
            })
            .collect();
        // 選択肢に「その他」と書いたものは、自由記述を受ける印にする
        let other_idx = options
            .iter()
            .position(|o| is_match(&OTHER_VALUE, &o.value));
        if let Some(i) = other_idx {
            options.remove(i);
        }
        if options.len() < 2 {
            diags.add(
                line,
                "warning",
                "unrecognized_line",
                format!("選択肢は「a | b」のように 2 つ以上書いてください: {name}"),
            );
            return None;
        }
        options.truncate(APP_CHOICE_OPTIONS);
        if let Some(init) = get(&values, "初期").filter(|v| !v.is_empty()) {
            let picked: Vec<String> = split_unescaped(init, &[',', '、', '|'])
                .into_iter()
                .filter_map(|v| {
                    options
                        .iter()
                        .find(|o| o.value == v || o.label == v)
                        .map(|o| o.value.clone())
                })
                .collect();
            if let Some(first) = picked.first() {
                field.initial = Some(if multi {
                    Value::from(picked.clone())
                } else {
                    Value::from(first.clone())
                });
            }
        }
        field.options = Some(options);
        if multi {
            field.multi = Some(true);
        }
        if other || other_idx.is_some() {
            field.other = Some(true);
        }
        return Some(field);
    }

    let (rest, values) = take_valued(&s);
    if let Some(p) = get(&values, "例").filter(|v| !v.is_empty()) {
        field.placeholder = Some(clip(p, line, diags));
    }
    let mut tail = rest.trim().to_string();
    match kind {
        FieldKind::Number | FieldKind::Slider | FieldKind::Count => {
            if let Some(c) = caps(&RANGE, &tail) {
                let (a, b) = (js_number(&cap(&c, 1)), js_number(&cap(&c, 2)));
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                field.min = Some(lo);
                field.max = Some(hi);
                let used = c.get(0).map_or(0, |m| m.end());
                tail = tail[used..].trim().to_string();
            } else if kind == FieldKind::Number && is_match(&HALF_RANGE, &tail) {
                diags.add(
                    line,
                    "warning",
                    "unrecognized_line",
                    format!("範囲は「最小..最大」の両方を書いてください（片側だけの範囲は読みません）: {name}"),
                );
                tail = replace_first(&HALF_RANGE, &tail, |_| String::new())
                    .trim()
                    .to_string();
            } else if kind != FieldKind::Number {
                let word = if kind == FieldKind::Slider {
                    "slider"
                } else {
                    "count"
                };
                diags.add(
                    line,
                    "warning",
                    "unrecognized_line",
                    format!("{word} には範囲（最小..最大）が要ります: {name}"),
                );
                return None;
            }
            if let Some(st) = get(&values, "step").filter(|v| !v.is_empty()) {
                let n = js_number(st);
                if n.is_finite() {
                    field.step = Some(if n.abs() == 0.0 { 1.0 } else { n.abs() });
                }
            }
            if kind == FieldKind::Count {
                field.step = Some(1.0);
            }
            if let Some(init) = get(&values, "初期") {
                let n = js_number(init);
                if n.is_finite() {
                    field.initial = Some(crate::text::json_num(n));
                }
            }
            if !tail.is_empty() {
                field.unit = Some(label(&tail, line, diags));
            }
        }
        FieldKind::Rating => {
            // `rating 5`（最大。既定 5、2〜10）
            let max =
                caps(&RATING_MAX, &tail).map_or(5.0, |c| js_number(&cap(&c, 1)).clamp(2.0, 10.0));
            field.max = Some(max);
            field.min = Some(1.0);
            if let Some(init) = get(&values, "初期")
                .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
            {
                field.initial = Some(crate::text::json_num(js_number(init).clamp(1.0, max)));
            }
        }
        FieldKind::Daterange => {
            if let Some(init) = get(&values, "初期") {
                let v = init.replace('/', "-");
                let v = replace_first(&RANGE_SEP, &v, |_| "/".to_string());
                if is_match(&DATE_PAIR, &v) {
                    field.initial = Some(Value::from(v));
                }
            }
        }
        FieldKind::Toggle => {
            if let Some(init) = get(&values, "初期").filter(|v| !v.is_empty()) {
                field.initial = Some(Value::from(is_match(&ON, init)));
            }
        }
        FieldKind::Date | FieldKind::Month => {
            if let Some(init) = get(&values, "初期").filter(|v| !v.is_empty()) {
                let v = init.replace('/', "-");
                let ok = if kind == FieldKind::Date {
                    is_match(&DATE, &v)
                } else {
                    is_match(&MONTH, &v)
                };
                if ok {
                    field.initial = Some(Value::from(v));
                }
            }
        }
        FieldKind::Text | FieldKind::Textarea | FieldKind::Choice => {
            // オプション以外の残りは例として扱う（`件名 = text 4月の交通費` のような書き方）
            if let Some(init) = get(&values, "初期").filter(|v| !v.is_empty()) {
                field.initial = Some(Value::from(clip(init, line, diags)));
            }
            if field.placeholder.is_none() && !tail.is_empty() {
                field.placeholder = Some(clip(&tail, line, diags));
            }
        }
    }
    Some(field)
}

#[cfg(test)]
mod pattern_tests {
    use super::*;

    /// 組めないパターンは「一致しない」扱いになり黙って壊れるので、すべて組めることを確かめる。
    #[test]
    fn patterns_compile() {
        for r in [
            &FIELD,
            &BUTTON,
            &RANGE,
            &HALF_RANGE,
            &KEY,
            &REQUIRED,
            &MULTI,
            &OTHER_OPT,
            &VALUED,
            &HEADING,
            &QUOTE,
            &DATE_RANGE_WORD,
            &CONFIRM,
            &CHAT,
            &WORKFLOW,
            &BAD_NAME,
            &OTHER_VALUE,
            &KV,
            &RATING_MAX,
            &RANGE_SEP,
            &DATE,
            &MONTH,
            &DATE_PAIR,
            &ON,
        ] {
            assert!(r.is_some());
        }
    }
}
