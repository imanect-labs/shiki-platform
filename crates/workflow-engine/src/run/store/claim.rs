//! step の claim（ready 専用）とリース失効回収（sweeper）。engine.md §4・§9.5（#438）。
//!
//! **claim は ready だけを対象にする。** リース失効した running の回収は scheduler tick が呼ぶ
//! [`RunStore::reclaim_expired_leases`] の仕事であり、engine.md §9.5 も sweeper をスケジューラ側と
//! 規定している。両者を 1 本の `OR` で拾うと partial index 2 本の BitmapOr になって index の
//! 並び順が失われ、`ORDER BY next_retry_at ... LIMIT 1` が「候補を全件集めて Sort してから 1 件」に
//! 退化する。すなわち **claim コストが実行待ち件数に比例**し、backlog が深くなるほど claim が遅く
//! なってさらに backlog が深くなる、という正のフィードバックを持つ。
//!
//! 実測（Postgres 16・`step_execution` 100 万行・ready 500 件・cross-tenant claim）:
//!
//! | 版 | バッファ | 実行計画 |
//! |---|---|---|
//! | `OR` 込み（旧） | 2,539 | BitmapOr → Sort → Limit（O(ready 件数)） |
//! | ready 専用＋`step_ready_global_idx`（本モジュール） | 22 | Index Scan → Limit（O(1)） |
//!
//! ## `attempt` の会計
//!
//! `step_execution.attempt` は「**試行として数える実行が何回始まったか**」で、実行履歴 UI が
//! そのまま「N 回目」として表示する（`web/src/components/workflow/runs/step-timeline.tsx`）。
//! したがって running の間も常に現在値でなければならない。規則は 2 つだけ:
//!
//! 1. **claim が +1 する**（実行を 1 つ始めた）。
//! 2. **「この実行は試行として数えない」と決めた経路が、ready へ戻すのと同一 UPDATE で -1 する。**
//!    該当するのは (a) リース失効回収＝ワーカーのクラッシュ（[`RunStore::reclaim_expired_leases`]・
//!    engine.md §9.5）と (b) `rate_limited`＝並行上限の順番待ち（`advance`・engine.md §8.2）。
//!
//! claim が ready 専用になったことで +1 は無条件になった（旧実装の
//! `CASE WHEN status = 'ready'` は、同じクエリでリース失効 running も拾っていたことの名残）。
//!
//! ## 拾い直しの上限（BUG-215）
//!
//! 回収は attempt を戻すので、試す回数ではリースの切れる step を止められない。ブロックの実行より外で決まって panic
//! する step や、ワーカーが誤りを返して前進できない step（IR を読めないなど）は、リースの期間ごとに上限なく拾い直されて
//! いた。回収の回数を `lease_reclaims` に数え、[`MAX_LEASE_RECLAIMS`] 回拾い直した step のリースがまた切れたら、
//! 拾い直さずに `internal` の失敗で確定し、run も失敗で終える（[`RunStore::reclaim_expired_leases`]）。

use serde_json::json;
use uuid::Uuid;

use super::{append_event, map_db, ClaimedStep, RunStore, RunStoreError};
use crate::vocab::RunEventKind;

/// リースが切れた step を拾い直す回数の上限（BUG-215）。
///
/// ワーカーが落ちる（配備の入れ替え・メモリの不足）たびに 1 回数える。同じ step の実行中にワーカーが 3 回落ちるのは
/// ふつうでないので、4 回目に切れたら拾い直さない。失敗したところから再開すると数え直す。
pub const MAX_LEASE_RECLAIMS: i32 = 3;

/// 拾い直しの上限に達した step の誤りの文（code は `internal`・やり直さない・実行履歴に出る）。panic の文や内部の値は
/// 出さない（[`super::super::node_panic::NODE_PANIC_MESSAGE`] と同じく、作成者には直せないので管理者に連絡するよう言う）。
pub const LEASE_RECLAIMS_EXHAUSTED_MESSAGE: &str =
    "このブロックの処理が途中で何度も止まったため、やり直さずに失敗にしました。\
     続くときは管理者に連絡してください。";

