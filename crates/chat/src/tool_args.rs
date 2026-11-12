//! 道具の object の引数の受け口（GAP-119・GAP-130・GAP-136・D-179・D-186・D-193）。
//!
//! モデルは object の引数を JSON の文字列にして送ることがある。どの道具でも同じ規則で受ける（写しを作らない）:
//! - object はそのまま。省いた・null は道具の扱い（必須か省略できるか）に任せる。
//! - 文字列は JSON として **1 段だけ**読み、object になれば object で来たものとして扱う。入力の中で置き換えるので、
//!   後の検証と上限は object で来たときと同じものを通る（受ける形を広げても権限や検証は変わらない）。
//! - 読めない文字列・object にならない文字列（配列・数・文字列の JSON）・object でない値は、直し方を言って断る。
//!   同じ道具が SQL の文を別の引数で受けるとき（data_query の `sql`）、SQL の文が来たらそちらに書くよう言う。
//! - 読めない文字列は、読めなくなった所の前後を印を入れて見せ、壊れ方の種類（括弧の数の食い違いが多い・途中で
//!   終わっている・文字列の中の生の改行などの制御文字・JSON に無いエスケープ）を言う。モデルは長い 1 行の中で serde の
//!   列の番号を数えられないため（GAP-136）。生の制御文字は、正しくエスケープした `\n` と見分けられる記号（⏎ など）で
//!   見せる。壊れた所を推測で直して受けることはしない（D-193）。前後に出すのはモデルが今書いた道具の引数で、他人の
//!   文章ではないので封筒には包まない。
//!
//! input_schema で `type: object` の引数を持つ道具は、すべてここの [`ObjectArg`] を通す（道具ごとの定数で分かる）。

use std::fmt::Write as _;

use serde_json::error::Category;
use serde_json::Value;

/// 断る文に添える、読めなくなった所の前の字数（`chars` で数える）。
const BEFORE: usize = 60;
/// 断る文に添える、読めなくなった所の後の字数（`chars` で数える）。
const AFTER: usize = 20;
/// 読めなくなった所の印。JSON に出てこない記号なので、モデルが本文と取り違えない。
const MARK: &str = "⟪ここ⟫";

/// serde_json の位置（行は 1 から。列は行の始めからの UTF-8 のバイトで 1 から数え、読めなくなったバイトを指す）を、
/// 文字列のバイトの位置に写す。行が無ければ終わり、長さを超えれば長さに丸め、字の途中なら字の始めまで戻す。
fn byte_at(raw: &str, line: usize, column: usize) -> usize {
    let start = match line.checked_sub(2) {
        None => 0,
        Some(newlines) => raw
            .match_indices('\n')
            .nth(newlines)
            .map_or(raw.len(), |(at, _)| at + 1),
    };
    let mut at = start
        .saturating_add(column.saturating_sub(1))
        .min(raw.len());
    while !raw.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// 断る文の中で見えるようにする。改行・復帰・タブは `\n`・`\r`・`\t` の 2 文字、ほかの制御文字は `\u{XXXX}` にする
/// （断る文に制御文字を残さない）。引用符はそのまま。
fn visible(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:04X}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// 読めなくなった所のバイトの位置（途中で終わっている〔Eof〕ときは文字列の終わり）。
fn error_at(raw: &str, e: &serde_json::Error) -> usize {
    if e.is_eof() {
        raw.len()
    } else {
        byte_at(raw, e.line(), e.column())
    }
}

/// 読めなくなった所の前 [`BEFORE`] 字と後 [`AFTER`] 字を、間に [`MARK`] を入れて切り出す。切ったほうにだけ `…` を
/// 付ける。途中で終わっている（Eof）ときは文字列の終わりに印を入れる。
fn broken_at(raw: &str, e: &serde_json::Error) -> String {
    excerpt(raw, error_at(raw, e), None)
}

/// `at`（バイトの位置・字の境界）の前後を、間に [`MARK`] を入れて切り出す。`first` を渡すと、印の直後の 1 字を
/// その記号で見せる（文字列の中の生の制御文字を、エスケープの `\n` と見分けられるようにする）。
fn excerpt(raw: &str, at: usize, first: Option<&str>) -> String {
    let (head, tail) = raw.split_at(at);
    let from = head
        .char_indices()
        .rev()
        .nth(BEFORE - 1)
        .map_or(0, |(i, _)| i);
    let to = tail
        .char_indices()
        .nth(AFTER)
        .map_or(tail.len(), |(i, _)| i);
    let shown_tail = match first {
        Some(symbol) => {
            let mut rest = tail[..to].chars();
            rest.next();
            format!("{symbol}{}", visible(rest.as_str()))
        }
        None => visible(&tail[..to]),
    };
    format!(
        "{}{}{MARK}{}{}",
        if from > 0 { "…" } else { "" },
        visible(&head[from..]),
        shown_tail,
        if to < tail.len() { "…" } else { "" }
    )
}

/// 文字列の中で読めなくなった字（括弧の食い違いではない壊れ方）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BadInString {
    /// 生の制御文字（改行・タブなど）。`at` はその字のバイトの位置。
    Control { at: usize, c: char },
    /// JSON に無いエスケープ（`\` の後ろが `" \ / b f n r t u` でない・`\u` の後ろが 16 進 4 桁でない）。`at` は
    /// その `\` のバイトの位置。
    Escape { at: usize },
}

