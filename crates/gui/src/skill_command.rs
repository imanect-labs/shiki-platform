//! スラッシュコマンド起動の宣言と解決（#387・#400）。
//!
//! `skill` は `/`（コンポーザ）から起動できることを **body で宣言する**。フロントには
//! コマンド定義を一切置かず、補完候補も引数プリセットも `GET /skills/catalog` 経由で
//! ここから配る（別の一覧をフロントで組むと「補完に出たのに呼べない」ずれが生まれる）。
//!
//! コマンドは能力を増やさない — 起動の入口を作るだけで、実効権限は従来どおり
//! 「セッション配線 ∩ 実行主体 ReBAC ∩ skill 宣言」。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::skill::skill_limits;
use crate::validate::GuiValidationError;

/// スキルをコンポーザから `/<name>` で起動するための宣言（#387）。
///
/// **フロントにコマンド定義をハードコードしない**ための宣言。補完候補・引数のプリセットは
/// すべてここから来る（`GET /skills/catalog` で配る）。コマンドは能力を増やさない —
/// 起動の入口を作るだけで、実効権限は従来どおり「セッション配線 ∩ 実行主体 ReBAC ∩ skill 宣言」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SkillCommand {
    /// コマンド名（`/` は含めない）。`^[a-z0-9][a-z0-9-]*$`。
    pub name: String,
    /// 入力欄のプレースホルダ（「調べたいことを入力」など）。
    #[serde(default)]
    pub hint: Option<String>,
    /// 引数プリセット（`/deep-research auto` の `auto`）。空文字は引数なしの既定。
    #[serde(default)]
    pub variants: Vec<SkillCommandVariant>,
}

impl SkillCommand {
    /// 発話リテラルがこのコマンドのどの variant で起動したかを解く。
    ///
    /// 発話は `/<name> [<args>] <依頼>`（`web/src/lib/slash-command.ts` の `composeText` が
    /// 組み立てる形）。`args` は空白を含み得るので**長い順に最長一致**を採る（`""` と `"auto"`
    /// が両方あるとき `/x auto …` は `auto` として解く）。コマンド名が違えば `None`。
    ///
    /// 解決は**発話の時点**で行い、当たった variant の `args` を run のピンへ焼き込む
    /// （`chat::SkillPin::command_args`）。カード回答で続く発話にはコマンドリテラルが
    /// 無いため、毎ターン引き直すことはできない。
    pub fn variant_for_invocation(&self, text: &str) -> Option<&SkillCommandVariant> {
        let rest = text
            .trim_start()
            .strip_prefix('/')?
            .strip_prefix(self.name.as_str())
            .filter(|r| r.is_empty() || r.starts_with(char::is_whitespace))?
            .trim_start();
        let mut variants: Vec<&SkillCommandVariant> = self.variants.iter().collect();
        variants.sort_by_key(|v| std::cmp::Reverse(v.args.len()));
        variants.into_iter().find(|v| {
            v.args.is_empty()
                || rest
                    .strip_prefix(v.args.as_str())
                    .is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
        })
    }

    /// 焼き込まれた `args` から variant を引き直す（run のピン → 宣言の解決）。
    ///
    /// `variants` が空の skill は「引数なしの 1 択」を暗黙に持つ（宣言が無いので `None`）。
    pub fn variant_by_args(&self, args: &str) -> Option<&SkillCommandVariant> {
        self.variants.iter().find(|v| v.args == args)
    }
}

/// コマンドの引数プリセット 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SkillCommandVariant {
    /// コマンド名の後ろに置く引数（空文字＝引数なし）。
    pub args: String,
    /// 補完候補に出す説明。
    pub summary: String,
    /// この variant で起動したとき、実行前に**確認フェーズを通す**か（#400）。
    ///
    /// `None` は従来どおり（最初から全ツール）。`PlanFirst` は「質問カード → 計画カード →
    /// 承認 → 実行」を**ツール提示で保証する**（instructions の門は実測で確率的にしか効かず、
    /// 同じ指示で止まる run と素通りする run が出た）。判定は chat 側の
    /// `crate::worker::gate` にあり、ここは skill が意図を宣言する場所。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<CommandPhase>,
}

/// コマンド variant の実行前フェーズ（閉じた集合・#400）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CommandPhase {
    /// 計画の承認を取ってから実行する（承認までは調査系ツールを提示しない）。
    PlanFirst,
}

