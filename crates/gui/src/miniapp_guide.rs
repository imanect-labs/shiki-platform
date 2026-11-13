//! AI に渡すミニアプリの画面の書き方（D-10 T07・HD-3）。`miniapp_create` / `miniapp_update` の説明にだけ載せる
//! （通常の会話の system は太らせない）。
//!
//! Yui の app の書き方は [`yui::app_guide`]（パーサーの表と定数から生成）を使い、ここではミニアプリの画面の
//! 決まり（束ねた部品の別名・表・使えない種類・上限）を足す。上限はこの crate の定数から取り、例はテストで
//! [`validate_miniapp_body`](crate::miniapp::validate_miniapp_body)・書き損じ・入力の突き合わせを通ることを確かめる。

use std::fmt::Write as _;

use crate::miniapp::{MAX_ALIAS_CHARS, MAX_DATA_VIEWS, MAX_SCREEN_BYTES, MAX_WORKFLOWS};

/// 案内に載せるミニアプリの画面の例（束ねたワークフロー `経費申請`・保存ビュー `承認待ち` を参照する）。
pub const MINIAPP_SCREEN_EXAMPLE: &str = "\
# 経費アプリ

```ui
app 経費
## 経費の申請
件名 = text 必須 as title
金額 = number 1..1000000 円 必須 as amount
button 申請する -> workflow 経費申請
```

## 承認待ちの経費

```data_grid
承認待ち
```
";

/// ミニアプリの画面（Markdown）の書き方。
#[must_use]
pub fn miniapp_screen_guide() -> String {
    let mut out = String::new();
    out.push_str("# ミニアプリの画面の書き方\n");
    let _ = writeln!(
        out,
        "- 画面は Markdown（{} KB まで）。見出し・文章に、入力とボタンの図（```ui の app）と表（```data_grid）を置く。図と表はトップレベルに置く（引用・箇条書きの中は動かない）。",
        MAX_SCREEN_BYTES / 1024
    );
    let _ = writeln!(
        out,
        "- ボタンの実行先 `-> workflow 名前` の名前は、束ねたワークフロー（workflows、{MAX_WORKFLOWS} 個まで）の alias をそのまま書く（alias は前後に空白の無い {MAX_ALIAS_CHARS} 字まで）。束ねていない名前は保存の誤りになる。"
    );
    out.push_str("- 部品のキー（`as キー`、無ければラベル）は、ボタンの実行先のワークフローの input_schema の properties のキーと揃える。required の入力には同じキーの部品を置いて `必須` を付け、型（文字・数値・選択・日付・オン・オフ）も合わせる。input_schema に無いキーの部品は置かない。\n");
    out.push_str(
        "- 会話に送るボタン（`-> chat`）・質問（ask）・計画（plan）はミニアプリでは使えない。\n",
    );
    out.push_str("- 閉じていない図・解釈できない行・上限を超えた行・入力の食い違いは、黙って落ちずに保存の誤りになる。誤りの文を読んで直して保存し直す。\n");
    let _ = writeln!(
        out,
        "- 表は、束ねた保存ビュー（data_views、{MAX_DATA_VIEWS} 個まで）の alias を中身の 1 行目に書いた ```data_grid で置く。データは開いた人の権限で毎回読む。"
    );
    out.push('\n');
    out.push_str(&yui::app_guide());
    out.push_str("\n画面全体の例（workflows に alias「経費申請」、data_views に alias「承認待ち」を束ねる）:\n");
    out.push_str(MINIAPP_SCREEN_EXAMPLE);
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::miniapp::{screen_problems, validate_miniapp_body};
    use crate::miniapp_inputs::{input_hints, screen_input_warnings};

    fn body(screen: &str) -> serde_json::Value {
        json!({
            "description": "経費の申請",
            "screen": screen,
            "workflows": [{ "alias": "経費申請", "artifact_id": uuid::Uuid::from_u128(1), "version": 1 }],
            "data_views": [{ "alias": "承認待ち", "artifact_id": uuid::Uuid::from_u128(2), "version": 1 }],
        })
    }

    /// 画面全体の例は、構造の検証・書き損じ・入力の突き合わせ（例が言う入力のワークフロー）を通る。
    #[test]
    fn screen_example_passes_every_check() {
        let parsed = validate_miniapp_body(&body(MINIAPP_SCREEN_EXAMPLE)).expect("例は保存できる");
        assert!(screen_problems(&parsed).is_empty());
        let schema = json!({ "type": "object", "required": ["title", "amount"], "properties": {
            "title": { "type": "string" }, "amount": { "type": "integer" } } });
        let warnings = screen_input_warnings(
            MINIAPP_SCREEN_EXAMPLE,
            [("経費申請", input_hints(Some(&schema)))],
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(miniapp_screen_guide().ends_with(MINIAPP_SCREEN_EXAMPLE));
    }

    /// Yui の app の例（ボタンの実行先が 2 つ）も、その 2 つを束ねればミニアプリの画面として通る。
    #[test]
    fn yui_example_is_a_valid_screen_when_its_workflows_are_bundled() {
        let raw = json!({
            "description": "経費",
            "screen": yui::APP_GUIDE_EXAMPLE,
            "workflows": [
                { "alias": "経費申請", "artifact_id": uuid::Uuid::from_u128(1), "version": 1 },
                { "alias": "月次集計", "artifact_id": uuid::Uuid::from_u128(2), "version": 1 }
            ],
        });
        let parsed = validate_miniapp_body(&raw).expect("例は保存できる");
        assert!(screen_problems(&parsed).is_empty());
    }

    /// 案内は Yui の app の書き方を含み、上限はこの crate の定数から作る。
    #[test]
    fn guide_embeds_the_app_guide_and_limits() {
        let guide = miniapp_screen_guide();
        assert!(guide.contains(&yui::app_guide()));
        assert!(guide.contains(&format!("{MAX_WORKFLOWS} 個まで")));
        assert!(guide.contains(&format!("{MAX_DATA_VIEWS} 個まで")));
        assert!(guide.contains(&format!("{} KB まで", MAX_SCREEN_BYTES / 1024)));
    }
}
