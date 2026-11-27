//! 図のボタンで起動したワークフローの入力の誤りを、図の部品の名前で言う（GAP-41）。
//!
//! 図の部品の定義（Yui の値の検証）は通ったが、束ねたワークフローの入力項目（`input_schema`）に合わない送信がある
//! （部品は必須でないが入力項目は必須・上限が違うなど）。起動口はそれを入力のキー（部品の `as` 名）と理由で返す。
//! 押した人にキー（`amount` など）を見せず、部品の表示名（「金額」）で言い直す。画面に当たる部品が無い入力（画面と
//! 入力項目が食い違っている）は押した人には直せないので、作った人に伝えるよう言う（付録 C の §3.4.2）。
//!
//! # キーと理由の運び方
//!
//! 起動口の誤りの型（[`ActionError`]・[`crate::dispatch::WorkflowStarter`] の約束）は変えない。起動口の誤りを写す
//! [`from_launcher_error`] は、キーを出さない [`ActionError::Invalid`]（[`INPUT_MISMATCH_FALLBACK`]）を返し、キーと
//! 理由は、図のボタンが起動を [`collect_input_problems`] で包んでいる間だけ、その起動の中に置く。図のボタンはそれを
//! [`with_field_labels`] で部品の名前の文にする。包まない口（図を持たない口・外の起動口）は今までどおりの文になる。

use std::cell::RefCell;
use std::future::Future;

use workflow_engine::run::LauncherError;

use crate::dispatch::ActionError;

tokio::task_local! {
    /// 図のボタンの起動の間、起動口が返した入力の誤り（キーと理由）を置く場所。
    static PROBLEMS: RefCell<Option<Vec<InputProblem>>>;
}

/// 図のボタンの起動（`start`）を走らせ、起動口が入力項目に合わない入力を返したら、そのキーと理由も返す。
pub async fn collect_input_problems<F: Future>(start: F) -> (F::Output, Option<Vec<InputProblem>>) {
    PROBLEMS
        .scope(RefCell::new(None), async move {
            let out = start.await;
            let problems = PROBLEMS.with(|slot| slot.borrow_mut().take());
            (out, problems)
        })
        .await
}

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
///
/// 入力項目に合わない入力は、キーを出さない [`ActionError::Invalid`] にし、キーと理由は
/// [`collect_input_problems`] の中なら置いておく（図のボタンが部品の名前で言い直す）。
#[must_use]
pub fn from_launcher_error(e: LauncherError) -> ActionError {
    match e {
        LauncherError::Ir(_) => ActionError::NotFound,
        LauncherError::Input(issues) => {
            let problems: Vec<InputProblem> = issues
                .into_iter()
                .map(|i| InputProblem {
                    key: i.key,
                    message: i.message,
                })
                .collect();
            // 包まれていない（図を持たない口）なら置かない。
            let _ = PROBLEMS.try_with(|slot| *slot.borrow_mut() = Some(problems));
            ActionError::Invalid(INPUT_MISMATCH_FALLBACK.into())
        }
        LauncherError::NotReady(errors) => ActionError::Invalid(LauncherError::messages(&errors)),
        other => ActionError::Internal(format!("run 起動: {other}")),
    }
}

/// 入力の誤り（[`collect_input_problems`] が受け取ったキーと理由）を、図の部品の表示名で言い直した
/// [`ActionError::Invalid`] にする。入力の誤りでなければ（`problems` が無い）そのまま返す。
#[must_use]
pub fn with_field_labels(
    err: ActionError,
    problems: Option<&[InputProblem]>,
    figure: &yui::Figure,
    surface: ButtonSurface,
) -> ActionError {
    match (err, problems) {
        (ActionError::Invalid(_), Some(problems)) => {
            ActionError::Invalid(problems_text(figure, problems, surface))
        }
        (err, _) => err,
    }
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
        let (err, problems) = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(collect_input_problems(async {
                from_launcher_error(LauncherError::Input(issues))
            }));
        let err = with_field_labels(err, problems.as_deref(), &figure(), surface);
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

    /// 入力の誤りでないものは、そのまま返す。包まない口では、入力の誤りもキーを出さない文になる。
    #[test]
    fn other_errors_pass_through() {
        let err = with_field_labels(ActionError::Forbidden, None, &figure(), ButtonSurface::Chat);
        assert!(matches!(err, ActionError::Forbidden));
        let invalid = ActionError::Invalid("必須の欄が空です".into());
        let err = with_field_labels(invalid, None, &figure(), ButtonSurface::Chat);
        assert!(matches!(err, ActionError::Invalid(ref m) if m == "必須の欄が空です"));
        let bare = from_launcher_error(LauncherError::Input(vec![issue("amount", "必須です")]));
        assert!(matches!(bare, ActionError::Invalid(ref m) if m == INPUT_MISMATCH_FALLBACK));
        assert!(matches!(
            from_launcher_error(LauncherError::Ir("x".into())),
            ActionError::NotFound
        ));
    }
}
