//! 作る人が JSON Schema に書く `pattern`（入力項目・「AI に聞く」の出力の形）の読み方と、保存のときの上限（単一の定義）。
//!
//! - 読み方（[`compile_pattern`]）: regex crate（線形時間なので入力で暴走しない）で、コンパイルの大きさを 1 MiB まで
//!   にして読む。起動の入力の確かめ・出力の確かめ・保存の確かめが同じ読み方をする。
//! - 保存のときの上限（[`PatternBudget::for_save`]・BUG-222・BUG-226）: 1 つ [`MAX_PATTERN_CHARS`] 文字まで（条件の
//!   「正規表現に一致」と同じ数・[`crate::ir::expr::MAX_REGEX_LEN`]）、数は [`MAX_PATTERNS`] 個まで。数える範囲は、
//!   入力項目では入力項目の全体、出力の形ではワークフローの全体（すべての「AI に聞く」の出力の形を合わせて）。
//!   超えた分はコンパイルしない（だれでも呼べる検証の口で、何万個もの pattern をコンパイルさせない）。数の上限は、
//!   超えた最初の位置で 1 回だけ言う。
//! - 保存のときだけの上限で、保存済みの版の起動と実行は変えない（実行の確かめは [`PatternBudget::unlimited`] で
//!   すべて読む）。

use serde_json::Value;

/// 1 つの `pattern` の長さの上限（文字数・条件の正規表現と同じ数）。
pub(crate) const MAX_PATTERN_CHARS: usize = crate::ir::expr::MAX_REGEX_LEN;
/// 数える範囲（入力項目の全体・出力の形のワークフローの全体）に書ける `pattern` の数の上限。
pub(crate) const MAX_PATTERNS: usize = 50;

/// 作者が書いた `pattern` を読む（読めなければ `None`）。
pub(crate) fn compile_pattern(p: &str) -> Option<regex::Regex> {
    #[cfg(test)]
    COMPILED.with(|n| n.set(n.get() + 1));
    regex::RegexBuilder::new(p).size_limit(1 << 20).build().ok()
}

#[cfg(test)]
thread_local! {
    /// このスレッドで [`compile_pattern`] を呼んだ回数（テストがコンパイルし直していないことを確かめる・BUG-227）。
    static COMPILED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// このスレッドで [`compile_pattern`] を呼んだ回数（テスト用）。
#[cfg(test)]
pub(crate) fn compiled_count() -> usize {
    COMPILED.with(std::cell::Cell::get)
}

/// `pattern` を数える範囲（誤りの文言が言う）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatternScope {
    /// 入力項目（`input_schema`）の全体。
    Input,
    /// ワークフローの全体の「AI に聞く」の出力の形（`output_schema`）。
    Output,
}

/// 1 つの `pattern` の誤り。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatternIssue {
    /// 長さの上限を超えた。
    TooLong,
    /// 文字列でない・正規表現として読めない。
    Unreadable,
    /// 数の上限を超えた（`first` は超えた最初の 1 つ。誤りとして言うのはそれだけ）。
    TooMany { first: bool },
}

impl PatternIssue {
    /// 利用者向けの文言（超えた最初の 1 つ以外の数の超過は `None`・言わずに飛ばす）。
    pub(crate) fn message(self, scope: PatternScope) -> Option<String> {
        Some(match (self, scope) {
            (PatternIssue::TooMany { first: false }, _) => return None,
            (PatternIssue::TooLong, _) => format!("pattern は {MAX_PATTERN_CHARS} 文字までです"),
            (PatternIssue::Unreadable, PatternScope::Input) => {
                "pattern を正規表現として読めません".to_string()
            }
            (PatternIssue::Unreadable, PatternScope::Output) => {
                "pattern は正しい正規表現の文字列で書いてください".to_string()
            }
            (PatternIssue::TooMany { first: true }, PatternScope::Input) => {
                format!("pattern は入力項目の全体で {MAX_PATTERNS} 個までです")
            }
            (PatternIssue::TooMany { first: true }, PatternScope::Output) => {
                format!("出力の形の pattern は、ワークフローの全体で {MAX_PATTERNS} 個までです")
            }
        })
    }
}

