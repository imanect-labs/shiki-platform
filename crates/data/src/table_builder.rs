//! 会話の AI がテーブルを作る・直す道具（D-10 の T05）の下支え。
//!
//! 道具（`crates/chat` の `data_table_create` / `data_table_update`）は、テーブルの作成・改訂そのものを
//! [`DataStore::create_table`]・[`DataStore::update_table_schema`]・[`crate::FsmStore::save_for_table`] に任せる。
//! ここに置くのは、その前後で道具が要る小さな読み口と、行の決まりの書き換えで書き込みの決まりを落とさない補い。
//!
//! - [`DataStore::find_table_by_name`]: 名前でテーブルを引く（閲覧できるものだけ・見えないと無いは同じ 404）。
//! - [`DataStore::own_tables_created_since`]: 本人が持ち主で、ある時刻（会話の run の作成時刻）以降に作ったテーブル。
//!   作成の冪等（R1・名前の重なりが本人のこの run の作成か）と、1 回の会話の run あたりの作成数の上限に使う。
//! - [`carry_write_guard`] は、行の決まりを入れ替えるとき、いまの書き込みの決まり（最初の状態を過ぎたら申請者は書き換え
//!   られない・D-115）を新しい行の決まりにも付け直す。AI は元の決まりだけを書き、決まりの後半を知らなくてよい。

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use storage::audit::AuditAction;
use uuid::Uuid;

