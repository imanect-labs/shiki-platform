//! 自動実行（schedule / event）の起動を見送った理由と、その警告・記録（BUG-146・BUG-51・BUG-170・GAP-67）。
//!
//! - 警告: 見送りは何度起こしても同じ結果になる（再試行しない）ので、発火のたびに警告するとログを埋める。
//!   (理由・テナント・ワークフロー・有効化した版) ごとにプロセスで 1 回にし、2 回目からは debug に落とす。
//! - 記録: 作成者が自動化の止まったことに気づけるよう、ワークフローごとに 1 行（`workflow_launch_skip`・migration
//!   0143）に、理由の code と作成者向けの文・回数・最初と最後の時刻を残す。自動実行が run を作れたら消し、オンに
//!   し直す・オフにすると消す。登録の API（`GET /workflows/{id}/registration`）が返し、ワークフローの画面が出す。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
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

    /// code から理由を読む（記録の行・知らない code は `None`）。
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.code() == code)
    }

    /// 作成者向けの理由の文（内部の文・ほかの人の情報を出さない）。`version` は見送った有効化した版。
    #[must_use]
    pub fn message(self, version: i64) -> String {
        match self {
            SkipCause::DefinitionUnreadable => {
                "自動実行のとき、このワークフローを読み込めませんでした。".to_string()
            }
            // 誰の、どの権限が外れたかは言わない（ほかの人の権限の状態を出さない・付録 C の P5）。
            SkipCause::DelegationInvalid => {
                "オンにした人の権限が外れたか、使ってよい操作が増えました。".to_string()
            }
            SkipCause::NotReady => {
                format!("オンにしている v{version} に、空の欄など実行できない設定があります。")
            }
        }
    }

    /// 直し方の文（画面の名前で言う・付録 C の P4）。
    #[must_use]
    pub const fn fix(self) -> &'static str {
        match self {
            SkipCause::DefinitionUnreadable | SkipCause::DelegationInvalid => FIX_REENABLE,
            SkipCause::NotReady => {
                "エディターで設定を直して保存してから、「自動実行の設定」でもう一度オンにしてください。"
            }
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

/// オンにし直すと直る見送りの直し方（知らない code の行にも使う）。
const FIX_REENABLE: &str = "「自動実行の設定」を開いて、もう一度オンにしてください。";

/// 記録した見送り（ワークフローごとに 1 行・続く見送りの回数と時刻）。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LaunchSkip {
    /// 理由の code（[`SkipCause::code`]）。
    pub code: String,
    /// 作成者向けの理由の文（記録したときの文）。
    pub message: String,
    /// 見送った有効化した版。
    pub version: i64,
    /// 続く見送りの最初の時刻。
    pub first_at: DateTime<Utc>,
    /// 最後に見送った時刻。
    pub last_at: DateTime<Utc>,
    /// 続けて見送った回数。
    pub count: i64,
}

impl LaunchSkip {
    /// 直し方の文（code から組む・知らない code はオンにし直すよう言う）。
    #[must_use]
    pub fn fix(&self) -> &'static str {
        SkipCause::parse(&self.code).map_or(FIX_REENABLE, SkipCause::fix)
    }
}

/// 見送りを記録する。同じワークフローの続く見送りは 1 行にまとめ、回数と最後の時刻を進める（版か理由が変わったら
/// 数え直す）。登録の行が無ければ書かない（外部キー・見送りは登録のある自動実行にしか起きない）。
pub(crate) async fn record(
    db: &PgPool,
    tenant_id: &str,
    workflow_id: Uuid,
    version: i64,
    cause: SkipCause,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO workflow_launch_skip (tenant_id, workflow_id, version, code, message) \
         SELECT $1, $2, $3, $4, $5 WHERE EXISTS ( \
             SELECT 1 FROM workflow_registration WHERE tenant_id = $1 AND workflow_id = $2) \
         ON CONFLICT (tenant_id, workflow_id) DO UPDATE SET \
             count = CASE WHEN workflow_launch_skip.version = EXCLUDED.version \
                           AND workflow_launch_skip.code = EXCLUDED.code \
                          THEN workflow_launch_skip.count + 1 ELSE 1 END, \
             first_at = CASE WHEN workflow_launch_skip.version = EXCLUDED.version \
                              AND workflow_launch_skip.code = EXCLUDED.code \
                             THEN workflow_launch_skip.first_at ELSE now() END, \
             version = EXCLUDED.version, code = EXCLUDED.code, message = EXCLUDED.message, \
             last_at = now()",
    )
    .bind(tenant_id)
    .bind(workflow_id)
    .bind(version)
    .bind(cause.code())
    .bind(cause.message(version))
    .execute(db)
    .await?;
    Ok(())
}