/// serde の誤りが文字列の中の生の制御文字か、JSON に無いエスケープなら、その字を返す。
///
/// serde_json は誤りの種類を公開していないので、文の頭（`control character`・`invalid escape`）で見分ける（文が
/// 変われば単体テストが落ちる）。位置は字を読んだ後のもの: 生の改行は次の行の 0 列（字の後ろ）、ほかの制御文字は
/// その字の列（字の前）、エスケープは `\` の後ろの字か 16 進の 4 桁目を指す。
fn bad_in_string(raw: &str, e: &serde_json::Error) -> Option<BadInString> {
    let text = e.to_string();
    let at = error_at(raw, e);
    if text.starts_with("control character") {
        let before = raw[..at]
            .chars()
            .next_back()
            .map(|c| (at - c.len_utf8(), c));
        let here = raw[at..].chars().next().map(|c| (at, c));
        let (at, c) = if e.column() == 0 {
            before.or(here)
        } else {
            here.or(before)
        }
        .filter(|(_, c)| c.is_control())?;
        return Some(BadInString::Control { at, c });
    }
    if text.starts_with("invalid escape") {
        // `\uXXXX` の 4 桁目まで戻れば、壊れたエスケープの `\` に届く。
        let from = at.saturating_sub(6);
        let back = raw[..at]
            .char_indices()
            .rev()
            .take_while(|(i, _)| *i >= from)
            .find(|(_, c)| *c == '\\')
            .map_or(at, |(i, _)| i);
        return Some(BadInString::Escape { at: back });
    }
    None
}

/// 生の制御文字の見せ方（エスケープの `\n` と見分けられる記号）と名前。
fn control_symbol(c: char) -> (String, &'static str) {
    match c {
        '\n' => ("⏎".into(), "改行"),
        '\r' => ("␍".into(), "復帰（CR）"),
        '\t' => ("⇥".into(), "タブ"),
        c => (visible(&c.to_string()), "制御文字"),
    }
}

/// 文字列の中の壊れ方の説明（括弧の数の食い違いとは言わない）。
fn in_string(raw: &str, bad: BadInString) -> String {
    match bad {
        BadInString::Control { at, c } => {
            let (symbol, name) = control_symbol(c);
            format!(
                "読めなくなったのは {MARK} の所です（直後の {symbol} が生の{name}）: {}。文字列の中に生の{name}が\
                 あります。JSON の文字列の中では、改行は \\n、タブは \\t と書きます（JSON をさらに文字列にして渡す\
                 なら、その \\ もエスケープして \\\\n と書きます）。",
                excerpt(raw, at, Some(&symbol))
            )
        }
        BadInString::Escape { at } => format!(
            "読めなくなったのは {MARK} の所です: {}。文字列の中の \\ の後ろが JSON のエスケープ\
             （\\\" \\\\ \\/ \\b \\f \\n \\r \\t \\uXXXX）ではありません。\\ そのものは \\\\ と書きます。",
            excerpt(raw, at, None)
        ),
    }
}

