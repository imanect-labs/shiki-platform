//! 承認ゲート（Task 5.6）。
//!
//! 破壊系/egress/高コスト操作（`requires_confirmation()`）は実行前に停止し、ユーザーの承認または
//! **事前許可**（auto-approve ルール／対象スコープ限定）を待つ。承認待ちはエージェントを**ブロック**し、
//! SSE 経由で UI へ承認要求を出す（chat 側の [`Approver`] 実装が run を `waiting_approval` にして待つ）。
//! 承認/却下の全判定は監査へ流す（5.10）。
//!
//! agent-core は「いつ承認が要るか」（[`ApprovalPolicy`]）と「どう待つか」（[`Approver`] トレイト）を
//! 分離する。決定は I/O を伴うため [`Approver`] は shiki-server 側で実装し、テストはフェイクを差す。

use std::collections::HashSet;

use async_trait::async_trait;

/// 事前許可ポリシ（どのツールを自動承認するか）。
///
/// 既定は「全て要承認」（`auto_approve_all=false`＋空集合）。Chat プロファイルは破壊系ツールを
/// 提示しないため実質無効。自律版は skill/組織ポリシで `auto_approve` にツール名を並べて緩める。
#[derive(Debug, Clone, Default)]
pub struct ApprovalPolicy {
    /// これらのツール名は承認なしで自動実行する（対象スコープ限定の事前許可）。
    pub auto_approve: HashSet<String>,
    /// true で全 `requires_confirmation` ツールを自動承認する（旧 `allow_confirmed_tools` 互換）。
    pub auto_approve_all: bool,
}

impl ApprovalPolicy {
    /// 全て要承認（既定）。
    #[must_use]
    pub fn deny_all() -> Self {
        ApprovalPolicy::default()
    }

    /// 全て自動承認（信頼済みバッチ・旧 `allow_confirmed_tools=true` 互換）。
    #[must_use]
    pub fn allow_all() -> Self {
        ApprovalPolicy {
            auto_approve: HashSet::new(),
            auto_approve_all: true,
        }
    }

    /// 指定ツール名だけ自動承認する。
    #[must_use]
    pub fn auto(names: impl IntoIterator<Item = String>) -> Self {
        ApprovalPolicy {
            auto_approve: names.into_iter().collect(),
            auto_approve_all: false,
        }
    }

    /// このツールが事前許可済み（承認をスキップしてよい）か。
    #[must_use]
    pub fn is_pre_authorized(&self, tool_name: &str) -> bool {
        self.auto_approve_all || self.auto_approve.contains(tool_name)
    }
}

/// 承認要求への決定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// 承認（実行してよい）。
    Approved,
    /// 却下（実行しない・観測としてモデルへ戻す）。
    Rejected,
    /// 承認待ち中にキャンセルされた（run を停止する）。
    Cancelled,
}

/// 承認要求を人間（or 事前設定）へ問い合わせて決定を待つ差し替え点。
///
/// chat 実装は run を `waiting_approval` にし、SSE で UI へ要求を出し、API 経由の決定 or キャンセルを
/// 待って返す（その間ハートビートがリースを延長する）。テストはフェイクを差す。
#[async_trait]
pub trait Approver: Send + Sync {
    /// `tool_call_id` のツール呼び出しについて決定を待つ。
    async fn decide(
        &self,
        tool_call_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> ApprovalDecision;

    /// **現在の**実効承認ポリシを返す（実行中のモードトグル対応・#350）。
    ///
    /// `Some` を返すと承認ゲートは `opts.approval`（run 開始時のスナップショット）ではなく
    /// この値で事前許可を判定する（各破壊系呼び出しの直前に問い直すため、緩和/厳格化の両方向が
    /// 実行中に効く）。既定 `None` = スナップショットのまま（既存実装・テストは無変更）。
    async fn current_policy(&self) -> Option<ApprovalPolicy> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_pre_authorization() {
        assert!(!ApprovalPolicy::deny_all().is_pre_authorized("shell"));
        assert!(ApprovalPolicy::allow_all().is_pre_authorized("shell"));
        let p = ApprovalPolicy::auto(["fs_write".to_string()]);
        assert!(p.is_pre_authorized("fs_write"));
        assert!(!p.is_pre_authorized("shell"));
    }
}