/// 見送りの記録を消す（自動実行が run を作れた・オンにし直した・オフにした）。
pub(crate) async fn clear<'e, E>(
    db: E,
    tenant_id: &str,
    workflow_id: Uuid,
) -> Result<(), sqlx::Error>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query("DELETE FROM workflow_launch_skip WHERE tenant_id = $1 AND workflow_id = $2")
        .bind(tenant_id)
        .bind(workflow_id)
        .execute(db)
        .await?;
    Ok(())
}

/// 記録が「いま見送り続けている」ものか（`s` = `workflow_launch_skip`・`r` = 同じワークフローの
/// `workflow_registration`）。
///
/// 記録した版がいまオンにしている版で、自動実行がオフでないものだけ（オフにした後・別の版をオンにした後の古い記録は
/// 見せない）。登録の API（[`current`]）とワークフローの一覧（`summary.rs`）が同じ条件で引く。
pub(crate) const CURRENT_SKIP: &str = "r.status <> 'disabled' AND r.enabled_version = s.version";

/// いま見送り続けている自動実行の記録（無ければ `None`・[`CURRENT_SKIP`]）。
pub(crate) async fn current(
    db: &PgPool,
    tenant_id: &str,
    workflow_id: Uuid,
) -> Result<Option<LaunchSkip>, sqlx::Error> {
    sqlx::query_as::<_, LaunchSkip>(&format!(
        "SELECT s.code, s.message, s.version, s.first_at, s.last_at, s.count \
         FROM workflow_launch_skip s \
         JOIN workflow_registration r \
           ON r.tenant_id = s.tenant_id AND r.workflow_id = s.workflow_id \
         WHERE s.tenant_id = $1 AND s.workflow_id = $2 AND {CURRENT_SKIP}"
    ))
    .bind(tenant_id)
    .bind(workflow_id)
    .fetch_optional(db)
    .await
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

    /// 理由の code は記録の表の check と同じ集合で、文は内部の言葉（委譲・コード）を出さない（GAP-67・付録 C の P1）。
    #[test]
    fn causes_match_the_table_and_speak_plainly() {
        let migration = include_str!("../../../../migrations/0144_workflow_launch_skip.sql");
        let check = migration
            .split("check (code in (")
            .nth(1)
            .and_then(|rest| rest.split("))").next())
            .unwrap();
        let listed: Vec<&str> = check
            .split(',')
            .map(|c| c.trim().trim_matches('\''))
            .collect();
        let codes: Vec<&str> = SkipCause::ALL.iter().map(|c| c.code()).collect();
        assert_eq!(listed, codes);
        for cause in SkipCause::ALL {
            assert_eq!(SkipCause::parse(cause.code()), Some(*cause));
            for text in [cause.message(3), cause.fix().to_string()] {
                assert!(
                    !text.contains("委譲") && !text.contains(cause.code()) && text.ends_with('。'),
                    "{cause:?}: {text}"
                );
            }
        }
        assert!(SkipCause::NotReady.message(3).contains("v3"));
        assert_eq!(SkipCause::parse("never-seen"), None);
        let unknown = LaunchSkip {
            code: "never-seen".into(),
            message: String::new(),
            version: 1,
            first_at: Utc::now(),
            last_at: Utc::now(),
            count: 1,
        };
        assert_eq!(unknown.fix(), FIX_REENABLE);
    }

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