/// 読めない文字列の壊れ方の説明（断る文の serde の位置の後、直し方の前に入れる）。
fn how_broken(raw: &str, e: &serde_json::Error) -> String {
    if let Some(bad) = bad_in_string(raw, e) {
        return in_string(raw, bad);
    }
    match e.classify() {
        Category::Syntax => format!(
            "読めなくなったのは {MARK} の所です: {}。多いのは括弧の数の食い違いで、閉じ括弧（}} か ]）が 1 つ多いか、\
             足りません。この所までの {{ と }}、[ と ] の数を数えて直してください。",
            broken_at(raw, e)
        ),
        Category::Eof => format!(
            "文字列が途中で終わっています（閉じ括弧が足りないか、途中で切れています）。終わりの所: {}。",
            broken_at(raw, e)
        ),
        // 位置の無い誤り（行が 0）には前後を添えない。
        Category::Data | Category::Io if e.line() == 0 => String::new(),
        Category::Data | Category::Io => {
            format!("読めなくなったのは {MARK} の所です: {}。", broken_at(raw, e))
        }
    }
}

/// object の引数（名前と、断る文に添える中身の説明）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ObjectArg {
    /// 引数の名前。
    pub key: &'static str,
    /// 何を持つオブジェクトか（断る文の括弧の中に入れる）。
    pub shape: &'static str,
    /// 同じ道具で SQL の文を受ける引数の名前（data_query の `sql`）。この引数に SQL の文（SELECT・WITH で始まる、
    /// JSON として読めない文字列）が来たら、JSON にしたことではなく、書く引数の取り違えを言う。
    pub sql_arg: Option<&'static str>,
}

/// SQL の文に見えるか（前の空白を除いた最初の語が SELECT・WITH・大文字小文字は問わない）。
fn looks_like_sql(raw: &str) -> bool {
    raw.split_whitespace().next().is_some_and(|word| {
        word.eq_ignore_ascii_case("select") || word.eq_ignore_ascii_case("with")
    })
}

impl ObjectArg {
    /// object で渡すよう言う文（読めない文字列のときは前に読めなかった位置と壊れ方を足す）。
    pub(crate) fn not_object(&self) -> String {
        format!(
            "{} は JSON の文字列にせず、オブジェクトのまま渡してください（{}）。",
            self.key, self.shape
        )
    }

    /// 値を object として読む。object はそのまま（`None`）、JSON として読むと object になる文字列は読んだもの
    /// （`Some`）。ほかは断る文を返す。
    pub(crate) fn read(&self, value: &Value) -> Result<Option<Value>, String> {
        match value {
            Value::Object(_) => Ok(None),
            Value::String(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(parsed @ Value::Object(_)) => Ok(Some(parsed)),
                Ok(_) => Err(self.not_object()),
                Err(_) if self.sql_arg.is_some() && looks_like_sql(raw) => Err(format!(
                    "{key} に SQL の文が入っています。SQL は {key} ではなく {sql} に書いてください（{key} は {shape}）。",
                    key = self.key,
                    sql = self.sql_arg.unwrap_or_default(),
                    shape = self.shape
                )),
                Err(e) => Err(format!(
                    "{} の文字列を JSON として読めませんでした（{e}）。{}{}",
                    self.key,
                    how_broken(raw, &e),
                    self.not_object()
                )),
            },
            _ => Err(self.not_object()),
        }
    }
}

/// data_query の `query`。
pub(crate) const DATA_QUERY: ObjectArg = ObjectArg {
    key: "query",
    shape: "filter・sort・select・limit などを持つオブジェクト",
    sql_arg: Some("sql"),
};

/// data_record_write の `data`。
pub(crate) const DATA_RECORD: ObjectArg = ObjectArg {
    key: "data",
    shape: "列の参照名 → 値",
    sql_arg: None,
};