/// スラッシュコマンド宣言の検証（#387）。
///
/// 名前は**スラグに限定**する。空白や `/` を許すとコンポーザのパースが曖昧になり、
/// 記号を許すと補完一覧での視覚的な詐称（他コマンドへの偽装）を招く。
/// 長さ上限も同じ理由（一覧を占有させない）。
pub(crate) fn validate_command(command: &SkillCommand, errors: &mut Vec<GuiValidationError>) {
    // **trim しない生値**を検証する。カタログへ載り本文へ連結されるのは生値であり、
    // trim 後に判定すると前後の空白/改行を含む値が通ってしまう（この関数が防ごうとしている
    // 「視覚的な詐称」「発話が壊れる」をすり抜ける）。
    let name = command.name.as_str();
    let valid_slug = !name.is_empty()
        && name.len() <= skill_limits::MAX_COMMAND_NAME_CHARS
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid_slug {
        errors.push(
            GuiValidationError::new(
                "skill.invalid_command_name",
                format!(
                    "command.name は英小文字・数字・ハイフンのみ（先頭は英数字・最大 {} 文字）",
                    skill_limits::MAX_COMMAND_NAME_CHARS
                ),
            )
            .at("command.name"),
        );
    }
    if command
        .hint
        .as_ref()
        .is_some_and(|h| h.chars().count() > skill_limits::MAX_COMMAND_TEXT_CHARS)
    {
        errors.push(
            GuiValidationError::new("skill.too_long", "command.hint が長すぎます")
                .at("command.hint"),
        );
    }
    if command.variants.len() > skill_limits::MAX_COMMAND_VARIANTS {
        errors.push(
            GuiValidationError::new(
                "skill.too_many_command_variants",
                format!(
                    "command.variants は最大 {} 件",
                    skill_limits::MAX_COMMAND_VARIANTS
                ),
            )
            .at("command.variants"),
        );
    }
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (i, v) in command.variants.iter().enumerate() {
        let args = v.args.as_str();
        // 引数は本文へそのまま連結されるため、改行を許すと発話が壊れる。
        // 前後の空白も拒否する（`"auto "` と `"auto"` が別コマンドに見え、連結でも崩れる）。
        if args.contains(['\n', '\r'])
            || args != args.trim()
            || args.chars().count() > skill_limits::MAX_COMMAND_TEXT_CHARS
        {
            errors.push(
                GuiValidationError::new(
                    "skill.invalid_command_args",
                    "command.variants[].args が不正です",
                )
                .at(format!("command.variants[{i}].args")),
            );
        }
        if v.summary.trim().is_empty()
            || v.summary.chars().count() > skill_limits::MAX_COMMAND_TEXT_CHARS
        {
            errors.push(
                GuiValidationError::new(
                    "skill.invalid_command_summary",
                    "command.variants[].summary は必須（長すぎないこと）",
                )
                .at(format!("command.variants[{i}].summary")),
            );
        }
        if !seen.insert(args) {
            errors.push(
                GuiValidationError::new(
                    "skill.duplicate_command_variant",
                    "command.variants の args が重複しています",
                )
                .at(format!("command.variants[{i}].args")),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(variants: &[(&str, Option<CommandPhase>)]) -> SkillCommand {
        SkillCommand {
            name: "deep-research".into(),
            hint: None,
            variants: variants
                .iter()
                .map(|(args, phase)| SkillCommandVariant {
                    args: (*args).into(),
                    summary: "s".into(),
                    phase: *phase,
                })
                .collect(),
        }
    }

    /// 既定 variant（引数なし）と `auto` の 2 択。`auto` だけが門を持たない。
    fn deep_research() -> SkillCommand {
        command(&[("", Some(CommandPhase::PlanFirst)), ("auto", None)])
    }

    /// 起動 variant → 宣言フェーズ（テストの読みやすさのための小道具）。
    fn phase_of(c: &SkillCommand, text: &str) -> Option<CommandPhase> {
        c.variant_for_invocation(text).and_then(|v| v.phase)
    }

    #[test]
    fn resolves_default_variant() {
        let c = deep_research();
        assert_eq!(
            phase_of(&c, "/deep-research リモートワークの生産性"),
            Some(CommandPhase::PlanFirst)
        );
        // 依頼本文なしの起動も同じ variant。
        assert_eq!(
            phase_of(&c, "/deep-research"),
            Some(CommandPhase::PlanFirst)
        );
    }

    #[test]
    fn longest_args_win() {
        // `""` が先に宣言されていても `auto` を先に当てる（最長一致）。
        let c = deep_research();
        assert_eq!(phase_of(&c, "/deep-research auto 調べて"), None);
        assert_eq!(phase_of(&c, "/deep-research auto"), None);
    }

    #[test]
    fn args_must_end_at_a_boundary() {
        // `auto` は `automatic` に当たらない（＝既定 variant として解ける）。
        let c = deep_research();
        assert_eq!(
            phase_of(&c, "/deep-research automatic なんとか"),
            Some(CommandPhase::PlanFirst)
        );
    }

    #[test]
    fn other_commands_do_not_match() {
        let c = deep_research();
        // 別コマンド・前方一致だけの名前・コマンドでない発話はすべて不一致。
        assert_eq!(phase_of(&c, "/other 調べて"), None);
        assert_eq!(phase_of(&c, "/deep-research-x 調べて"), None);
        assert_eq!(phase_of(&c, "リモートワークについて"), None);
    }

    #[test]
    fn phase_is_none_without_declaration() {
        // variant が phase を宣言していなければ門は掛からない（既存 skill の互換）。
        let c = command(&[("", None)]);
        assert_eq!(phase_of(&c, "/deep-research 調べて"), None);
        // variants が空（＝引数なし 1 択）も同じ。
        let c = command(&[]);
        assert_eq!(phase_of(&c, "/deep-research 調べて"), None);
    }
}
