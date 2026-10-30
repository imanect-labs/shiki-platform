//! 試す回数を使わずに待ってやり直す失敗（取り込み待ち・D-125・GAP-50）。
//!
//! 「ファイルを読む」で PDF・Office の本文の取り込みが終わっていない（`index_pending`）とき、step を失敗にせず ready に
//! 戻して、間隔を伸ばしながら読み直す（[`crate::retry::WaitPolicy`]）。試す回数（attempt）は使わない（claim の +1 を
//! 打ち消す）。待ち始めた時刻（`wait_since`）から上限を過ぎたら、呼び出し側が同じ誤りで失敗にする。手動の実行・
//! 自動実行のどちらにも効く（ファイルが保存されたときのきっかけは、作ったファイルですぐ実行する）。
//!
//! 待つかどうかは code だけで決める（[`crate::retry::wait_policy`]）。script の `Shiki.*` が投げ返した誤りも元の code
//! で返る（D-121）ので、同じ code なら script ごと待って動かし直す（`Shiki.*` の副作用は呼び出し連番の冪等キーで
//! 高々 1 回・engine.md §7）。いまの `Shiki.storage.read` は文章のファイルだけを読み、取り込み待ちを返さない。

use serde_json::json;

use super::super::model::StepStatus;
use super::{append_event, map_db, ClaimedStep, RunStoreError};
use crate::retry::WaitPolicy;
use crate::vocab::RunEventKind;

/// 待つなら step を ready に戻して `true`。待ち始めてから上限を過ぎていれば何もせず `false`（失敗にする）。
///
/// 呼び出し側（checkpoint の Tx）が step の行をロックしている前提。
pub(super) async fn wait_and_retry(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    claimed: &ClaimedStep,
    code: &str,
    policy: WaitPolicy,
) -> Result<bool, RunStoreError> {
    let waited: Option<i64> = sqlx::query_scalar(
        "SELECT floor(extract(epoch FROM now() - wait_since))::bigint FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(&claimed.tenant_id)
    .bind(claimed.run_id)
    .bind(&claimed.step_path)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_db)?;
    let Some(delay) = policy.next_delay_secs(waited.unwrap_or(0)) else {
        return Ok(false);
    };
    // attempt の会計は「claim が +1 → 数えない実行を ready へ戻す経路が -1」（store/claim.rs のモジュールドキュメント）。
    sqlx::query(
        "UPDATE step_execution SET status = $4, lease_owner = NULL, lease_expires_at = NULL, \
             next_retry_at = now() + ($5 || ' seconds')::interval, attempt = attempt - 1, \
             wait_code = $6, wait_since = coalesce(wait_since, now()), updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(&claimed.tenant_id)
    .bind(claimed.run_id)
    .bind(&claimed.step_path)
    .bind(StepStatus::Ready.as_str())
    .bind(delay)
    .bind(code)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    append_event(
        tx,
        &claimed.tenant_id,
        claimed.run_id,
        RunEventKind::StepRetrying,
        &json!({
            "step": claimed.step_path,
            "attempt": claimed.attempt,
            "class": "Wait",
            "code": code,
            "waited_secs": waited.unwrap_or(0),
        }),
    )
    .await?;
    Ok(true)
}
