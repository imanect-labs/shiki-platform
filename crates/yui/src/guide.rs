//! AI に渡す Yui の app の書き方の案内（[`app_guide`]・D-10 T07・HD-3）。
//!
//! 部品の種類と別名・オプションはパーサーと同じ表（[`FIELD_KINDS`]）から、上限はパーサーの定数から組み立てる
//! （codegen が正。手書きの対応表を別に持たない）。例はテストで診断 0 件を確かめる。
//!
//! 案内はミニアプリの画面向けで、ボタンの実行先はワークフローだけを書く（会話に送る `-> chat` はミニアプリでは
//! 使えない）。画面全体の決まり（束ねた部品の別名・表）はホスト（`gui::miniapp_screen_guide`）が足す。

use std::fmt::Write as _;

use crate::app::{kind_names, FIELD_KINDS};
use crate::text::{
    APP_CHOICE_OPTIONS, APP_FIELDS, APP_LABEL, APP_SECTIONS, APP_SECTION_ACTIONS,
    APP_SECTION_FIELDS, TEXT_LENGTH,
};

/// `as キー` に書けるキーの長さの上限（パーサーの `KEY` の正規表現と同じ）。
pub const APP_KEY_CHARS: usize = 64;

/// 案内に載せる app の図の例（診断 0 件・テストで確かめる）。
pub const APP_GUIDE_EXAMPLE: &str = "\
```ui
app 経費の処理
## 経費の申請
> 領収書の内容を入力してください。
件名 = text 必須 例 4月 大阪出張の交通費 as title
金額 = number 1..1000000 円 必須 as amount
区分 = choice 交通費 | 出張費 | 備品 必須 as category
日付 = date 必須 as day
button 申請する -> workflow 経費申請

