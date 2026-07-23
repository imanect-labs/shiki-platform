//! 自律エージェントの承認 3 モード（#350）。
//!
//! Claude Code の権限モード（default / acceptEdits / bypassPermissions）に対応する 3 値を
//! thread 単位で持ち、既定は**承認必須**。モード→[`ApprovalPolicy`] の写像と、実行時の
//! 実効モード決定（org キャップ・設定者と実行者の一致検査）は**このファイルに集約**する。
//!
//! 不変条件:
//! - skill はモード（承認ポリシ）を緩められない（`opts.approval` に触れない・Task 6.9 の
//!   `apply_never_relaxes_approval_policy` を維持）。モード変更はユーザー（API）と org のみ。
//! - **緩和は実行者本人の同意に限る**: run はメッセージ投入時点のモードをスナップショットし
//!   （発話者はモードを見て投稿する＝同意）、実行中のモード変更は「厳格化は誰でも・緩和は
//!   run の actor が設定した場合のみ」有効（共有スレッドの別編集者が他人の権限で走る run の
//!   承認を緩められない・confused-deputy 防御）。
//! - org キャップ（`tenant.allow_autonomous_bypass=false`）下では bypass を選べない。API は
//!   明示エラーで弾き、実行時に残っていた bypass は承認必須へクランプして警告イベントを出す
//!   （黙って実行を続けない・黙って降格もしない）。

use agent_core::{ApprovalPolicy, ToolName};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 自律 run の承認モード（thread 単位・実行中トグル可・#350）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutonomousMode {
    /// 承認必須（既定）: 全ての破壊系ツールが承認カードで止まる（= `deny_all` 相当）。
    #[default]
    RequireApproval,
    /// オート: 版管理で復元可能な書込のみ自動承認。不可逆・高影響（fs_delete / shell /
    /// office.live_edit）は承認を維持する。
    Auto,
    /// 全自動（危険）: 全ての要確認ツールを自動承認（= `allow_all`）。明示オプトインで、
    /// org 管理者ポリシで禁止できる。
    Bypass,
}

impl AutonomousMode {
    /// DB / API で共通の文字列表現。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AutonomousMode::RequireApproval => "require_approval",
            AutonomousMode::Auto => "auto",
            AutonomousMode::Bypass => "bypass",
        }
    }

    /// 文字列から閉集合へ（未知は None・fail-closed）。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "require_approval" => Some(AutonomousMode::RequireApproval),
            "auto" => Some(AutonomousMode::Auto),
            "bypass" => Some(AutonomousMode::Bypass),
            _ => None,
        }
    }

    /// 許可の広さ（順序: 承認必須 < オート < 全自動）。実行中トグルの緩和/厳格化判定に使う。
    const fn permissiveness(self) -> u8 {
        match self {
            AutonomousMode::RequireApproval => 0,
            AutonomousMode::Auto => 1,
            AutonomousMode::Bypass => 2,
        }
    }

    /// モード→承認ポリシの写像（**単一の定義箇所**・#350）。
    ///
    /// read-only（web_search / web_fetch / doc_search）はどのモードでも止まらない（#350 決定）。
    /// doc_search 等は `requires_confirmation=false` で素通りし、egress の 2 つは自律版の
    /// 承認ゲート対象（Task 5.6）のため全モードで事前許可に含める。
    #[must_use]
    pub fn approval_policy(self) -> ApprovalPolicy {
        /// 全モード共通の事前許可（read-only egress・#350 決定）。
        const READ_ONLY: [ToolName; 2] = [ToolName::WebSearch, ToolName::WebFetch];
        /// オートで自動承認する「版管理で復元可能な書込」。不可逆・高影響（fs_delete / shell /
        /// office.live_edit=開いているセッションへの即時注入）は含めない。
        const VERSIONED_WRITES: [ToolName; 7] = [
            ToolName::FsWrite,
            ToolName::FsEdit,
            ToolName::DocumentEdit,
            ToolName::SlideEdit,
            ToolName::CsvPatch,
            ToolName::CsvWrite,
            ToolName::OfficeEdit,
        ];
        match self {
            AutonomousMode::RequireApproval => {
                ApprovalPolicy::auto(READ_ONLY.iter().map(|t| t.as_str().to_string()))
            }
            AutonomousMode::Auto => ApprovalPolicy::auto(
                READ_ONLY
                    .iter()
                    .chain(VERSIONED_WRITES.iter())
                    .map(|t| t.as_str().to_string()),
            ),
            AutonomousMode::Bypass => ApprovalPolicy::allow_all(),
        }
    }
}

/// 実効モードのクランプ理由（黙って降格せず、警告イベント/エラーで明示する・#350）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeClamp {
    /// org 管理者ポリシで bypass（全自動）が禁止されている。
    OrgBypassForbidden,
    /// 実行中の緩和が run の actor 以外による設定だったため、スナップショットへ戻した。
    RelaxedByOtherUser,
}

impl ModeClamp {
    /// UI へ出す説明（FailureRecovery イベントの detail）。
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            ModeClamp::OrgBypassForbidden => {
                "全自動（bypass）モードは組織ポリシで禁止されているため、承認必須で実行します。\
                 モードを変更してください。"
            }
            ModeClamp::RelaxedByOtherUser => {
                "承認モードの緩和は実行者本人の設定のみ有効です。この実行は投稿時のモードで続行します。"
            }
        }
    }
}

