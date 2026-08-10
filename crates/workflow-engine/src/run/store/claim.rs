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

use super::{map_db, ClaimedStep, RunStore, RunStoreError};

/// ready 専用 claim の SQL。`$tenant_pred` はテナント絞り込み述語（空文字か `AND ...`）。
///
/// `cancel_requested` の除外は **`LIMIT` の内側**に置く。外へ出すと、キャンセル要求済み run の
/// step が先頭に居座っている間 claim が空振りし、他の run の実行待ちが進まなくなる。`OR` を外した
/// 後は index 順の nested loop で流れるため、内側に置いても評価は 1 件で止まる（実測 22 バッファ）。
///
/// run 側の列（workflow_id/org/principal/input/ir_snapshot）は副問い合わせで一緒に取り出し、
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
                        r2.input, r2.ir_snapshot \
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
             RETURNING s.run_id, picked.workflow_id, s.step_path, s.node_id, s.tenant_id, \
                       picked.org, picked.principal, picked.principal_kind, \
                       s.attempt, s.fencing_token, s.idempotency_key, \
                       picked.input, s.input AS step_input, picked.ir_snapshot"
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
/// 常に成り立つ。`fencing_token` は触らない——次の claim が +1 して旧ワーカーの書込を無効化する。
macro_rules! reclaim_leases_sql {
    ($tenant_pred:literal) => {
        concat!(
            "UPDATE step_execution s SET status = 'ready', lease_owner = NULL, \
                 lease_expires_at = NULL, next_retry_at = now(), \
                 attempt = greatest(s.attempt - 1, 0), updated_at = now() \
             FROM ( \
                 SELECT s2.tenant_id, s2.run_id, s2.step_path \
                   FROM step_execution s2 \
                   JOIN workflow_run r2 \
                     ON r2.tenant_id = s2.tenant_id AND r2.run_id = s2.run_id \
                  WHERE s2.status = 'running' AND s2.lease_expires_at < now() \
                    AND r2.status = 'running' AND NOT r2.cancel_requested",
            $tenant_pred,
            "  ORDER BY s2.lease_expires_at \
                  FOR UPDATE OF s2 SKIP LOCKED LIMIT 256 \
             ) picked \
             WHERE s.tenant_id = picked.tenant_id AND s.run_id = picked.run_id \
               AND s.step_path = picked.step_path"
        )
    };
}

const RECLAIM_LEASES_GLOBAL: &str = reclaim_leases_sql!(" ");
const RECLAIM_LEASES_SCOPED: &str = reclaim_leases_sql!(" AND s2.tenant_id = $1 ");

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
    pub async fn reclaim_expired_leases(
        &self,
        tenant_scope: Option<&str>,
    ) -> Result<u64, RunStoreError> {
        let query = match tenant_scope {
            Some(tenant_id) => sqlx::query(RECLAIM_LEASES_SCOPED).bind(tenant_id),
            None => sqlx::query(RECLAIM_LEASES_GLOBAL),
        };
        let done = query.execute(&self.db).await.map_err(map_db)?;
        Ok(done.rows_affected())
    }
}