/// 数える範囲の中で見た `pattern` の数と、上限を掛けるか（[モジュールの説明](self)）。
#[derive(Debug)]
pub(crate) struct PatternBudget {
    seen: usize,
    capped: bool,
}

impl PatternBudget {
    /// 保存のときの確かめ（長さと数の上限を掛ける）。
    pub(crate) const fn for_save() -> Self {
        PatternBudget {
            seen: 0,
            capped: true,
        }
    }

    /// 実行のときの確かめ（上限を掛けずにすべて読む・保存済みの版の実行を変えない）。
    pub(crate) const fn unlimited() -> Self {
        PatternBudget {
            seen: 0,
            capped: false,
        }
    }

    /// スキーマの `pattern` の値 1 つを数えて確かめる。上限の内のものだけをコンパイルする。
    pub(crate) fn check(&mut self, p: &Value) -> Result<(), PatternIssue> {
        self.seen += 1;
        if self.capped && self.seen > MAX_PATTERNS {
            return Err(PatternIssue::TooMany {
                first: self.seen == MAX_PATTERNS + 1,
            });
        }
        let Some(text) = p.as_str() else {
            return Err(PatternIssue::Unreadable);
        };
        if self.capped && text.chars().count() > MAX_PATTERN_CHARS {
            return Err(PatternIssue::TooLong);
        }
        compile_pattern(text)
            .map(|_| ())
            .ok_or(PatternIssue::Unreadable)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// 保存のときは長さ（文字数）と数に上限があり、超えた分はコンパイルしない。実行のときは上限を掛けない。
    #[test]
    fn the_save_budget_caps_length_and_count() {
        let mut save = PatternBudget::for_save();
        assert_eq!(save.check(&json!("あ".repeat(MAX_PATTERN_CHARS))), Ok(()));
        assert_eq!(
            save.check(&json!("a".repeat(MAX_PATTERN_CHARS + 1))),
            Err(PatternIssue::TooLong)
        );
        assert_eq!(save.check(&json!("(")), Err(PatternIssue::Unreadable));
        assert_eq!(save.check(&json!(5)), Err(PatternIssue::Unreadable));
        for _ in 4..MAX_PATTERNS {
            assert_eq!(save.check(&json!("^a$")), Ok(()));
        }
        let before = compiled_count();
        assert_eq!(
            save.check(&json!("^a$")),
            Err(PatternIssue::TooMany { first: true })
        );
        assert_eq!(
            save.check(&json!("(")),
            Err(PatternIssue::TooMany { first: false })
        );
        assert_eq!(compiled_count(), before, "上限を超えた分はコンパイルしない");

        let mut run = PatternBudget::unlimited();
        for _ in 0..=MAX_PATTERNS {
            assert_eq!(run.check(&json!("^a$")), Ok(()));
        }
        assert_eq!(run.check(&json!("a".repeat(MAX_PATTERN_CHARS + 1))), Ok(()));
    }

    /// 数の超過は最初の 1 つだけを言い、文言は数える範囲で言い分ける。
    #[test]
    fn messages_name_the_scope() {
        let first = PatternIssue::TooMany { first: true };
        assert_eq!(
            first.message(PatternScope::Input).as_deref(),
            Some("pattern は入力項目の全体で 50 個までです")
        );
        assert_eq!(
            first.message(PatternScope::Output).as_deref(),
            Some("出力の形の pattern は、ワークフローの全体で 50 個までです")
        );
        assert_eq!(
            PatternIssue::TooMany { first: false }.message(PatternScope::Output),
            None
        );
        assert_eq!(
            PatternIssue::TooLong
                .message(PatternScope::Output)
                .as_deref(),
            Some("pattern は 256 文字までです")
        );
    }
}