/// save_skill の `knowledge_scope` と `model`。
pub(crate) const SKILL_DRAFT: [ObjectArg; 2] = [
    ObjectArg {
        key: "knowledge_scope",
        shape: "folders・files を持つオブジェクト",
        sql_arg: None,
    },
    ObjectArg {
        key: "model",
        shape: "model・temperature・max_tokens を持つオブジェクト",
        sql_arg: None,
    },
];

/// 入力の object の引数のうち、JSON の文字列で来たものを読んだ object に置き換える（省いた・null は触らない）。
///
/// 断るときは最初に当たった引数の文を返す（呼ぶ側が [`agent_core::ToolError::Invalid`] にする）。
pub(crate) fn accept_object_args(input: &mut Value, args: &[ObjectArg]) -> Result<(), String> {
    let Some(map) = input.as_object_mut() else {
        return Ok(());
    };
    for arg in args {
        let Some(slot) = map.get_mut(arg.key) else {
            continue;
        };
        if slot.is_null() {
            continue;
        }
        if let Some(parsed) = arg.read(slot)? {
            *slot = parsed;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ARG: ObjectArg = ObjectArg {
        key: "data",
        shape: "列の参照名 → 値",
        sql_arg: None,
    };

    /// object・JSON の文字列（1 行・整形）は同じ object になり、省いた・null は触らない。
    #[test]
    fn objects_and_json_strings_of_one_are_the_same() {
        let data = json!({ "title": "交通費", "amount": 1200 });
        for given in [
            data.clone(),
            json!(data.to_string()),
            json!(serde_json::to_string_pretty(&data).unwrap()),
        ] {
            let mut input = json!({ "table": "経費", "data": given });
            accept_object_args(&mut input, &[ARG]).unwrap();
            assert_eq!(input, json!({ "table": "経費", "data": data }));
        }
        for untouched in [json!({ "table": "経費" }), json!({ "data": null })] {
            let mut input = untouched.clone();
            accept_object_args(&mut input, &[ARG]).unwrap();
            assert_eq!(input, untouched);
        }
    }

    /// 壊れた文字列は読めなくなった位置と直し方、配列・数・文字列の JSON の文字列と object でない値は直し方を言う。
    /// 文字列の JSON の文字列（2 段）は読まない。
    #[test]
    fn other_values_are_refused_with_how_to_fix() {
        let refused = |value: Value| {
            let mut input = json!({ "data": value });
            accept_object_args(&mut input, &[ARG]).unwrap_err()
        };
        let broken = refused(json!(r#"{"title": "交通費", "amount": }"#));
        assert!(
            broken.starts_with("data の文字列を JSON として読めませんでした（")
                && broken.contains("line 1 column")
                && broken.ends_with(&ARG.not_object()),
            "{broken}"
        );
        assert_eq!(
            ARG.not_object(),
            "data は JSON の文字列にせず、オブジェクトのまま渡してください（列の参照名 → 値）。"
        );
        let object = json!({ "title": "交通費" });
        for bad in [
            json!(format!("[{object}]")),
            json!(json!(object.to_string()).to_string()),
            json!("12"),
            json!([object.clone()]),
            json!(12),
            json!(true),
        ] {
            assert_eq!(refused(bad.clone()), ARG.not_object(), "{bad}");
        }
    }

    /// data_query の query に SQL の文を書いた（sql と取り違えた）ときは、JSON にしたことではなく sql に書くよう言う。
    /// SQL に見えない壊れた文字列は今までどおり。SQL を受ける引数の無い道具では言わない。
    #[test]
    fn sql_in_the_query_points_at_the_sql_arg() {
        let refused = |arg: ObjectArg, value: &str| {
            let mut input = json!({ arg.key: value });
            accept_object_args(&mut input, &[arg]).unwrap_err()
        };
        let expected =
            "query に SQL の文が入っています。SQL は query ではなく sql に書いてください\
             （query は filter・sort・select・limit などを持つオブジェクト）。";
        for sql in [
            "SELECT title FROM 経費",
            "  select title from 経費",
            "WITH a AS (SELECT 1) SELECT * FROM a",
        ] {
            assert_eq!(refused(DATA_QUERY, sql), expected, "{sql}");
        }
        let broken = refused(DATA_QUERY, r#"{"limit": }"#);
        assert!(
            broken.starts_with("query の文字列を JSON として読めませんでした（")
                && broken.ends_with(&DATA_QUERY.not_object()),
            "{broken}"
        );
        assert!(
            refused(ARG, "SELECT 1").starts_with("data の文字列を JSON として読めませんでした（")
        );
    }

    /// 入力が object でなければ何もしない（道具の型の検証に任せる）。複数の引数は順に見て、最初の誤りを返す。
    #[test]
    fn several_args_are_read_in_order() {
        let mut not_object = json!("x");
        accept_object_args(&mut not_object, &[ARG]).unwrap();
        assert_eq!(not_object, json!("x"));
        let other = ObjectArg {
            key: "model",
            shape: "model・temperature・max_tokens",
            sql_arg: None,
        };
        let mut input = json!({ "data": "{\"a\":1}", "model": "{\"model\":\"fast\"}" });
        accept_object_args(&mut input, &[ARG, other]).unwrap();
        assert_eq!(
            input,
            json!({ "data": { "a": 1 }, "model": { "model": "fast" } })
        );
        let mut bad = json!({ "data": "[1]", "model": "x" });
        assert!(accept_object_args(&mut bad, &[ARG, other])
            .unwrap_err()
            .starts_with("data は"));
    }

    /// 断る文（key は ir）。
    fn refused_ir(raw: &str) -> String {
        const IR: ObjectArg = ObjectArg {
            key: "ir",
            shape: "ir_version・name・triggers・nodes・edges を持つオブジェクト",
            sql_arg: None,
        };
        let mut input = json!({ "ir": raw });
        let msg = accept_object_args(&mut input, &[IR]).unwrap_err();
        assert!(
            msg.starts_with("ir の文字列を JSON として読めませんでした（")
                && msg.ends_with(&IR.not_object()),
            "{msg}"
        );
        msg
    }

    /// ノードの並びの終わりの } が 1 つ多い 1 行の IR（E1 の vg の Mode B の app12-faq-table と同じ形・GAP-136）は、
    /// 読めなくなった所の前 60 字と後 20 字を印を入れて見せ、括弧の数を数えるよう言う。
    #[test]
    fn an_extra_closing_brace_is_shown_with_its_surroundings() {
        let raw = r#"{"ir_version":1,"name":"faq-auto-answer","display_name":"社内の質問に答える","triggers":[{"kind":"interactive"}],"nodes":[{"id":"record_auto","type":"data.record.create","label":"問い合わせ記録（自動回答）","params":{"table":"問い合わせ","data":{"question":{"$from":"input","path":"/question"},"status":"自動回答"}}}},{"id":"reply_auto","type":"workflow.reply","label":"回答を表示","params":{"format":"text","value":"受け付けました"}}],"edges":[]}"#;
        assert_eq!(
            refused_ir(raw),
            "ir の文字列を JSON として読めませんでした（expected `,` or `]` at line 1 column 348）。\
             読めなくなったのは ⟪ここ⟫ の所です: \
             …ion\":{\"$from\":\"input\",\"path\":\"/question\"},\"status\":\"自動回答\"}}}⟪ここ⟫},{\"id\":\"reply_auto\"…。\
             多いのは括弧の数の食い違いで、閉じ括弧（} か ]）が 1 つ多いか、足りません。\
             この所までの { と }、[ と ] の数を数えて直してください。\
             ir は JSON の文字列にせず、オブジェクトのまま渡してください\
             （ir_version・name・triggers・nodes・edges を持つオブジェクト）。"
        );
        // ほかの道具の引数（data_query の query）も同じ規則で言う。
        let mut input = json!({ "query": r#"{"limit": }"# });
        let query = accept_object_args(&mut input, &[DATA_QUERY]).unwrap_err();
        assert!(
            query.contains("読めなくなったのは ⟪ここ⟫ の所です: {\"limit\": ⟪ここ⟫}。")
                && query.contains("括弧の数の食い違い"),
            "{query}"
        );
    }

    /// 2,000 字を超える 1 行の文字列でも、前後は字の境界で切る（多バイトの字の途中で切らない）。
    #[test]
    fn a_long_line_is_cut_at_char_boundaries() {
        let raw = format!(
            r#"{{"nodes":[{{"id":"{}"}}}},{{"id":"{}"}}]}}"#,
            "あ".repeat(1000),
            "い".repeat(1000)
        );
        assert!(raw.chars().count() > 2000);
        let expected = format!(
            "…{}\"}}⟪ここ⟫}},{{\"id\":\"{}…",
            "あ".repeat(58),
            "い".repeat(11)
        );
        let msg = refused_ir(&raw);
        assert!(msg.contains(&format!("の所です: {expected}。")), "{msg}");
    }

    /// 整形した複数行の IR でも、3 行目より後の位置を正しく写す。前後の改行は `\n` として見せる。
    #[test]
    fn a_pretty_ir_maps_lines_and_shows_newlines() {
        let raw = "{\n  \"name\": \"wf\",\n  \"nodes\": [\n    {\n      \"id\": \"say\",\n      \
                   \"type\": \"llm.invoke\"\n    }\n    },\n    {\n      \"id\": \"end\"\n    }\n  ]\n}";
        let msg = refused_ir(raw);
        assert!(
            msg.contains("at line 8 column 5）")
                && msg.contains(
                    "の所です: …  {\\n      \"id\": \"say\",\\n      \"type\": \"llm.invoke\"\\n    }\\n    \
                     ⟪ここ⟫},\\n    {\\n      \"id\":…。"
                ),
            "{msg}"
        );
    }

    /// 途中で切れた文字列は、途中で終わっていると言い、終わりに印を入れて見せる。
    #[test]
    fn a_truncated_string_says_it_ends_midway() {
        let msg = refused_ir(r#"{"ir_version": 1, "name": "wf", "nodes": ["#);
        assert!(
            msg.contains(
                "。文字列が途中で終わっています（閉じ括弧が足りないか、途中で切れています）。\
                 終わりの所: {\"ir_version\": 1, \"name\": \"wf\", \"nodes\": [⟪ここ⟫。ir は"
            ),
            "{msg}"
        );
        let long = format!(r#"{{"prompt": "{}"#, "あ".repeat(100));
        let msg = refused_ir(&long);
        assert!(
            msg.contains(&format!("終わりの所: …{}⟪ここ⟫。", "あ".repeat(60))),
            "{msg}"
        );
    }

    /// 生のタブ・改行・制御文字は見える並びにして、断る文に制御文字を 1 つも残さない。
    #[test]
    fn control_characters_are_made_visible() {
        let raw = "{\"a\":\t1,\r\n\"b\":\u{1}\"x\u{7f}\u{85}\"}";
        let msg = refused_ir(raw);
        assert!(
            msg.contains(
                "の所です: {\"a\":\\t1,\\r\\n\"b\":⟪ここ⟫\\u{0001}\"x\\u{007F}\\u{0085}\"}。"
            ),
            "{msg}"
        );
        assert!(!msg.chars().any(char::is_control), "{msg:?}");
        // 文字列の中の生のタブは、エスケープの \t と見分けられる記号で見せる（後ろの制御文字は今までどおり）。
        let in_string = refused_ir("{\"a\": \"x\ty\u{0}z\"}");
        assert!(
            in_string.contains(
                "の所です（直後の ⇥ が生のタブ）: {\"a\": \"x⟪ここ⟫⇥y\\u{0000}z\"}。文字列の中に生のタブがあります。"
            ) && !in_string.chars().any(char::is_control),
            "{in_string:?}"
        );
    }

    /// 文字列の中の生の改行（IR を文字列で渡すときに外側の \n を 1 段しかエスケープしなかった）は、括弧の数の食い違いと
    /// 言わず、生の改行と言う。印は改行の前に入れ、改行は正しいエスケープの \n と見分けられる ⏎ で見せる。
    #[test]
    fn a_raw_newline_in_a_string_is_not_called_a_bracket_mismatch() {
        let raw = "{\"nodes\":[{\"params\":{\"prompt\":\"1行目\\n2行目\n3行目\"}}]}";
        let msg = refused_ir(raw);
        assert!(msg.contains("（control character"), "{msg}");
        assert!(
            msg.contains(
                "読めなくなったのは ⟪ここ⟫ の所です（直後の ⏎ が生の改行）: \
                 {\"nodes\":[{\"params\":{\"prompt\":\"1行目\\n2行目⟪ここ⟫⏎3行目\"}}]}。\
                 文字列の中に生の改行があります。JSON の文字列の中では、改行は \\n、タブは \\t と書きます\
                 （JSON をさらに文字列にして渡すなら、その \\ もエスケープして \\\\n と書きます）。"
            ),
            "{msg}"
        );
        assert!(!msg.contains("括弧の数"), "{msg}");
        // 生の復帰（CR）とほかの制御文字も、その字の前に印を入れる。
        let cr = refused_ir("{\"a\":\"x\ry\"}");
        assert!(
            cr.contains("\"x⟪ここ⟫␍y\"}。文字列の中に生の復帰（CR）があります。"),
            "{cr}"
        );
        let bell = refused_ir("{\"a\":\"x\u{7}y\"}");
        assert!(
            bell.contains("（直後の \\u{0007} が生の制御文字）: {\"a\":\"x⟪ここ⟫\\u{0007}y\"}。"),
            "{bell}"
        );
    }

    /// JSON に無いエスケープ（\q・16 進でない \u）は、括弧の数の食い違いと言わず、エスケープの書き方を言う。印は壊れた
    /// エスケープの \ の前に入れる。
    #[test]
    fn an_invalid_escape_names_the_escapes() {
        let msg = refused_ir(r#"{"a":"C:\q","b":1}"#);
        assert!(
            msg.contains(
                r#"読めなくなったのは ⟪ここ⟫ の所です: {"a":"C:⟪ここ⟫\q","b":1}。文字列の中の \ の後ろが JSON のエスケープ（\" \\ \/ \b \f \n \r \t \uXXXX）ではありません。\ そのものは \\ と書きます。"#
            ),
            "{msg}"
        );
        assert!(!msg.contains("括弧の数"), "{msg}");
        let hex = refused_ir(r#"{"a":"\u12G4"}"#);
        assert!(hex.contains(r#"の所です: {"a":"⟪ここ⟫\u12G4"}。"#), "{hex}");
        // 正しいエスケープの後ろの括弧の誤りは今までどおり括弧の数と言う。
        let bracket = refused_ir(r#"{"a":"\n\\"}}"#);
        assert!(bracket.contains("括弧の数の食い違い"), "{bracket}");
    }

    /// serde の位置をバイトの位置に写すとき、列の 0・長さを超える列・字の途中を指す列・無い行を丸める。
    #[test]
    fn byte_at_rounds_to_the_string() {
        let raw = "あい\nうえ";
        assert_eq!(byte_at(raw, 1, 0), 0);
        assert_eq!(byte_at(raw, 0, 0), 0);
        assert_eq!(byte_at(raw, 1, 4), 3);
        assert_eq!(byte_at(raw, 1, 2), 0, "あ の途中は あ の始めに戻す");
        assert_eq!(byte_at(raw, 2, 1), 7);
        assert_eq!(byte_at(raw, 2, 3), 7, "う の途中は う の始めに戻す");
        assert_eq!(byte_at(raw, 2, 100), raw.len());
        assert_eq!(byte_at(raw, 9, 1), raw.len());
        assert_eq!(byte_at(raw, usize::MAX, usize::MAX), raw.len());
    }

    /// 型の誤り（Data）は前後だけを添え、位置の無い誤りには何も添えない。
    #[test]
    fn data_errors_get_only_the_surroundings() {
        let raw = r#""x""#;
        let typed = serde_json::from_str::<u8>(raw).unwrap_err();
        assert_eq!(typed.classify(), Category::Data);
        assert_eq!(
            how_broken(raw, &typed),
            "読めなくなったのは ⟪ここ⟫ の所です: \"x⟪ここ⟫\"。"
        );
        let positionless = serde_json::from_value::<u8>(json!("x")).unwrap_err();
        assert_eq!(positionless.line(), 0);
        assert_eq!(how_broken(raw, &positionless), "");
    }
}
