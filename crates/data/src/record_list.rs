//! レコード一覧（v1 の API 形・Task 15.3 でクエリコンパイラ v2 の上に載せ替え）。
//!
//! 等値フィルタ 1 つ＋ソート 1 つの簡易形を [`crate::DataQuery`] へ写して同じチョークポイントで
//! 実行する（行述語・マスク・索引到達可能性はそちらで強制）。OFFSET は廃止し、keyset カーソル
//! （`next_cursor` → `cursor`）でページングする。

use authz::AuthContext;
use serde_json::Value;
use uuid::Uuid;

use crate::index::SortPrecision;
use crate::model::DataRecord;
use crate::query::ir::{CondOp, Condition, CountResult, DataQuery, SortKey};
use crate::store::DataStore;
use crate::DataError;

/// 一覧のフィルタ（型で決まる等値。multi_select は「含む」）。
#[derive(Debug, Clone)]
pub struct RecordFilter {
    pub field: String,
    pub value: Value,
}

/// 一覧のソート（宣言フィールドのみ）。
#[derive(Debug, Clone)]
pub struct RecordSort {
    pub field: String,
    pub descending: bool,
}

/// 一覧のオプション。
#[derive(Debug, Clone, Default)]
pub struct ListRecordsOptions {
    pub filter: Option<RecordFilter>,
    pub sort: Option<RecordSort>,
    /// 1 ページの件数（0 以下は既定 50・最大 200）。
    pub limit: i64,
    /// 前ページの `next_cursor`。
    pub cursor: Option<String>,
}

/// 一覧結果。
#[derive(Debug, Clone)]
pub struct ListRecordsPage {
    pub items: Vec<DataRecord>,
    /// 個別共有集合が上限（PIT-18）で切り詰められ、共有経由の一部行が
    /// 表示されていない可能性がある（fail-closed・可視減方向）。
    pub shares_truncated: bool,
    /// 次ページのカーソル（無ければ終端）。
    pub next_cursor: Option<String>,
    /// ソートの精度（切り詰めが起こり得る列なら `prefix`）。
    pub sort_precision: SortPrecision,
}

fn to_query(filter: Option<&RecordFilter>) -> DataQuery {
    DataQuery {
        filter: filter.map(|f| Condition::cmp(&f.field, CondOp::Eq, Some(f.value.clone()))),
        ..DataQuery::default()
    }
}

impl DataStore {
    /// レコード一覧（viewer・宣言フィールドの等値フィルタ＋ソート・keyset ページング）。
    pub async fn list_records(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        opts: &ListRecordsOptions,
        trace_id: Option<&str>,
    ) -> Result<ListRecordsPage, DataError> {
        let mut q = to_query(opts.filter.as_ref());
        q.sort = opts
            .sort
            .iter()
            .map(|s| SortKey {
                field: s.field.clone(),
                descending: s.descending,
            })
            .collect();
        q.limit = u32::try_from(opts.limit.clamp(0, 200))
            .ok()
            .filter(|n| *n > 0);
        q.cursor.clone_from(&opts.cursor);
        let tuning = self.tuning.clone();
        let (r, _) = self
            .run_query_as(ctx, table_id, &q, trace_id, "data.record.list", &tuning)
            .await?;
        Ok(ListRecordsPage {
            items: r.items.unwrap_or_default(),
            shares_truncated: r.shares_truncated,
            next_cursor: r.next_cursor,
            sort_precision: r.sort_precision,
        })
    }

    /// 可視行の件数（上限付き・行述語適用・不可視行は件数に混入しない）。上限に達したら上限値。
    pub async fn count_records(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        filter: Option<&RecordFilter>,
        trace_id: Option<&str>,
    ) -> Result<i64, DataError> {
        Ok(self
            .count_records_capped(ctx, table_id, filter, trace_id)
            .await?
            .value() as i64)
    }

    /// 可視行の上限付き件数（`Exact(n)` / `AtLeast(上限)`）。
    pub async fn count_records_capped(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        filter: Option<&RecordFilter>,
        trace_id: Option<&str>,
    ) -> Result<CountResult, DataError> {
        let mut q = to_query(filter);
        q.with_count = true;
        // limit 0 + with_count = 件数だけ（ページは取らない）。
        q.limit = Some(0);
        let tuning = self.tuning.clone();
        let (r, _) = self
            .run_query_as(ctx, table_id, &q, trace_id, "data.record.count", &tuning)
            .await?;
        Ok(r.count.unwrap_or(CountResult::Exact(0)))
    }
}
