//! 自前オプティマイザの運用面（Task 15.12）: フィードバックの記録・統計収集の前倒し・較正・
//! 管理画面向けの報告（値・推定・戦略名を含まない）・機微なテーブルの固定戦略の設定。

use std::time::Instant;

use authz::AuthContext;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::bound::BoundQuery;
use super::planner::cost::{calibrate, CalibrationReport, UnitCosts};
use super::planner::exec::Exec;
use super::planner::feedback::{CandidateKind, QueryExecStats, ShapeKey};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 補正係数のキー（テーブル, フィルタ列, 第 1 ソート列）。
pub(crate) fn shape_key(table_id: Uuid, q: &BoundQuery) -> ShapeKey {
    let mut atoms = Vec::new();
    q.filter.atoms(&mut atoms);
    let mut fields: Vec<_> = atoms.iter().map(|a| a.field).collect();
    fields.sort_unstable();
    fields.dedup();
    (table_id, fields, q.sort.first().map(|k| k.field))
}

/// 管理画面の索引の候補（列の参照名と回数だけ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OptimizerIndexCandidate {
    /// 候補の種類。
    pub kind: OptimizerCandidateKind,
    /// 対象の列（参照名・複合索引は並べ替えの順）。
    pub fields: Vec<String>,
    /// このプロセスで観測した回数。
    pub occurrences: u64,
}

/// 索引の候補の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OptimizerCandidateKind {
    /// 複数ソートの複合索引の材料化（深いページで劣化し続ける並べ替えの組み合わせ）。
    CompositeIndex,
    /// 未索引列の索引の昇格（絞り込み・並べ替えの要求が多い列）。
    IndexPromotion,
}

/// テーブルのクエリ最適化の報告（管理画面・組織管理者）。
///
/// **推定値・統計値・予測コスト・選んだ計画の名前を含めない**（PIT-75）。値を含まない運用の数値と
/// 索引の候補（列の参照名）だけ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TableOptimizerReport {
    /// 固定計画（統計に依らない計画で読む・機微なテーブル向け）か。
    pub fixed_plan: bool,
    /// このプロセスで処理した読取の数。
    pub requests: u64,
    /// 走査効率（読んだ索引エントリ ÷ 返した行）の p95。
    pub scan_ratio_p95: f64,
    /// 途中で計画を切り替えた割合（0〜1）。
    pub switch_rate: f64,
    /// 重すぎるため断った読取の数。
    pub rejected_requests: u64,
    /// 索引の候補。
    pub candidates: Vec<OptimizerIndexCandidate>,
}

impl DataStore {
    /// 受付制御で断った読取も運用指標に残す（推定値はエラーに含めない）。
    pub(crate) fn reject_record<T>(
        &self,
        table_id: Uuid,
        stats: &QueryExecStats,
        r: Result<T, DataError>,
    ) -> Result<T, DataError> {
        if let Err(DataError::QueryTooExpensive { .. }) = &r {
            self.feedback.record(table_id, stats);
        }
        r
    }