/// ready 専用 claim の SQL。`$tenant_pred` はテナント絞り込み述語（空文字か `AND ...`）。
///
/// `cancel_requested` の除外は **`LIMIT` の内側**に置く。外へ出すと、キャンセル要求済み run の
/// step が先頭に居座っている間 claim が空振りし、他の run の実行待ちが進まなくなる。`OR` を外した
/// 後は index 順の nested loop で流れるため、内側に置いても評価は 1 件で止まる（実測 22 バッファ）。
///
/// run 側の列（workflow_id/org/principal/input/きっかけの内容/ir_snapshot/始めた時刻/予定の時刻）は副問い合わせで一緒に取り出し、
/// `RETURNING` から `picked.*` として参照する（`workflow_run` への join を 1 本に保つ）。
macro_rules! claim_ready_sql {
    ($tenant_pred:literal) => {
        concat!(
            "UPDATE step_execution s SET status = 'running', lease_owner = $1, \
                 lease_expires_at = now() + ($2 || ' seconds')::interval, \
                 fencing_token = s.fencing_token + 1, attempt = s.attempt + 1, \
                 updated_at = now() \
             FROM ( \
                 SELECT s2.tenant_id, s2.run_id, s2.step_path, \
                        r2.workflow_id, r2.org, r2.principal, r2.principal_kind, \
                        r2.input, r2.trigger_payload AS trigger, \
                        r2.ir_snapshot, \
                        coalesce(r2.started_at, r2.created_at) AS run_started_at, \
                        r2.scheduled_at AS run_scheduled_at \
                   FROM step_execution s2 \
                   JOIN workflow_run r2 \
                     ON r2.tenant_id = s2.tenant_id AND r2.run_id = s2.run_id \
                  WHERE s2.status = 'ready' AND s2.next_retry_at <= now() \
                    AND NOT r2.cancel_requested",
            $tenant_pred,
            "  ORDER BY s2.next_retry_at \
                  FOR UPDATE OF s2 SKIP LOCKED LIMIT 1 \
             ) picked \
             WHERE s.tenant_id = picked.tenant_id AND s.run_id = picked.run_id \
               AND s.step_path = picked.step_path \
             RETURNING s.run_id, picked.run_started_at, picked.run_scheduled_at, \
                       picked.workflow_id, s.step_path, \
                       s.node_id, s.tenant_id, \
                       picked.org, picked.principal, picked.principal_kind, \
                       s.attempt, s.fencing_token, s.idempotency_key, \
                       picked.input, picked.trigger, s.input AS step_input, picked.ir_snapshot"
        )
    };
}

/// 全テナント横断 claim（既定のワーカー動作）。`step_ready_global_idx` を並び順ごと使う。
const CLAIM_READY_GLOBAL: &str = claim_ready_sql!(" ");
/// テナント固定 claim（tenant シャーディング・テスト分離）。`step_ready_idx` を並び順ごと使う。
///
/// 述語をパラメータの `IS NULL OR` で切り替えず SQL を分けているのは、prepared statement の
/// generic plan では `($3 IS NULL OR tenant_id = $3)` が index 条件へ落ちず、テナント絞り込みが
/// filter に退化し得るため。両分岐とも `'static` リテラルなのでインジェクション面は無い。
const CLAIM_READY_SCOPED: &str = claim_ready_sql!(" AND s2.tenant_id = $3 ");

