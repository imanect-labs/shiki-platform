//! クエリの入口（Task 15.10）: 資源上限 → テーブルを開く → 参照先を開く → 束縛 → 正規化。
//!
//! コンパイル（束縛・正規化）の仕事量と経過時間は [`CompileBudget`] に積み、`MAX_COMPILE_WORK`（主）か
//! `MAX_COMPILE_MS`（外側の安全網）を超えたら 422。
//! テーブル・参照先を開く OpenFGA / DB の待ち時間は数えない。

use std::collections::HashSet;

use authz::AuthContext;
use storage::audit::AuditAction;
use uuid::Uuid;

use super::bound::{bind_query_scoped, BindOpts, BoundQuery, Expr};
use super::ir::DataQuery;
use super::limits::{check_ir, CompileBudget};
use super::planner::normalize::{normalize, Rules};
use super::run::QueryTuning;
use super::scopes::ScopeTables;
use crate::model::DataTable;
use crate::store::DataStore;
use crate::DataError;

impl DataStore {
    /// 入口の検査 → テーブルを開く → 参照先を開く → 束縛 → 正規化（コンパイルの時間予算つき）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_query(
        &self,
        az: &crate::policy::memo::AuthzMemo<'_>,
        budget: &mut CompileBudget,
        ctx: &AuthContext,
        table_id: Uuid,
        query: &DataQuery,
        trace_id: Option<&str>,
        action: AuditAction,
        tuning: &QueryTuning,
        auto_promote: bool,
    ) -> Result<(DataTable, HashSet<String>, ScopeTables, BoundQuery), DataError> {
        // 入口の資源上限（SQL 前段面と同じ上限を IR 直投入にも課す・data-platform §16.1.1）。
        check_ir(query, &self.limits).map_err(|r| DataError::Rejected(Box::new(r)))?;
        // 比較値・検索語・カーソルに NUL があれば 400（Postgres の text は NUL を受けられない・検証 D-8）。
        let doc =
            serde_json::to_value(query).map_err(|e| DataError::Internal(format!("query: {e}")))?;
        if crate::validate::contains_nul(&doc) {
            return Err(crate::validate::nul_error(None));
        }
        let linked =
            !query.joins.is_empty() || query.filter.as_ref().is_some_and(super::scopes::has_link);
        if query.snapshot && linked {
            return Err(DataError::input_detail(
                crate::InvalidCode::SnapshotUnsupported,
                None,
                "スナップショット読取では結合・リンクの条件を使えません",
            ));
        }
        let mut table = self
            .open_for_read(ctx, az, table_id, action, trace_id)
            .await?;
        let masked = self.masked_fields(ctx, az, &table).await?;
        self.analyze_on_read(tuning, &ctx.tenant_id, &table).await?;
        if auto_promote {
            let promoted = self
                .auto_promote_for(ctx, &table, &masked, query, trace_id)
                .await;
            if self.note_candidates(table_id, &table, &masked, query, promoted)? {
                table = self.fetch_live(ctx, table_id).await?;
            }
        }
        // 結合・半結合の参照先（閲覧者本人の権限で開く・PIT-20）。
        let scopes = self
            .load_scope_tables(ctx, az, &table, query, trace_id)
            .await?;
        let opts = BindOpts {
            max_join_depth: self.limits.max_join_depth,
            semijoin: !tuning.keep_joins,
        };
        let rej = |r| DataError::Rejected(Box::new(r));
        let bound = budget
            .stage(&self.limits, || {
                bind_query_scoped(&table.schema, &masked, query, &scopes, opts)
            })
            .map_err(rej)?;
        let mut bq = self.note_candidates(table_id, &table, &masked, query, bound)?;
        // 材料化した派生列で絞る・並べるなら、その前提（4 階層）を今の認可状態で検査し直す。崩れていれば
        // 降格（索引行を同一 Tx で破棄）してから束縛し直す＝その列はフィルタ/ソートに使えなくなる（PIT-71）。
        if bq.fields.values().any(|f| f.derived)
            && self.verify_materialized(ctx, az, &table).await?
        {
            table = self.fetch_live(ctx, table_id).await?;
            bq = bind_query_scoped(&table.schema, &masked, query, &scopes, opts)?;
        }
        if bq.is_aggregate() {
            // 集計クエリは監査に残す（PIT-17: 反復差分の検知可能性）。
            self.record_audit_best_effort(
                ctx,
                AuditAction::DataQueryAggregate,
                &table_id.to_string(),
                trace_id,
                serde_json::json!({ "group_by": query.group_by }),
            )
            .await;
        }
        bq.filter = budget
            .stage(&self.limits, || {
                normalize(
                    std::mem::replace(&mut bq.filter, Expr::True),
                    &bq.fields,
                    Rules::ALL,
                )
            })
            .map_err(rej)?;
        Ok((table, masked, scopes, bq))
    }
}