## 月次の集計
対象月 = month 必須 as month
button 集計する -> workflow 月次集計
```
";

/// Yui の app（入力部品とボタンの画面）の書き方。部品の表・上限の定数・例から組み立てる。
#[must_use]
pub fn app_guide() -> String {
    let mut out = String::new();
    out.push_str("## 画面の図（```ui の app）の書き方\n");
    out.push_str("- ```ui のフェンスの 1 行目に `app タイトル`、2 行目から中身を書く。\n");
    out.push_str("- `## 見出し` で機能を分け、`> 説明` で機能の説明を書く。1 つの機能が 1 つのフォームになる。\n");
    out.push_str(
        "- 入力は 1 行に 1 つ、`ラベル = 種類 オプション`。オプションはどの種類でも次を書ける:\n",
    );
    out.push_str("  - `必須`: 入力しないと押せない。\n");
    let _ = writeln!(
        out,
        "  - `as キー`: ワークフローに渡す入力のキー（英字か `_` で始まる英数字・`_`・`.`・`-`、{APP_KEY_CHARS} 字まで）。書かないとラベルがキーになる。"
    );
    out.push_str("- ボタンは `button ラベル -> workflow 名前`。押すと、同じ機能の入力をキーと値にしてそのワークフローを起動する（起動の前に確認が出る）。\n");
    let _ = writeln!(out, "- 種類（{}）:", kind_names("・"));
    for k in &FIELD_KINDS {
        let aliases = if k.words.len() > 1 {
            format!("（別名 {}）", k.words[1..].join("・"))
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "  - `{}`{aliases}: {}。例 `{}`",
            k.words[0], k.syntax, k.example
        );
    }
    let _ = writeln!(
        out,
        "- 上限: 機能 {APP_SECTIONS} 個、入力は 1 つの機能に {APP_SECTION_FIELDS} 個・全体で {APP_FIELDS} 個、ボタンは 1 つの機能に {APP_SECTION_ACTIONS} 個、選択肢 {APP_CHOICE_OPTIONS} 個、ラベル {APP_LABEL} 字、説明・例 {TEXT_LENGTH} 字。"
    );
    out.push_str("- 書くのはデータだけ。色・配置・HTML・JSON は書かない。\n");
    out.push_str("\n例:\n");
    out.push_str(APP_GUIDE_EXAMPLE);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{extract_action_blocks, parse_block, AppField, AppSection, Figure, Kind};

    fn sections(figure: Option<&Figure>) -> &[AppSection] {
        match figure {
            Some(Figure::App { sections, .. }) => sections,
            _ => &[],
        }
    }

    /// app の図の最初の部品（読めなければ `None`）。
    fn first_field(source: &str) -> Option<AppField> {
        let parsed = parse_block(source, Some(Kind::App));
        sections(parsed.figure.as_ref())
            .first()?
            .fields
            .first()
            .cloned()
    }

    /// 案内の例は診断 0 件で読め、ボタンはワークフローに向く。
    #[test]
    fn guide_example_parses_without_diagnostics() {
        let blocks = extract_action_blocks(APP_GUIDE_EXAMPLE);
        assert_eq!(blocks.len(), 1);
        assert!(
            blocks[0].diagnostics.is_empty(),
            "{:?}",
            blocks[0].diagnostics
        );
        assert_eq!(sections(Some(&blocks[0].figure)).len(), 2);
        assert!(app_guide().ends_with(APP_GUIDE_EXAMPLE));
    }

    /// 表の種類ごとの例は、その種類の部品として診断 0 件で読める（案内の書き方がパーサーと揃う）。
    #[test]
    fn every_kind_example_parses_as_that_kind() {
        for k in &FIELD_KINDS {
            let source = format!("app 例\n{}\nbutton 送る -> workflow 例\n", k.example);
            let parsed = parse_block(&source, Some(Kind::App));
            assert!(
                parsed.diagnostics.is_empty(),
                "{}: {:?}",
                k.example,
                parsed.diagnostics
            );
            assert_eq!(
                first_field(&source).map(|f| f.kind),
                Some(k.kind),
                "{}",
                k.example
            );
        }
    }

    /// 表は種類をちょうど 1 回ずつ持ち、どの語もその種類として読める。
    #[test]
    fn table_covers_every_kind_once_and_words_round_trip() {
        use crate::FieldKind as F;
        let all = [
            F::Text,
            F::Textarea,
            F::Number,
            F::Slider,
            F::Count,
            F::Choice,
            F::Date,
            F::Month,
            F::Daterange,
            F::Rating,
            F::Toggle,
        ];
        for kind in all {
            // 種類を足したら match が網羅でなくなり、ここで気づく。
            match kind {
                F::Text
                | F::Textarea
                | F::Number
                | F::Slider
                | F::Count
                | F::Choice
                | F::Date
                | F::Month
                | F::Daterange
                | F::Rating
                | F::Toggle => {}
            }
            assert_eq!(
                FIELD_KINDS.iter().filter(|k| k.kind == kind).count(),
                1,
                "{kind:?}"
            );
        }
        for k in &FIELD_KINDS {
            for word in k.words {
                let source = format!("app 例\nラベル = {word} {}\n", range_for(k.kind));
                assert_eq!(first_field(&source).map(|f| f.kind), Some(k.kind), "{word}");
            }
        }
    }

    fn range_for(kind: crate::FieldKind) -> &'static str {
        match kind {
            crate::FieldKind::Slider | crate::FieldKind::Count => "1..5",
            crate::FieldKind::Choice => "a | b",
            _ => "",
        }
    }

    /// 案内の上限は定数から作る（定数を変えれば案内も変わる）。キーの長さはパーサーと揃う。
    #[test]
    fn guide_states_limits_and_key_length_from_the_parser() {
        let guide = app_guide();
        assert!(guide.contains(&format!("機能 {APP_SECTIONS} 個")));
        assert!(guide.contains(&format!("全体で {APP_FIELDS} 個")));
        assert!(guide.contains(&format!("{APP_KEY_CHARS} 字まで")));
        for k in &FIELD_KINDS {
            assert!(guide.contains(&format!("`{}`", k.words[0])), "{:?}", k.kind);
        }
        let key = |n: usize| -> Option<usize> {
            let source = format!("app 例\n件名 = text as k{}\n", "a".repeat(n - 1));
            first_field(&source).map(|f| f.key.len())
        };
        assert_eq!(key(APP_KEY_CHARS), Some(APP_KEY_CHARS));
        assert_eq!(key(APP_KEY_CHARS + 1), Some("件名".len()));
    }
}