/// リース失効回収の SQL。`$tenant_pred` は claim と同じくテナント絞り込み述語。
///
/// 対象は「ワーカー数 × 並列数」で上限が決まる有界集合なので、`step_lease_idx` を素直に引いて
/// バッチで戻す。cancel 要求済み run は対象外——そちらは `drain_cancel_requested` が
/// `cancelled` へ回収する（claim が cancel 要求済み run を除外する以上、ready に戻しても
/// 誰も拾えない）。
///
/// `attempt` を **ready へ戻すのと同一 UPDATE で -1 する**のが §9.5 の要件（ワーカーが
/// クラッシュしただけで試行を消費させない）。claim の +1 とこの -1 が対になっているため、
/// 「ready な step の attempt = 数え済みの試行回数」「running な step の attempt = 今回が何回目か」が
/// 常に成り立つ。
///
/// **`fencing_token` も同時に +1 する。** 回収を「次の claim が fencing を進めるまで」待たせては
/// いけない。回収から次の claim までの窓では旧ワーカーの token が依然一致するため、リースが切れた
/// だけで生きている旧ワーカーが `checkpoint_and_advance` の fencing 検証を通過してしまう。その結果、
/// retryable な結果なら **-1 済みの attempt のまま再試行されて `max_attempts` を超え**、terminal な
/// 結果なら実行済み step が 1 つ少ない attempt で記録される。回収した時点で旧 token を失効させれば、
/// 窓そのものが無くなる（Codex P1・#439）。
///
/// fencing は「現在の保持者だけが書ける」ことだけを表す単調増加値なので、claim 以外が進めても
/// 不変条件は壊れない（`heartbeat` / `append_event` / `fenced_finalize` はいずれも一致検査のみ）。
///
/// **拾い直した回数（`lease_reclaims`）も同時に +1 する。** 上限（`$max`）に達した step は対象外で、
/// [`EXHAUSTED_LEASES_GLOBAL`] が失敗で確定する（BUG-215）。
macro_rules! reclaim_leases_sql {
    ($tenant_pred:literal, $max:literal) => {
        concat!(
            "UPDATE step_execution s SET status = 'ready', lease_owner = NULL, \
                 lease_expires_at = NULL, next_retry_at = now(), \
                 attempt = greatest(s.attempt - 1, 0), lease_reclaims = s.lease_reclaims + 1, \
                 fencing_token = s.fencing_token + 1, updated_at = now() \
             FROM ( \
                 SELECT s2.tenant_id, s2.run_id, s2.step_path \
                   FROM step_execution s2 \
                   JOIN workflow_run r2 \
                     ON r2.tenant_id = s2.tenant_id AND r2.run_id = s2.run_id \
                  WHERE s2.status = 'running' AND s2.lease_expires_at < now() \
                    AND r2.status = 'running' AND NOT r2.cancel_requested \
                    AND s2.lease_reclaims < ",
            $max,
            $tenant_pred,
            "  ORDER BY s2.lease_expires_at \
                  FOR UPDATE OF s2 SKIP LOCKED LIMIT 256 \
             ) picked \
             WHERE s.tenant_id = picked.tenant_id AND s.run_id = picked.run_id \
               AND s.step_path = picked.step_path"
        )
    };
}

const RECLAIM_LEASES_GLOBAL: &str = reclaim_leases_sql!(" ", "$1");
const RECLAIM_LEASES_SCOPED: &str = reclaim_leases_sql!(" AND s2.tenant_id = $1 ", "$2");

/// 拾い直しの上限に達したまま、またリースが切れた step の候補（回収と同じ対象の条件・`$1` は上限）。
///
/// ロックは取らない（確定は 1 つずつ、run の行を先にロックする Tx で条件を確かめ直す・[`fail_exhausted_lease`]）。
macro_rules! exhausted_leases_sql {
    ($tenant_pred:literal) => {
        concat!(
            "SELECT s.tenant_id, s.run_id, s.step_path \
               FROM step_execution s \
               JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id \
              WHERE s.status = 'running' AND s.lease_expires_at < now() \
                AND r.status = 'running' AND NOT r.cancel_requested \
                AND s.lease_reclaims >= $1",
            $tenant_pred,
            " ORDER BY s.lease_expires_at LIMIT 64"
        )
    };
}

const EXHAUSTED_LEASES_GLOBAL: &str = exhausted_leases_sql!(" ");
const EXHAUSTED_LEASES_SCOPED: &str = exhausted_leases_sql!(" AND s.tenant_id = $2 ");

