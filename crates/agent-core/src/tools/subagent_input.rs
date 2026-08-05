//! 委譲ツールの**入力の解釈**（`subagent.rs` から分割・500 行規約）。
//!
//! 「どのロールとして走らせるか」と「必須項目が入っているか」だけを持つ。ツールの提示・予算・
//! 隔離は `subagent.rs`、ロールごとの system プロンプトは `subagent_prompts.rs` が持つ。

use super::subagent_prompts::{PLAN_SYSTEM, VERIFY_SYSTEM};
use crate::tool::ToolError;

/// 委譲のロール。**何を渡すか**と**何を返させるか**が変わる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// 調べて findings を返す（既定）。
    Research,
    /// 調べずに「何を確かめるべきか」だけを返す（#402）。
    Plan,
    /// 書き上がったレポートの主張が証拠に紐づいているかを見る（#407）。
    ///
    /// **執筆の委譲ではない**（書き直させない）。返るのは指摘のリストだけで、直すのは親。
    Verify,
}

impl Role {
    pub(super) fn parse(raw: Option<&str>) -> Role {
        match raw {
            Some("plan") => Role::Plan,
            Some("verify") => Role::Verify,
            _ => Role::Research,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Role::Research => "research",
            Role::Plan => "plan",
            Role::Verify => "verify",
        }
    }

    /// 担当範囲（`boundary`）が必須か。
    ///
    /// 分担して重複なく調べるための概念なので、必須なのは調査だけ。計画は依頼だけを見て立て、
    /// 検証はレポート全体が既定の範囲（節を割って複数体で検証したいときだけ任意で指定する）。
    pub(super) fn needs_boundary(self) -> bool {
        matches!(self, Role::Research)
    }

    /// ロールごとの system プロンプト。調査だけは呼び出し側の設定を使う（差し替え可）。
    pub(super) fn system(self, research: &str) -> &str {
        match self {
            Role::Plan => PLAN_SYSTEM,
            Role::Verify => VERIFY_SYSTEM,
            Role::Research => research,
        }
    }

    /// 委譲が失敗したときに**親がやるべきこと**（ロールで違う）。
    ///
    /// 検証が失敗した時点でレポートは既に書き上がっているので、調査と同じ「boundary を狭くして
    /// 調べ直せ」は誤った誘導になる（実測: 検証委譲が空を返した run で、親が調査のやり直しへ
    /// 行きかけた）。ここで必要なのは、自分で `report.md` と `notes.md` を突き合わせること。
    pub(super) fn fallback(self) -> &'static str {
        match self {
            Role::Verify => {
                "**レポートは既に書けている**ので調査へ戻らないこと。自分で report.md を頭から\
                 読み、実質的な主張を 1 つずつ notes.md の証拠 ID と突き合わせて直したうえで、\
                 **必ず提出まで進むこと**（検証を飛ばして出さない）。"
            }
            Role::Plan => "計画は自分で立てて先へ進むこと。",
            Role::Research => {
                "委譲をやり直すなら boundary をもっと狭く切ること。やり直さない場合は自分で\
                 web_search / web_fetch を使って調べ、**レポートは必ず書くこと**。"
            }
        }
    }

    /// 子にツールを渡すか。
    ///
    /// 計画だけは**調べさせない**（承認前に調査するのと変わらなくなる・#402）。検証は
    /// 逐語引用を原典と突き合わせる必要があるので、調査と同じ read-only 一式を渡す。
    pub(super) fn uses_tools(self) -> bool {
        !matches!(self, Role::Plan)
    }
}

/// 入力の必須文字列を取り出す（空白のみは欠落として扱う）。
pub(super) fn required(input: &serde_json::Value, key: &str) -> Result<String, ToolError> {
    let raw = input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    if raw.is_empty() {
        return Err(ToolError::Invalid(format!("missing '{key}'")));
    }
    Ok(raw.to_string())
}

/// 任意の文字列（空はなし扱い）。
pub(super) fn optional(input: &serde_json::Value, key: &str) -> Option<String> {
    let raw = input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    (!raw.is_empty()).then(|| raw.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// 担当範囲が必須なのは調査だけ（分担の概念があるのはそこだけ・#407）。
    #[test]
    fn boundary_is_required_only_for_research() {
        assert_eq!(Role::parse(None), Role::Research);
        assert_eq!(Role::parse(Some("verify")), Role::Verify);
        assert_eq!(Role::parse(Some("plan")), Role::Plan);
        // 知らない値は既定（調査）に落とす。
        assert_eq!(Role::parse(Some("なにか")), Role::Research);
        assert!(Role::Research.needs_boundary());
        assert!(!Role::Plan.needs_boundary());
        assert!(!Role::Verify.needs_boundary());
    }

    /// 検証者には**独立した読み手**の役目を与え、原典に当たれるようツールも渡す（#407）。
    #[test]
    fn verify_is_an_independent_reviewer_that_never_rewrites() {
        let system = Role::Verify.system("調査用の設定");
        assert!(system.contains("独立した検証者"), "{system}");
        assert!(system.contains("書いた本人ではありません"), "{system}");
        assert!(system.contains("書き直さない"), "{system}");
        // 上限で切られると指摘が何も残らない。着地指示は**調査と検証の両方**に要る
        // （実測: verify が 8 ステップ使い切って空で返し、親が自分で突き合わせ直す羽目になった）。
        for (role, prompt) in [
            ("verify", VERIFY_SYSTEM),
            ("research", crate::tools::subagent_prompts::DEFAULT_SYSTEM),
        ] {
            assert!(prompt.contains("[予算]"), "{role} に着地指示が無い");
        }
        // 逐語引用を原典と突き合わせる必要があるのでツールを渡す（計画だけが渡さない）。
        assert!(Role::Verify.uses_tools());
        assert!(Role::Research.uses_tools());
        assert!(!Role::Plan.uses_tools());
        // 調査の system だけは呼び出し側の設定をそのまま使う。
        assert_eq!(Role::Research.system("調査用の設定"), "調査用の設定");
        assert!(Role::Plan
            .system("調査用の設定")
            .contains("計画だけを立てる"));
    }

    #[test]
    fn required_and_optional_treat_blank_as_missing() {
        let input = serde_json::json!({ "objective": " 調べる ", "boundary": "  ", "hint": "" });
        assert_eq!(required(&input, "objective").unwrap(), "調べる");
        assert!(matches!(
            required(&input, "boundary"),
            Err(ToolError::Invalid(_))
        ));
        assert!(optional(&input, "hint").is_none());
        assert!(optional(&input, "missing").is_none());
    }
}
