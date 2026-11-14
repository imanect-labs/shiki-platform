//! ミニアプリの画面の要約（観測に載せる・機能・部品のキーと種類・ボタンの実行先・表）。
//!
//! 画面を読むのは gui と同じ関数（[`yui::extract_action_blocks`]・[`gui::data_grid_blocks`]）で、ボタンを出す図
//! （閉じていて、エラーの無いトップレベルの図）だけを数える。名前と文言は利用者・AI の書いた文字列なので 1 行に潰して
//! 切る（封筒が要るかは呼ぶ側が決める）。

use std::fmt::Write as _;

use crate::data_tool::one_line;

/// 要約に載せる図の数の上限。
const MAX_FIGURES: usize = 10;

/// 部品の種類の名前（Yui の書き方の代表の語・パーサーの表から引く）。
fn kind_word(kind: yui::FieldKind) -> &'static str {
    yui::FIELD_KINDS
        .iter()
        .find(|k| k.kind == kind)
        .map_or("?", |k| k.words[0])
}

/// 画面の要約（複数行）。
#[must_use]
pub(super) fn outline(body: &gui::MiniAppBody) -> String {
    let mut out = String::from("画面:");
    let mut figures = 0;
    for block in yui::extract_action_blocks(&body.screen) {
        let yui::Figure::App { base, sections } = &block.figure else {
            continue;
        };
        figures += 1;
        if figures > MAX_FIGURES {
            out.push_str("\n- …（図が多いので省略）");
            break;
        }
        let _ = write!(out, "\n- 図「{}」", one_line(&base.title, 40));
        for s in sections {
            let title = s
                .title
                .as_deref()
                .map_or("（見出しなし）".into(), |t| one_line(t, 40));
            let fields: Vec<String> = s
                .fields
                .iter()
                .map(|f| {
                    format!(
                        "{}={}{}",
                        one_line(&f.key, 40),
                        kind_word(f.kind),
                        if f.required { "*" } else { "" }
                    )
                })
                .collect();
            let actions: Vec<String> = s
                .actions
                .iter()
                .map(|a| match &a.target {
                    yui::ActionTarget::Workflow { name } => {
                        format!("{} → {}", one_line(&a.label, 40), one_line(name, 40))
                    }
                    yui::ActionTarget::Chat => format!("{} → 会話", one_line(&a.label, 40)),
                })
                .collect();
            let _ = write!(
                out,
                "\n  - 機能「{title}」 入力: {} / ボタン: {}",
                if fields.is_empty() {
                    "なし".into()
                } else {
                    fields.join(", ")
                },
                if actions.is_empty() {
                    "なし".into()
                } else {
                    actions.join(", ")
                },
            );
        }
    }
    let grids: Vec<String> = gui::data_grid_blocks(&body.screen)
        .into_iter()
        .map(|g| g.alias.map_or("（別名なし）".into(), |a| one_line(&a, 40)))
        .collect();
    if !grids.is_empty() {
        let _ = write!(out, "\n- 表: {}", grids.join(", "));
    }
    if figures == 0 && grids.is_empty() {
        out.push_str(" ボタンのある図も表もありません");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_lists_features_keys_buttons_and_grids() {
        let body = gui::MiniAppBody {
            description: "d".into(),
            screen: gui::MINIAPP_SCREEN_EXAMPLE.into(),
            skill: None,
            workflows: vec![],
            data_views: vec![],
        };
        assert_eq!(
            outline(&body),
            "画面:\n- 図「経費」\n  - 機能「経費の申請」 入力: title=text*, amount=number* / ボタン: 申請する → 経費申請\n- 表: 承認待ち"
        );
        let empty = gui::MiniAppBody {
            screen: "# だけ".into(),
            ..body
        };
        assert_eq!(outline(&empty), "画面: ボタンのある図も表もありません");
    }
}
