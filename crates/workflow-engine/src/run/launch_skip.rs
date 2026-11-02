//! 自動実行（schedule / event）の起動を見送った理由と、その警告を版ごとに 1 回にする覚え（BUG-146・BUG-51・BUG-170）。
//!
//! 見送りは何度起こしても同じ結果になる（再試行しない）ので、発火のたびに警告するとログを埋める。警告は
//! (理由・テナント・ワークフロー・有効化した版) ごとにプロセスで 1 回にし、2 回目からは debug に落とす。

use uuid::Uuid;

/// 自動実行の起動を見送った理由（作成者が直せるもの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipCause {
    /// 自動実行の主体が、自分の定義を読めない（登録と認可の食い違い・BUG-146）。
    DefinitionUnreadable,
    /// 委譲の不成立（オンにした人の権限が外れた・使ってよい操作が増えた・台帳が読めない・BUG-170）。
    DelegationInvalid,
    /// 有効化した版に実行の前に止める設定がある（必須の欄が空・中にブロックの無い繰り返し・BUG-51）。
    NotReady,
}

impl SkipCause {
    /// 全理由（表示・記録の照合に使う）。
    pub const ALL: &'static [SkipCause] = &[
        SkipCause::DefinitionUnreadable,
        SkipCause::DelegationInvalid,
        SkipCause::NotReady,
    ];

    /// 理由の code。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            SkipCause::DefinitionUnreadable => "definition_unreadable",
            SkipCause::DelegationInvalid => "delegation_invalid",
            SkipCause::NotReady => "not_ready",
        }
    }

    /// サーバーのログの文（作成者向けの文ではない）。
    const fn log_message(self) -> &'static str {
        match self {
            SkipCause::DefinitionUnreadable => {
                "自動実行の主体がワークフローの定義を読めないため run を開始しません（自動実行をオンにし直すと直ります。\
                 以後この版の発火では警告しません）"
            }
            SkipCause::DelegationInvalid => {
                "委譲チェック不成立のため run を開始しません（自動実行をオンにし直すと直ります。以後この版の発火では\
                 警告しません）"
            }
            SkipCause::NotReady => {
                "自動実行の版に、実行の前に止める設定があるため run を開始しません（欄を埋めて自動実行をオンにし直すと\
                 直ります。以後この版の発火では警告しません）"
            }
        }
    }
}

/// 覚えの鍵（理由・テナント・ワークフロー・有効化した版）。
type WarnKey = (SkipCause, String, Uuid, i64);

/// 同じ見送りの警告をプロセスで 1 回にする覚え。覚える数が上限に届いたら忘れて、また 1 回ずつ警告する。
#[derive(Debug, Default)]
pub(crate) struct SkipWarnings(std::sync::Mutex<std::collections::HashSet<WarnKey>>);

impl SkipWarnings {
    /// 覚える数の上限。
    const MAX: usize = 10_000;

    /// はじめての見送りなら覚えて `true`（警告する）。覚えていれば `false`。
    fn first(&self, cause: SkipCause, tenant_id: &str, workflow_id: Uuid, version: i64) -> bool {
        self.0.lock().is_ok_and(|mut seen| {
            if seen.len() >= Self::MAX {
                seen.clear();
            }
            seen.insert((cause, tenant_id.to_string(), workflow_id, version))
        })
    }

    /// 見送りを警告する（同じ理由・版の 2 回目からは debug）。`reason` はログ用の詳しい理由（作成者には見せない）。
    pub(crate) fn warn(
        &self,
        cause: SkipCause,
        tenant_id: &str,
        workflow_id: Uuid,
        version: i64,
        reason: &str,
    ) {
        if self.first(cause, tenant_id, workflow_id, version) {
            tracing::warn!(tenant_id, %workflow_id, version, reason, code = cause.code(), "{}", cause.log_message());
        } else {
            tracing::debug!(tenant_id, %workflow_id, version, reason, code = cause.code(), "自動実行の起動を見送りました");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 見送りの警告は、(理由・テナント・ワークフロー・版) ごとに 1 回（BUG-146・BUG-51・BUG-170）。
    #[test]
    fn skips_are_warned_once_per_cause_and_version() {
        let seen = SkipWarnings::default();
        let wf = Uuid::new_v4();
        for cause in SkipCause::ALL {
            assert!(seen.first(*cause, "t", wf, 1), "{cause:?}");
            assert!(
                !seen.first(*cause, "t", wf, 1),
                "{cause:?}: 同じ版の 2 回目の発火では警告しない"
            );
            assert!(
                seen.first(*cause, "t", wf, 2),
                "{cause:?}: オンにし直した別の版は警告する"
            );
            assert!(seen.first(*cause, "u", wf, 1), "{cause:?}: 別のテナント");
        }
        // 上限に届いたら忘れる（覚えが際限なく増えない）。
        for i in 0..SkipWarnings::MAX {
            seen.first(
                SkipCause::DelegationInvalid,
                "x",
                Uuid::new_v4(),
                i64::try_from(i).unwrap_or(0),
            );
        }
        assert!(seen.0.lock().unwrap().len() <= SkipWarnings::MAX);
    }
}
