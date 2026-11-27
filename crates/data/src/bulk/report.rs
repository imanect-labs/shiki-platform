//! 取込の断られた行の一覧（行番号・断られた段・書き換えようとしたレコード）。
//!
//! 行の誤りの文（[`DataStore::bulk_import_errors`]）は呼び出し側が先頭の一部だけを見せる。ワークフローの
//! `data.bulk_upsert` の同じレコードを探す列（GAP-16）は、照合の後に同じ値を先に追加された行を探し直すので、断られた
//! 行を**全部**、検証の段（不合格）か適用の段（拒否）か、更新の行ならどのレコードを書き換えようとしたかと一緒に知る
//! 必要がある。行の値・誤りの文は返さない（観測 API と同じ入口で見せる相手を絞る）。

use authz::AuthContext;
use uuid::Uuid;

use super::{OP_UPDATE, ST_INVALID, ST_REJECTED};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 断られた行（行番号は 1 始まり）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkFailedRow {
    pub row_no: i64,
    /// 適用の段で拒否された（偽なら検証の段で不合格）。
    pub rejected: bool,
    /// 更新の行が書き換えようとしたレコード（作成の行は `None`）。
    pub update_of: Option<Uuid>,
}

impl DataStore {
    /// 不合格・拒否された行を、行番号の昇順に `limit` 行まで返す（入口は [`Self::bulk_import_errors`] と同じ）。
    pub async fn bulk_import_failed_rows(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        import_id: Uuid,
        limit: i64,
    ) -> Result<Vec<BulkFailedRow>, DataError> {
        let keys = self.bulk_import_gate(ctx, table_id, import_id).await?;
        let mut conn = self
            .pools
            .for_tenant(ctx.tenant_id())
            .acquire()
            .await
            .map_err(map_db)?;
        let rows: Vec<(i64, i16, i16, Uuid)> = sqlx::query_as(
            "SELECT row_no, state, op, record_id FROM data_bulk_staging \
             WHERE tenant_int = $1 AND import_id = $2 AND state IN ($3, $4) \
             ORDER BY row_no LIMIT $5",
        )
        .bind(keys.tenant_int)
        .bind(import_id)
        .bind(ST_INVALID)
        .bind(ST_REJECTED)
        .bind(limit.max(1))
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|(row_no, state, op, record_id)| BulkFailedRow {
                row_no,
                rejected: state == ST_REJECTED,
                update_of: (op == OP_UPDATE).then_some(record_id),
            })
            .collect())
    }
}