use crate::model::{DataTable, TableSchema};
use crate::policy::{PolicyExpr, RowPolicy};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 本人が作ったテーブルの 1 件（[`DataStore::own_tables_created_since`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnTable {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// [`DataStore::own_tables_created_since`] が返す件数の上限（道具の作成数の上限より十分大きい）。
pub const OWN_TABLES_SINCE_CAP: i64 = 100;

impl DataStore {
    /// 名前でテーブルを引く（閲覧できるものだけ・本人の権限）。
    ///
    /// 名前は (テナント, 組織) で一意（生きている行のみ・`data_table_name_idx`）。見えない・無い・消したは同じ
    /// [`DataError::NotFound`]（存在を明かさない）。閲覧の確かめと監査は [`DataStore::get_table`] と同じ。
    pub async fn find_table_by_name(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND name = $3 AND deleted_at IS NULL",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(name.trim())
        .fetch_optional(self.pools.for_tenant(ctx.tenant_id()))
        .await
        .map_err(map_db)?;
        let id = id.ok_or(DataError::NotFound)?;
        match self.get_table(ctx, id, trace_id).await {
            // 見えるが読めない（追加だけで定義を読めない等）も、名前で引く入口では無いのと同じに言う。
            Err(DataError::Forbidden) => Err(DataError::NotFound),
            other => other,
        }
    }

    /// 本人が持ち主で、`since` 以降に作り、消していないテーブル（作成の古い順・上限 [`OWN_TABLES_SINCE_CAP`] 件）。
    ///
    /// `name` を渡せばその名前だけ。持ち主（owner タプル）も確かめる（作成者でも、持ち主を外されたものは返さない）。
    pub async fn own_tables_created_since(
        &self,
        ctx: &AuthContext,
        since: DateTime<Utc>,
        name: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<Vec<OwnTable>, DataError> {
        let rows: Vec<(Uuid, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT id, name, created_at FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND created_by = $3 AND created_at >= $4 \
               AND deleted_at IS NULL AND ($5::text IS NULL OR name = $5) \
             ORDER BY created_at, id LIMIT $6",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&ctx.principal().id)
        .bind(since)
        .bind(name.map(str::trim))
        .bind(OWN_TABLES_SINCE_CAP)
        .fetch_all(self.pools.for_tenant(ctx.tenant_id()))
        .await
        .map_err(map_db)?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, name, created_at) in rows {
            match self
                .require(
                    ctx,
                    id,
                    Relation::Owner,
                    AuditAction::DataTableGet,
                    trace_id,
                )
                .await
            {
                Ok(()) => out.push(OwnTable {
                    id,
                    name,
                    created_at,
                }),
                Err(DataError::NotFound | DataError::Forbidden) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }
}

/// 行の決まりを `next` に入れ替えるとき、`current`（いまのスキーマ）に付いている書き込みの決まり（D-115）を
/// `next` にも付け直す（`next` にすでに付いていれば、そのまま）。
///
/// 決まりは `write = all[ 元の書き込みの決まり, 決まりの後半 ]` の形なので、`next` の書き込みの決まり（無ければ
/// 読む決まり）を元の決まりとして、いまの後半をそのまま後ろに付ける。状態の列が無い・決まりが付いていないなら
/// 何もしない。
pub fn carry_write_guard(current: &TableSchema, next: &mut TableSchema) {
    let Some(status) = current.status_field.as_deref() else {
        return;
    };
    let Some(tail) = guard_tail(current.row_policy.as_ref(), status) else {
        return;
    };
    if crate::fsm_write_guard::current_guard(next.row_policy.as_ref(), status).is_some() {
        return;
    }
    let (read, base) = match next.row_policy.as_ref() {
        Some(p) => (p.read.clone(), p.write_expr().clone()),
        None => (PolicyExpr::Public, PolicyExpr::Public),
    };
    next.row_policy = Some(RowPolicy {
        read,
        write: Some(PolicyExpr::All(vec![base, tail])),
    });
}

/// いまの書き込みの決まりの後半（`any[ 状態 = 最初の状態, 申請者 ≠ 自分, … ]`）。付いていなければ `None`。
fn guard_tail(policy: Option<&RowPolicy>, status: &str) -> Option<PolicyExpr> {
    crate::fsm_write_guard::current_guard(policy, status)?;
    match policy?.write.as_ref()? {
        PolicyExpr::All(v) if v.len() == 2 => v.get(1).cloned(),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use crate::policy::{CmpOp, PolicyOperand};

    fn schema(policy: Option<RowPolicy>) -> TableSchema {
        TableSchema {
            fields: Vec::new(),
            status_field: Some("status".into()),
            row_policy: policy,
            field_policy: Vec::new(),
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        }
    }

    fn mine() -> PolicyExpr {
        PolicyExpr::FieldCmp {
            field: "applicant".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        }
    }

    fn guarded(base: Option<&RowPolicy>) -> RowPolicy {
        crate::fsm_write_guard::with_guard(base, "status", "draft", "applicant", &[])
    }

    #[test]
    fn carries_the_guard_onto_a_replaced_policy() {
        let current = schema(Some(guarded(None)));
        let mut next = schema(Some(RowPolicy {
            read: mine(),
            write: None,
        }));
        carry_write_guard(&current, &mut next);
        let expected = guarded(Some(&RowPolicy {
            read: mine(),
            write: None,
        }));
        assert_eq!(next.row_policy, Some(expected));
        assert_eq!(
            crate::fsm_write_guard::current_guard(next.row_policy.as_ref(), "status"),
            Some(("draft".into(), "applicant".into()))
        );
    }

    #[test]
    fn removed_policy_keeps_only_the_guard() {
        let current = schema(Some(guarded(None)));
        let mut next = schema(None);
        carry_write_guard(&current, &mut next);
        assert_eq!(next.row_policy, Some(guarded(None)));
    }

    #[test]
    fn leaves_policies_without_a_guard_alone() {
        let plain = RowPolicy {
            read: mine(),
            write: None,
        };
        let current = schema(Some(plain.clone()));
        let mut next = schema(None);
        carry_write_guard(&current, &mut next);
        assert_eq!(next.row_policy, None);

        // すでに付いていれば二重に付けない。
        let current = schema(Some(guarded(None)));
        let mut next = schema(Some(guarded(Some(&plain))));
        carry_write_guard(&current, &mut next);
        assert_eq!(next.row_policy, Some(guarded(Some(&plain))));

        // 状態の列が無ければ何もしない。
        let mut no_status = schema(Some(guarded(None)));
        no_status.status_field = None;
        let mut next = schema(None);
        carry_write_guard(&no_status, &mut next);
        assert_eq!(next.row_policy, None);
    }
}
