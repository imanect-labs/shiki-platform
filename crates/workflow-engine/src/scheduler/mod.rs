//! トリガ: スケジューラ＋イベントマッチング（Task 10.3・engine.md §5）。
//!
//! - `leader`: リーダーリース（単一スケジューラループ・多重発火防止）。
//! - `cron`: cron 評価（5 フィールド＋IANA tz・watermark 前進）。
//! - `store`: occurrence 冪等記録＋イベントトリガの候補の照合。
//! - `firing`: trigger_firing の占有・抑止（因果の鎖・queued 上限）・起動・一時失敗の再試行。
//! - `coalesce`: 同じファイルの続く保存を 1 回にまとめる発火（新しい版の保存・D-38）。
//!
//! run 起動は [`RunLauncher`] トレイト裏（実装は delegation チェック＋IR 取得＋create_run を束ねる）。

pub mod coalesce;
pub mod cron;
pub mod firing;
pub mod leader;
pub mod store;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

pub use coalesce::{Coalesce, COALESCE_MAX_SECS, COALESCE_QUIET_SECS};
pub use firing::{
    cause_run_of_trace, EventTriggerRef, FireOutcome, GaveUpFiring, RetryReport,
    MAX_LAUNCH_ATTEMPTS, MAX_QUEUED_RUNS, MAX_TRIGGER_CHAIN,
};
pub use leader::LeaderLease;
pub use store::{DataTriggerCandidate, FileTriggerCandidate, SchedulerStore, SchedulerStoreError};

/// トリガ発火から run を起動した結果（D-21・engine.md §5.4）。
///
/// 呼び出し側（スケジューラ・イベントのマッチャ）はこれで「再試行するか」を決める。一時的な失敗
/// （IR の取得・DB・OpenFGA の障害）だけを再試行し、委譲の不成立や多重度あふれによる見送りは
/// 再試行しない（何度起こしても同じ結果になる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// run を作った。同じイベントで作った run が既にあれば、その run_id（二重に作らない）。
    Started(Uuid),
    /// 起動しない（再試行しない）。
    Skipped(SkipReason),
    /// 一時的な失敗（再試行する）。理由はログ・監査用。
    Failed(String),
}

/// 起動を見送った理由（[`LaunchOutcome::Skipped`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `policies.on_trigger_overflow = skip` による多重度あふれ。
    Overflow,
    /// 有効化されていない（未登録・無効化済み・有効化した版が無い）。
    NotEnabled,
    /// 委譲の不成立（失効・未同意のスコープなど）。自動実行の主体が定義を読めない（BUG-146）と、有効化した版に実行の
    /// 前に止める設定がある（必須の欄が空など・BUG-51）ときも、同じく理由の文を添えてこれで見送る。
    DelegationInvalid(String),
}

impl LaunchOutcome {
    /// 起動した run（見送り・失敗は `None`）。
    #[must_use]
    pub fn run_id(&self) -> Option<Uuid> {
        match self {
            LaunchOutcome::Started(id) => Some(*id),
            LaunchOutcome::Skipped(_) | LaunchOutcome::Failed(_) => None,
        }
    }
}

/// トリガ発火から run を起動する委譲先（delegation チェック＋IR 取得＋create_run）。
///
/// schedule/event の run は workflow プリンシパルで実行し、run 開始時委譲チェックを通る
/// （engine.md §6.2）。委譲不成立なら [`LaunchOutcome::Skipped`]（occurrence・firing は記録済みで
/// 再発火しない）。
#[async_trait]
pub trait RunLauncher: Send + Sync {
    /// 指定ワークフローの run を起動する。
    ///
    /// `payload` はトリガペイロード。event はイベントペイロードで、run の入力に載せ、`$from trigger`/`$from input`
    /// で参照できるようにする（engine.md §6.1）。schedule は予定の時刻だけを持つ [`schedule_payload`] で、run の入力
    /// には載せず（`$from trigger`/`$from input` は今までどおり null）、実行情報の予定の日時（`$from run` の
    /// `scheduled_at`）にする（[`scheduled_at_of`] で読む・GAP-15）。
    ///
    /// `event_id` は発火元の outbox イベント（schedule は `None`）。`(trigger_id, event_id)` で run を
    /// 一意にするので、クラッシュ後の再配送・再試行で同じイベントから run を二重に作らない。
    async fn launch(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
        trigger_id: &str,
        payload: &Value,
        event_id: Option<i64>,
    ) -> LaunchOutcome;
}

/// スケジュールの発火が起動口（[`RunLauncher::launch`] の `payload`）へ渡す値（予定の時刻だけ・GAP-15）。
///
/// 起動口は [`scheduled_at_of`] で読み、run の行の予定の時刻にする（run の入力には載せない）。形はここだけで決める。
#[must_use]
pub fn schedule_payload(scheduled_at: DateTime<Utc>) -> Value {
    json!({ "scheduled_at": scheduled_at.to_rfc3339() })
}

/// [`schedule_payload`] の予定の時刻（無い・読めなければ `None`。そのときの実行情報の予定の日時は始めた時刻）。
#[must_use]
pub fn scheduled_at_of(payload: &Value) -> Option<DateTime<Utc>> {
    let at = payload.get("scheduled_at")?.as_str()?;
    DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 予定の時刻は、渡した値と読んだ値が同じ（秒より細かい部分も落とさない）。形の違う値は読まない。
    #[test]
    fn schedule_payload_roundtrips() {
        let at = DateTime::parse_from_rfc3339("2026-11-09T00:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(scheduled_at_of(&schedule_payload(at)), Some(at));
        assert_eq!(scheduled_at_of(&Value::Null), None);
        assert_eq!(scheduled_at_of(&json!({ "scheduled_at": "monday" })), None);
        assert_eq!(scheduled_at_of(&json!({ "scheduled_at": 1 })), None);
    }
}
