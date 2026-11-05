//! 図のボタンで起動したワークフローの入力の誤りを、図の部品の名前で言う（GAP-41）。
//!
//! 図の部品の定義（Yui の値の検証）は通ったが、束ねたワークフローの入力項目（`input_schema`）に合わない送信がある
//! （部品は必須でないが入力項目は必須・上限が違うなど）。起動口はそれを入力のキー（部品の `as` 名）と理由で返す
//! （[`ActionError::InvalidInput`]）。押した人にキー（`amount` など）を見せず、部品の表示名（「金額」）で言い直す。
//! 画面に当たる部品が無い入力（画面と入力項目が食い違っている）は押した人には直せないので、作った人に伝えるよう言う
//! （付録 C の §3.4.2）。

use workflow_engine::run::LauncherError;

use crate::dispatch::ActionError;

/// 起動の入力が入力項目に合わない 1 件（キーと理由・BUG-109 と同じ形）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputProblem {
    /// 入力のキー（入れ子は `addr.zip`・リストの要素は `tags[2]`）。入力全体の誤りは空。
    pub key: String,
    /// 理由（利用者に見せてよい日本語・例: `必須です`）。
    pub message: String,
}

/// ボタンを置いた場所（押した人が直せない誤りの、伝える先の言い方を変える）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSurface {
    /// ミニアプリの画面（作った人に伝える）。
    MiniApp,
    /// 会話の回答の図（AI に図を作り直してもらう）。
    Chat,
}

/// 押した人が直せない入力の誤り（部品に当たらない入力・入力全体の誤り）の文。
fn mismatch_text(surface: ButtonSurface) -> &'static str {
    match surface {
        ButtonSurface::MiniApp => {
            "画面の入力が、ワークフローの入力項目と合っていないため、実行できませんでした。アプリを作成した人に伝えてください。"
        }
        ButtonSurface::Chat => {
            "この図の入力が、ワークフローの入力項目と合っていないため、実行できませんでした。AI に図を作り直すよう頼んでください。"
        }
    }
}

/// 図の部品に当たらない入力の誤りを言い直さずに返すときの文（図を持たない口の最後の手段・キーは出さない）。
pub const INPUT_MISMATCH_FALLBACK: &str = "入力が、ワークフローの入力項目と合っていません。";

/// ワークフローの起動口の誤りを、図のボタンの誤りに写す（api の配線とテストが同じものを使う）。
#[must_use]
pub fn from_launcher_error(e: LauncherError) -> ActionError {
    match e {
        LauncherError::Ir(_) => ActionError::NotFound,
        LauncherError::Input(issues) => ActionError::InvalidInput(
            issues
                .into_iter()
                .map(|i| InputProblem {
                    key: i.key,
                    message: i.message,
                })
                .collect(),
        ),
        LauncherError::NotReady(errors) => ActionError::Invalid(LauncherError::messages(&errors)),
        other => ActionError::Internal(format!("run 起動: {other}")),
    }
}

/// 入力の誤り（[`ActionError::InvalidInput`]）を、図の部品の表示名で言い直した [`ActionError::Invalid`] にする。
/// ほかの誤りはそのまま返す。
#[must_use]
pub fn with_field_labels(
    err: ActionError,
    figure: &yui::Figure,
    surface: ButtonSurface,
) -> ActionError {
    let ActionError::InvalidInput(problems) = err else {
        return err;
    };
    ActionError::Invalid(problems_text(figure, &problems, surface))
}

/// 部品に当たる誤りは「部品の名前: 理由」、当たらない誤りは作った人に伝える 1 文（1 回だけ・最後）。
fn problems_text(
    figure: &yui::Figure,
    problems: &[InputProblem],
    surface: ButtonSurface,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut mismatch = false;
    for p in problems {
        // 入れ子・リストの要素は、先頭のキーの部品で言う（図の部品は 1 段の値だけを送る）。
        let head = p.key.split(['.', '[']).next().unwrap_or_default();
        match yui::field_label(figure, head) {
            Some(label) => {
                let line = format!("{label}: {}", p.message);
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
            None => mismatch = true,
        }
    }
    if mismatch || lines.is_empty() {
        lines.push(mismatch_text(surface).to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_engine::ir::input_schema::InputIssue;

    const APP: &str = "```ui\napp 申請\n金額 = number 円 as amount\nタグ = choice A | B multi as tags\nbutton 申請する -> workflow expense\n```\n";

    fn figure() -> yui::Figure {
        yui::extract_action_blocks(APP).remove(0).figure
    }

    fn issue(key: &str, message: &str) -> InputIssue {
        InputIssue {
            key: key.into(),
            message: message.into(),
        }
    }

    fn text(issues: Vec<InputIssue>, surface: ButtonSurface) -> String {
        let err = with_field_labels(
            from_launcher_error(LauncherError::Input(issues)),
            &figure(),
            surface,
        );
        let ActionError::Invalid(m) = err else {
            panic!("{err:?}")
        };
        m
    }

    /// 部品に当たる入力は部品の名前で言い、キー（amount）を出さない。
    #[test]
    fn names_the_field_instead_of_the_key() {
        let m = text(
            vec![
                issue("amount", "必須です"),
                issue("tags[1]", "選択肢にありません"),
            ],
            ButtonSurface::MiniApp,
        );
        assert_eq!(m, "金額: 必須です\nタグ: 選択肢にありません");
        assert!(!m.contains("amount"), "{m}");
    }

    /// 画面に部品が無い入力・入力全体の誤りは、キーを出さずに作った人に伝えるよう言う（1 回だけ）。
    #[test]
    fn unmatched_inputs_ask_to_tell_the_creator() {
        let m = text(
            vec![
                issue("amount", "1,000 以下にしてください"),
                issue("note", "必須です"),
                issue("", "項目のまとまりにしてください"),
            ],
            ButtonSurface::MiniApp,
        );
        assert_eq!(
            m,
            "金額: 1,000 以下にしてください\n画面の入力が、ワークフローの入力項目と合っていないため、実行できませんでした。アプリを作成した人に伝えてください。"
        );
        assert!(!m.contains("note"), "{m}");
        let chat = text(vec![issue("note", "必須です")], ButtonSurface::Chat);
        assert!(
            chat.contains("AI に図を作り直すよう頼んでください"),
            "{chat}"
        );
    }

    /// 入力の誤りでないものは、そのまま返す。
    #[test]
    fn other_errors_pass_through() {
        let err = with_field_labels(ActionError::Forbidden, &figure(), ButtonSurface::Chat);
        assert!(matches!(err, ActionError::Forbidden));
        assert!(matches!(
            from_launcher_error(LauncherError::Ir("x".into())),
            ActionError::NotFound
        ));
    }
}