    /// 実行の後始末: 所要時間・予測コストの記録と、ソート駆動の補正係数の更新。
    pub(crate) fn observe(&self, ex: &mut Exec<'_, '_>, shape: ShapeKey, started: Instant) {
        ex.stats.elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            ex.stats.predicted_us = ex.predicted_us.map(|p| p.max(0.0).round() as u64);
        }
        if let (Some(pred), Some(obs)) = (ex.sort_walk_pred, ex.sort_walk_obs) {
            let corrected = pred * ex.correction;
            if obs > 10.0 * corrected.max(1.0) || (corrected >= 10.0 && obs * 10.0 < corrected) {
                ex.stats.estimate_miss = true;
            }
            self.feedback.observe_sort(shape, pred, obs);
        }
    }

    /// 統計収集を前倒しで積む（ベストエフォート）。
    pub(crate) async fn request_analyze(&self, tenant_id: &str, table_id: Uuid) {
        let r = async {
            let mut conn = self
                .pools
                .for_tenant(tenant_id)
                .acquire()
                .await
                .map_err(map_db)?;
            crate::stats::counters::enqueue_analyze(&mut conn, tenant_id, table_id).await
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, %table_id, "統計収集の前倒しの要求に失敗");
        }
    }

    /// 起動時の較正ベンチ（一時表で範囲走査・点検索・本体取得・往復を計測し、単価を置き換える）。
    /// 顧客データには触れない。前後の単価を返す。
    pub async fn calibrate_costs(&self) -> Result<CalibrationReport, DataError> {
        let mut conn = self.pools.registry().acquire().await.map_err(map_db)?;
        calibrate(&mut conn, &self.costs).await
    }

    /// 運用・ベンチ用: テーブルの形（行数・型・種類数・頻度の形・値なし率・相関）を**値を含まない数値**で
    /// 取り出す（query-optimizer.md §9.3。合成ベンチの入力・利用者へは返さない・API には出さない）。
    #[doc(hidden)]
    pub async fn extract_table_shape(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<Option<crate::query::planner::shape::TableShape>, DataError> {
        let mut conn = self
            .pools
            .for_tenant(tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let row: Option<(i32, i64, sqlx::types::Json<crate::model::TableSchema>)> = sqlx::query_as(
            "SELECT tenant_int, table_int, schema FROM data_table \
                 WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db)?;
        let Some((tenant_int, table_int, schema)) = row else {
            return Ok(None);
        };
        crate::query::planner::shape::extract(
            &mut conn,
            crate::model::TableKeys {
                tenant_int,
                table_int,
            },
            &schema.0,
        )
        .await
    }

    /// 現在の単価（較正済みならその値）。
    pub fn unit_costs(&self) -> UnitCosts {
        self.costs.units()
    }

    /// 最後の較正の結果。
    pub fn calibration_report(&self) -> Option<CalibrationReport> {
        self.costs.report()
    }

    /// 検証用: 単価を差し替える（較正の前後の比較）。
    #[doc(hidden)]
    pub fn set_unit_costs(&self, u: UnitCosts) {
        self.costs.set(u);
    }

    /// 予測コストと実測の相関（対数のピアソン）と組の数（運用指標・値を含まない）。
    pub fn optimizer_correlation(&self) -> (Option<f64>, usize) {
        self.feedback.correlation()
    }

    /// 検証用: 予測と実測の組を捨てる。
    #[doc(hidden)]
    pub fn clear_optimizer_samples(&self) {
        self.feedback.clear_samples();
    }

    /// 管理画面: テーブルのクエリ最適化の報告（組織管理者・呼び出し側のルートが `OrgAdmin` を掛ける）。
    pub async fn table_optimizer_report(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
    ) -> Result<TableOptimizerReport, DataError> {
        let fixed: Option<bool> = sqlx::query_scalar(
            "SELECT planner_fixed FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND id = $3 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(table_id)
        .fetch_optional(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        let fixed = fixed.ok_or(DataError::NotFound)?;
        let m = self.feedback.snapshot(table_id).unwrap_or_default();
        #[allow(clippy::cast_precision_loss)]
        let switch_rate = if m.requests > 0 {
            m.switches as f64 / m.requests as f64
        } else {
            0.0
        };
        let candidates = self
            .feedback
            .candidates(table_id)
            .into_iter()
            .map(|(k, fields, n)| OptimizerIndexCandidate {
                kind: match k {
                    CandidateKind::CompositeIndex => OptimizerCandidateKind::CompositeIndex,
                    CandidateKind::IndexPromotion => OptimizerCandidateKind::IndexPromotion,
                },
                fields,
                occurrences: n,
            })
            .collect();
        Ok(TableOptimizerReport {
            fixed_plan: fixed,
            requests: m.requests,
            scan_ratio_p95: m.scan_ratio_p95,
            switch_rate,
            rejected_requests: m.rejections,
            candidates,
        })
    }

    /// 管理画面: 機微なテーブルの固定計画を切り替える（組織管理者・監査に残す・§7.3）。
    pub async fn set_table_fixed_plan(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        fixed: bool,
        trace_id: Option<&str>,
    ) -> Result<TableOptimizerReport, DataError> {
        let mut tx = self
            .pools
            .for_tenant(&ctx.tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let n = sqlx::query(
            "UPDATE data_table SET planner_fixed = $4, updated_at = now() \
             WHERE tenant_id = $1 AND org = $2 AND id = $3 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(table_id)
        .bind(fixed)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?
        .rows_affected();
        if n == 0 {
            return Err(DataError::NotFound);
        }
        self.record_audit_on(
            &mut tx,
            ctx,
            "data.table.fixed_plan",
            &table_id.to_string(),
            trace_id,
            serde_json::json!({ "fixed_plan": fixed }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        self.table_optimizer_report(ctx, table_id).await
    }

    /// 索引の昇格の候補として数える（上限超過で承認が要る列・未索引列での絞り込み・並べ替えの要求）。
    /// 結果はそのまま返す。
    pub(crate) fn note_candidates<T>(
        &self,
        table_id: Uuid,
        table: &crate::model::DataTable,
        masked: &std::collections::HashSet<String>,
        query: &super::ir::DataQuery,
        r: Result<T, DataError>,
    ) -> Result<T, DataError> {
        let fields: Vec<String> = match &r {
            Err(DataError::IndexApprovalRequired { field, .. }) => vec![field.clone()],
            Err(DataError::Invalid(_)) => crate::index::promote::query_fields(query)
                .into_iter()
                .filter(|k| {
                    !masked.contains(k)
                        && table
                            .schema
                            .field(k)
                            .is_some_and(crate::index::promote::promotable)
                })
                .collect(),
            _ => Vec::new(),
        };
        for f in fields {
            self.feedback
                .candidate(table_id, CandidateKind::IndexPromotion, vec![f]);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 管理画面の報告に推定値・統計値・計画名が無い（PIT-75）。
    #[test]
    fn admin_report_schema_has_no_estimates() {
        use utoipa::PartialSchema;
        let s = serde_json::to_string(&<TableOptimizerReport as PartialSchema>::schema()).unwrap();
        for banned in [
            "estimate",
            "selectivity",
            "cost",
            "record_int",
            "strategy",
            "histogram",
            "distinct",
            "mcv",
            "predicted",
        ] {
            assert!(!s.contains(banned), "{banned} が API スキーマにある: {s}");
        }
    }
}