/// 拾い直しの上限に達した step を `internal` の失敗で確定し、run も失敗で終える（BUG-215）。確定したら `true`。
///
/// run の行を先にロックする（checkpoint と同じ順・デッドロックを避ける）。候補を読んでからの間に、生きていた
/// ワーカーが checkpoint した・取り消しを求められた・再開された step は、条件を確かめ直して触らない。`fencing_token` も
/// 進め、リースを失ったまま動いているワーカーの checkpoint を通さない。失敗の扱い（エラー時の出口・繰り返しの要素）は
/// 使わない（ブロックの外の不具合で、作成者の決めた道では扱えない）。
async fn fail_exhausted_lease(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    step_path: &str,
) -> Result<bool, RunStoreError> {
    let run_running: Option<bool> = sqlx::query_scalar(
        "SELECT true FROM workflow_run WHERE tenant_id = $1 AND run_id = $2 \
           AND status = 'running' AND NOT cancel_requested FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db)?;
    if run_running.is_none() {
        return Ok(false);
    }
    let failed: Option<String> = sqlx::query_scalar(
        "UPDATE step_execution SET status = 'failed', taken_ports = '{}', \
             error = jsonb_build_object('code', $5::text, 'message', $6::text, 'retryable', false, \
                                        'node_id', node_id, 'attempt', attempt), \
             lease_owner = NULL, lease_expires_at = NULL, wait_code = NULL, wait_since = NULL, \
             fencing_token = fencing_token + 1, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3 AND status = 'running' \
           AND lease_expires_at < now() AND lease_reclaims >= $4 \
         RETURNING node_id",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(step_path)
    .bind(MAX_LEASE_RECLAIMS)
    .bind(super::super::node_panic::NODE_PANIC_CODE)
    .bind(LEASE_RECLAIMS_EXHAUSTED_MESSAGE)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db)?;
    let Some(node_id) = failed else {
        return Ok(false);
    };
    append_event(
        tx,
        tenant_id,
        run_id,
        RunEventKind::StepFailed,
        &json!({ "step": step_path, "on_error": null, "taken_ports": [],
                 "reason": "lease_reclaims_exhausted" }),
    )
    .await?;
    super::advance::cancel_remaining_steps(tx, tenant_id, run_id).await?;
    sqlx::query(
        "UPDATE workflow_run SET status = 'failed', finished_at = now(), updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND status = 'running'",
    )
    .bind(tenant_id)
    .bind(run_id)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    append_event(
        tx,
        tenant_id,
        run_id,
        RunEventKind::RunFailed,
        &serde_json::Value::Null,
    )
    .await?;
    tracing::warn!(
        tenant_id, %run_id, step = step_path, node_id,
        max = MAX_LEASE_RECLAIMS,
        "リースの切れた step を上限まで拾い直したので、失敗にして run を終えた"
    );
    Ok(true)
}

impl RunStore {
    /// ready な step を 1 つ claim する（`FOR UPDATE SKIP LOCKED`・fencing +1・lease 取得）。
    ///
    /// `tenant_scope` を渡すとそのテナントの step のみ claim する（ワーカーの tenant シャーディング・
    /// テスト分離）。`None` は全テナント横断（既定のワーカー動作）。
    ///
    /// 返す `attempt` は「今回が何回目の実行か」（1-origin・DB へも書く）。会計規則は
    /// モジュールドキュメント参照。
    pub async fn claim_ready_step(
        &self,
        worker_id: &str,
        lease_secs: i64,
        tenant_scope: Option<&str>,
    ) -> Result<Option<ClaimedStep>, RunStoreError> {
        let query = match tenant_scope {
            Some(tenant_id) => sqlx::query_as::<_, ClaimedStep>(CLAIM_READY_SCOPED)
                .bind(worker_id)
                .bind(lease_secs)
                .bind(tenant_id),
            None => sqlx::query_as::<_, ClaimedStep>(CLAIM_READY_GLOBAL)
                .bind(worker_id)
                .bind(lease_secs),
        };
        query.fetch_optional(&self.db).await.map_err(map_db)
    }

    /// claim クエリの実行計画をテキストで返す（回帰テスト・運用診断用・実行はしない）。
    ///
    /// claim の性能は「partial index の並び順をそのまま使って 1 件で打ち切る」ことが前提で、
    /// `WHERE` 条件を足して `Sort` や `Bitmap Heap Scan` に退化すると O(実行待ち件数) へ戻る。
    /// 静かに退化するのを止めるため、計画を検証できる口をテストへ開けておく（#438）。
    pub async fn explain_claim_ready_step(
        &self,
        tenant_scope: Option<&str>,
    ) -> Result<String, RunStoreError> {
        let sql = match tenant_scope {
            Some(_) => CLAIM_READY_SCOPED,
            None => CLAIM_READY_GLOBAL,
        };
        let explain = format!("EXPLAIN {sql}");
        let query = match tenant_scope {
            Some(tenant_id) => sqlx::query_scalar::<_, String>(&explain)
                .bind("explain")
                .bind(1_i64)
                .bind(tenant_id),
            None => sqlx::query_scalar::<_, String>(&explain)
                .bind("explain")
                .bind(1_i64),
        };
        let plan: Vec<String> = query.fetch_all(&self.db).await.map_err(map_db)?;
        Ok(plan.join("\n"))
    }

    /// リース失効した running step を ready へ戻す（sweeper・engine.md §9.5）。戻り値は回収件数。
    ///
    /// 回収は「ready へ戻す」＋「`attempt` を -1（claim の +1 を打ち消す）」を同一 UPDATE で行う。
    /// クラッシュしただけで試行を消費させないための §9.5 の要件。
    ///
    /// 並行カウンタの減分リークは同一 tick の [`ConcurrencyStore::reconcile`] が running の実数から
    /// 再計算して回収する（engine.md §8.1）。そのため本関数は **reconcile より前**に呼ぶこと。
    ///
    /// [`ConcurrencyStore::reconcile`]: crate::concurrency::ConcurrencyStore::reconcile
    ///
    /// 拾い直しの上限（[`MAX_LEASE_RECLAIMS`]）に達した step は戻さずに `internal` の失敗で確定し、run も失敗で終える
    /// （BUG-215）。戻り値は戻した数と確定した数の和。
    pub async fn reclaim_expired_leases(
        &self,
        tenant_scope: Option<&str>,
    ) -> Result<u64, RunStoreError> {
        let failed = self.fail_exhausted_leases(tenant_scope).await?;
        let query = match tenant_scope {
            Some(tenant_id) => sqlx::query(RECLAIM_LEASES_SCOPED)
                .bind(tenant_id)
                .bind(MAX_LEASE_RECLAIMS),
            None => sqlx::query(RECLAIM_LEASES_GLOBAL).bind(MAX_LEASE_RECLAIMS),
        };
        let done = query.execute(&self.db).await.map_err(map_db)?;
        Ok(done.rows_affected() + failed)
    }

    /// 拾い直しの上限に達したまま、またリースが切れた step を失敗で確定する（1 つずつの Tx・[`fail_exhausted_lease`]）。
    async fn fail_exhausted_leases(
        &self,
        tenant_scope: Option<&str>,
    ) -> Result<u64, RunStoreError> {
        let query = match tenant_scope {
            Some(tenant_id) => sqlx::query_as::<_, (String, Uuid, String)>(EXHAUSTED_LEASES_SCOPED)
                .bind(MAX_LEASE_RECLAIMS)
                .bind(tenant_id),
            None => sqlx::query_as::<_, (String, Uuid, String)>(EXHAUSTED_LEASES_GLOBAL)
                .bind(MAX_LEASE_RECLAIMS),
        };
        let candidates = query.fetch_all(&self.db).await.map_err(map_db)?;
        let mut failed = 0;
        for (tenant_id, run_id, step_path) in candidates {
            let mut tx = self.db.begin().await.map_err(map_db)?;
            if fail_exhausted_lease(&mut tx, &tenant_id, run_id, &step_path).await? {
                tx.commit().await.map_err(map_db)?;
                failed += 1;
            } else {
                tx.rollback().await.map_err(map_db)?;
            }
        }
        Ok(failed)
    }
}
