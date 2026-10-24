//! 一括 upsert の更新行の適用（Task 15.7・15.11 からの申し送り）。
//!
//! 一括書込パス（[`super`]）の適用段は作成行だけを扱っていた。ワークフローの `data.bulk_upsert` は
//! 既存レコードの更新を混ぜるので、更新行を **1 件の更新と同じ入口**（[`DataStore::update_locked`]）で
//! 適用する。チャンクの Tx の中で行ごとにセーブポイントを切り、行の誤り（不可視・書込述語・`rev`・
//! マスク列・検証・unique）はその行だけを拒否してチャンクの他の行は進める。
//!
//! | 不変条件 | 入口 |
//! |---|---|
//! | 行述語つきロック（不可視は「見つからない」・rev オラクル封じ） | `lock_visible_by_id` |
//! | 書込述語（row_policy.write・個別共有 editor） | `write_allowed` |
//! | `rev` の楽観ロック（指定した行だけ） | `update_locked` |
//! | マスク列への書込拒否（PIT-19） | `ensure_writable_fields` |
//! | 型・required・参照先の可視性（PIT-20）・FSM の status 直書き拒否 | 1 件の更新と同じ検証器 |
//! | 索引の差分更新・材料化・リビジョン・outbox（SSE）・監査 | 1 件の更新と同じ副作用 |

use authz::AuthContext;
use serde_json::{Map, Value};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use crate::index::state::IndexPlan;
use crate::model::DataTable;
use crate::policy::memo::AuthzMemo;
use crate::store::DataStore;
use crate::DataError;

/// 適用待ちの更新行。
pub(super) struct PendingUpdate {
    pub(super) row_no: i64,
    pub(super) id: Uuid,
    pub(super) patch: Map<String, Value>,
    pub(super) expected_rev: Option<i64>,
}

/// 更新行の誤りを行の拒否理由へ（入力・権限・競合は行に付け、内部エラーは伝播する）。
///
/// 不可視と不在は同じ文言（存在オラクルを作らない・PIT-21）。
fn update_row_message(e: DataError) -> Result<String, DataError> {
    match e {
        DataError::Invalid(m) => Ok(m),
        DataError::InvalidInput(e) => Ok(e.message().to_string()),
        e if e.is_conflict() => Ok(e.conflict_detail().unwrap_or_default()),
        DataError::NotFound => Ok("更新対象のレコードが見つかりません".into()),
        DataError::Forbidden => {
            Ok("このレコード（またはフィールド）を更新する権限がありません".into())
        }
        other => Err(other),
    }
}

impl DataStore {
    /// 更新行を適用する。`(適用した (row_no, 外部 ID), 拒否した (row_no, 文言))`。
    #[allow(clippy::too_many_arguments)] // チャンクの Tx・認可の文脈・計画・行の束。
    pub(super) async fn apply_updates(
        &self,
        tx: &mut PgConnection,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
        plan: &IndexPlan,
        rows: &[PendingUpdate],
        trace_id: Option<&str>,
    ) -> Result<(Vec<(i64, Uuid)>, Vec<(i64, String)>), DataError> {
        let mut applied = Vec::new();
        let mut rejected = Vec::new();
        for r in rows {
            let mut one = tx.begin().await.map_err(crate::map_db)?;
            match self
                .update_locked(
                    &mut one,
                    ctx,
                    az,
                    table,
                    plan,
                    r.id,
                    &r.patch,
                    r.expected_rev,
                    trace_id,
                )
                .await
            {
                Ok(_) => {
                    one.commit().await.map_err(crate::map_db)?;
                    applied.push((r.row_no, r.id));
                }
                Err(e) => {
                    one.rollback().await.map_err(crate::map_db)?;
                    rejected.push((r.row_no, update_row_message(e)?));
                }
            }
        }
        Ok((applied, rejected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_errors_do_not_reveal_invisible_rows() {
        let not_found = update_row_message(DataError::NotFound).unwrap();
        assert!(!not_found.contains("権限"), "不可視と不在を区別しない");
        assert!(update_row_message(DataError::Forbidden).is_ok());
        assert_eq!(
            update_row_message(DataError::Conflict("rev".into())).unwrap(),
            "rev"
        );
        assert_eq!(
            update_row_message(DataError::RevConflict { current: 3 }).unwrap(),
            "rev が一致しません（現在 3）"
        );
        assert!(update_row_message(DataError::Internal("db".into())).is_err());
    }
}
