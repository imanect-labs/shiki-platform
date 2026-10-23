//! 本体の取得（ページの行だけ・`(tenant_int, table_int, record_int)` の一意索引で点検索・監査 B2-1）。
//!
//! **射影刈り込み**（data-platform §16.2）: 射影（`select`）があれば、本体の JSONB から射影の列と
//! 事後検査（行述語の Rust での再評価・query-optimizer §7.2）が読む列**だけ**を DB 側で取り出す
//! （[`super::planner::lower::fetch_sql`]）。キーの集合はバインドで渡し、テキストは刈り込みの有無だけで決まる
//! （同一形状 ⇒ 同一テキスト・プランキャッシュの契約）。次の場合は刈り込まない（本体を丸ごと取る）:
//!
//! - 派生列（lookup / rollup / formula 等）・対称列を射影する（計算の材料に他の列が要る）
//! - 結合を残したクエリ（組の組み立ては結合元の行を使う）

use std::collections::BTreeSet;

use super::bound::BoundQuery;
use super::planner::exec::{Captured, Exec, V_FETCH};
use super::planner::lower::fetch_sql;
use super::sql::bind_query_as;
use crate::model::TableSchema;
use crate::policy::ast::PolicyExpr;
use crate::record::RecordRow;
use crate::{map_db, DataError};

/// 本体取得の行（`record_int` は crate 内に閉じる）。
#[derive(sqlx::FromRow)]
pub(crate) struct FetchedRow {
    pub(crate) record_int: i64,
    #[sqlx(flatten)]
    pub(crate) row: RecordRow,
}

/// 射影刈り込みで取る本体のキー（内部 ID 形式 `f{id}`・`None` は丸ごと）。
pub(crate) fn prune_keys(bq: &BoundQuery, schema: &TableSchema) -> Option<Vec<String>> {
    let sel = bq.select.as_ref()?;
    if !bq.joins.is_empty() {
        return None;
    }
    let mut keys = BTreeSet::new();
    for k in sel {
        // 擬似列（`_created_at` 等）は本体の列から取るので JSONB のキーは要らない。
        let Some(f) = schema.field(k) else {
            continue;
        };
        if f.field_type.is_derived() || f.is_reverse_link() {
            return None;
        }
        keys.insert(f.json_key());
    }
    // 事後検査が読む列（行述語の比較の列）。
    fn walk<'a>(e: &'a PolicyExpr, out: &mut Vec<&'a str>) {
        match e {
            PolicyExpr::Any(v) | PolicyExpr::All(v) => v.iter().for_each(|c| walk(c, out)),
            PolicyExpr::FieldCmp { field, .. } => out.push(field),
            _ => {}
        }
    }
    if let Some(p) = &schema.row_policy {
        let mut refs = Vec::new();
        walk(&p.read, &mut refs);
        for r in refs {
            if let Some(f) = schema.field(r) {
                keys.insert(f.json_key());
            }
        }
    }
    Some(keys.into_iter().collect())
}

/// ページの行の本体を取る（プランキャッシュに命中すれば文を組まない）。
pub(crate) async fn fetch_page(
    ex: &mut Exec<'_, '_>,
    tenant_id: &str,
    ints: &[i64],
    keys: Option<Vec<String>>,
) -> Result<Vec<FetchedRow>, DataError> {
    let v = ex.variant(V_FETCH).flag(keys.is_some()).finish();
    let (text, binds) = ex.compile("fetch", &v, |sql| {
        Ok(fetch_sql(tenant_id, ints.to_vec(), keys.clone(), sql))
    })?;
    ex.stats.statements += 1;
    let rows = bind_query_as(sqlx::query_as::<_, FetchedRow>(&text), &binds)
        .fetch_all(&mut *ex.conn)
        .await
        .map_err(map_db)?;
    ex.captured.push(Captured {
        label: "fetch",
        sql: text.to_string(),
        binds,
    });
    Ok(rows)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::{FieldDef, FieldType};
    use crate::policy::ast::{CmpOp, PolicyOperand, RowPolicy};
    use std::collections::HashSet;

    fn schema() -> TableSchema {
        let mut s = TableSchema {
            fields: vec![
                FieldDef::new("title", FieldType::Text),
                FieldDef::new("owner_dept", FieldType::Text),
                FieldDef::new("amount", FieldType::Number),
                FieldDef::new("total", FieldType::Formula),
            ],
            status_field: None,
            row_policy: Some(RowPolicy {
                read: PolicyExpr::Any(vec![
                    PolicyExpr::IsOwner,
                    PolicyExpr::FieldCmp {
                        field: "owner_dept".into(),
                        op: CmpOp::Eq,
                        value: PolicyOperand::UserId,
                    },
                ]),
                write: None,
            }),
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        s
    }

    fn bq(select: Option<&[&str]>) -> BoundQuery {
        let mut q = BoundQuery::fields_only(std::collections::BTreeMap::new());
        q.select = select.map(|v| v.iter().map(|k| (*k).to_string()).collect::<HashSet<_>>());
        q
    }

    #[test]
    fn prunes_to_selected_and_policy_columns() {
        let s = schema();
        assert_eq!(prune_keys(&bq(None), &s), None, "射影なしは丸ごと");
        let k = |key: &str| s.field(key).unwrap().json_key();
        let got = prune_keys(&bq(Some(&["title", "_created_at"])), &s).unwrap();
        let mut want = vec![k("title"), k("owner_dept")];
        want.sort();
        assert_eq!(got, want, "射影の列＋事後検査が読む行述語の列");
        assert_eq!(
            prune_keys(&bq(Some(&["total"])), &s),
            None,
            "派生列の射影は材料が要るので刈り込まない"
        );
    }
}