/// 実効モードを決める純関数（#350・run 開始時と実行中の各承認判定で共通に使う）。
///
/// - `snapshot`: メッセージ投入時点のモード（発話者が同意した水準）。
/// - `current` / `set_by`: thread の現在モードとその設定者（実行中トグル）。
/// - 厳格化（current が snapshot より狭い）は誰の設定でも即時有効。
/// - 緩和は `set_by == actor` の場合のみ有効（他人が緩めても actor の run には効かない）。
/// - bypass は `bypass_allowed=false` なら承認必須へクランプ（fail-closed）。
#[must_use]
pub fn effective_mode(
    snapshot: AutonomousMode,
    current: AutonomousMode,
    set_by: Option<&str>,
    actor: &str,
    bypass_allowed: bool,
) -> (AutonomousMode, Option<ModeClamp>) {
    let mut clamp = None;
    let mut mode = if current.permissiveness() <= snapshot.permissiveness() || set_by == Some(actor)
    {
        current
    } else {
        clamp = Some(ModeClamp::RelaxedByOtherUser);
        snapshot
    };
    if mode == AutonomousMode::Bypass && !bypass_allowed {
        mode = AutonomousMode::RequireApproval;
        clamp = Some(ModeClamp::OrgBypassForbidden);
    }
    (mode, clamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_roundtrip_and_default() {
        for m in [
            AutonomousMode::RequireApproval,
            AutonomousMode::Auto,
            AutonomousMode::Bypass,
        ] {
            assert_eq!(AutonomousMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(AutonomousMode::parse("bogus"), None);
        // 既定は承認必須（#350 の決定・オート固定からの転換）。
        assert_eq!(AutonomousMode::default(), AutonomousMode::RequireApproval);
    }

    #[test]
    fn require_approval_gates_all_destructive_tools() {
        let p = AutonomousMode::RequireApproval.approval_policy();
        for tool in [
            "fs_write",
            "fs_edit",
            "fs_delete",
            "shell",
            "document.edit",
            "slide.edit",
            "csv.patch",
            "csv.write",
            "office.edit",
            "office.live_edit",
        ] {
            assert!(!p.is_pre_authorized(tool), "{tool} は承認必須で止まること");
        }
        // read-only（egress）はどのモードでも止まらない（#350 決定）。
        assert!(p.is_pre_authorized("web_search"));
        assert!(p.is_pre_authorized("web_fetch"));
    }

    #[test]
    fn auto_allows_versioned_writes_but_keeps_irreversible_gated() {
        let p = AutonomousMode::Auto.approval_policy();
        for tool in [
            "fs_write",
            "fs_edit",
            "document.edit",
            "slide.edit",
            "csv.patch",
            "csv.write",
            "office.edit",
        ] {
            assert!(p.is_pre_authorized(tool), "{tool} はオートで自動承認");
        }
        for tool in ["fs_delete", "shell", "office.live_edit"] {
            assert!(!p.is_pre_authorized(tool), "{tool} は不可逆のため承認維持");
        }
    }

    #[test]
    fn bypass_allows_everything() {
        let p = AutonomousMode::Bypass.approval_policy();
        assert!(p.is_pre_authorized("shell"));
        assert!(p.is_pre_authorized("fs_delete"));
    }

    #[test]
    fn effective_mode_honors_tightening_by_anyone() {
        // 他人による厳格化は即時有効（安全側は誰でも締められる）。
        let (m, clamp) = effective_mode(
            AutonomousMode::Bypass,
            AutonomousMode::RequireApproval,
            Some("bob"),
            "alice",
            true,
        );
        assert_eq!(m, AutonomousMode::RequireApproval);
        assert_eq!(clamp, None);
    }

    #[test]
    fn effective_mode_rejects_relaxation_by_other_user() {
        // 他人による緩和はスナップショットへ戻す（confused-deputy 防御）。
        let (m, clamp) = effective_mode(
            AutonomousMode::RequireApproval,
            AutonomousMode::Bypass,
            Some("bob"),
            "alice",
            true,
        );
        assert_eq!(m, AutonomousMode::RequireApproval);
        assert_eq!(clamp, Some(ModeClamp::RelaxedByOtherUser));
        // 本人による緩和は有効。
        let (m, clamp) = effective_mode(
            AutonomousMode::RequireApproval,
            AutonomousMode::Auto,
            Some("alice"),
            "alice",
            true,
        );
        assert_eq!(m, AutonomousMode::Auto);
        assert_eq!(clamp, None);
    }

    #[test]
    fn effective_mode_clamps_bypass_when_org_forbids() {
        // org キャップ下の bypass は承認必須へクランプ＋理由を返す（黙って降格しない）。
        let (m, clamp) = effective_mode(
            AutonomousMode::Bypass,
            AutonomousMode::Bypass,
            Some("alice"),
            "alice",
            false,
        );
        assert_eq!(m, AutonomousMode::RequireApproval);
        assert_eq!(clamp, Some(ModeClamp::OrgBypassForbidden));
    }
}
