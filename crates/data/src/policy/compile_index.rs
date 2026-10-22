//! 行述語 → **索引テーブル上の**述語（Task 15.3・data-platform §6.2・query-optimizer.md §3.3）。
//!
//! v1 の [`super::compile`]（`data_record.data` の JSONB を見る）と**同じ意味論**を、pivot 索引の
//! 点検索（`EXISTS … OFFSET 0`）で表す。クエリ条件の 2 値論理とは別の降下であり、混ぜない:
//!
//! | 述語 | v1（本体） | ここ（索引） |
//! |---|---|---|
//! | `eq c` | `data->>'f' = c`（値なしは NULL＝不成立） | 値の行が存在し `= c` |
//! | `ne c` | `data->>'f' <> c`（**値なしは不成立**＝fail-closed） | 値の行が存在し `≠ c`（行が無ければ不成立） |
//! | `in` | `= ANY(..)` | 値の行が存在し `= ANY(..)` |
//! | number | `::numeric` で比較 | `data_index_num.val`（numeric）で比較 |
//! | `is_owner` | `owner = $user` | 擬似列 owner（`field_id = 0`） |
//! | 個別共有 | `id = ANY(uuid[])` | `record_int = ANY(int8[])`（材料解決時に引き直し済み） |
//!
//! 索引が `active` でない列（バックフィル中・旧テーブル）は、その葉だけ**本体の点検索**に落とす
//! （v1 の式そのもの。正しいが少し遅い）。行述語を省く経路は存在しない。
//!
//! 切り詰め値（`truncated`）は等値の候補から外すか本体で再確認する（data-platform §3.3）。

use std::collections::HashSet;

use crate::index::{IndexTable, MAX_INDEX_KEY_BYTES};
use crate::model::{FieldId, FieldType, TableSchema, SYSTEM_FIELD_OWNER};
use crate::policy::ast::{CmpOp, PolicyExpr, PolicyOperand};
use crate::policy::compile::compile_field_cmp;
use crate::policy::material::PolicyMaterial;
use crate::query::sql::{Bind, SqlBuf};
use crate::DataError;

/// 索引上の行述語の材料（1 リクエスト分）。
pub(crate) struct IndexGuardCtx<'a> {
    pub schema: &'a TableSchema,
    pub material: &'a PolicyMaterial,
    pub user: &'a str,
    /// `active` な (フィールド, 索引テーブル)。
    pub active: &'a HashSet<(FieldId, IndexTable)>,
    /// 個別共有の `record_int`（この表に属するものだけ・外へ出さない）。
    pub shared_ints: &'a [i64],
}

/// 読取用の合成述語 `(row_policy OR 個別共有)`。`rec` は駆動行の `record_int` を指す SQL 式。
pub(crate) fn compile_index_read(
    g: &IndexGuardCtx<'_>,
    rec: &str,
    sql: &mut SqlBuf,
) -> Result<String, DataError> {
    let Some(policy) = &g.schema.row_policy else {
        return Ok("TRUE".to_string());
    };
    let p = compile_index_expr(&policy.read, g, rec, sql)?;
    if g.shared_ints.is_empty() {
        return Ok(format!("({p})"));
    }
    let ph = sql.push(Bind::I64Array(g.shared_ints.to_vec()));
    Ok(format!("({p} OR {rec} = ANY({ph}::int8[]))"))
}

/// 述語式 1 つ（v1 と同じ意味論）。
pub(crate) fn compile_index_expr(
    expr: &PolicyExpr,
    g: &IndexGuardCtx<'_>,
    rec: &str,
    sql: &mut SqlBuf,
) -> Result<String, DataError> {
    match expr {
        PolicyExpr::Public => Ok("TRUE".into()),
        PolicyExpr::HasRole { role, subtree } => {
            let holds = g.material.has_role(role, *subtree);
            Ok(if holds { "TRUE" } else { "FALSE" }.into())
        }
        PolicyExpr::IsOwner => {
            if g.active.contains(&(SYSTEM_FIELD_OWNER, IndexTable::Text)) {
                let cond = text_eq_cond("x", g.user, None, sql);
                Ok(exists_index(
                    IndexTable::Text,
                    SYSTEM_FIELD_OWNER,
                    rec,
                    &cond,
                    sql,
                ))
            } else {
                let ph = sql.push(Bind::Text(g.user.to_string()));
                Ok(exists_body(rec, &format!("b.owner = {ph}")))
            }
        }
        PolicyExpr::Any(children) | PolicyExpr::All(children) => {
            if children.is_empty() {
                return Ok("FALSE".into());
            }
            let joiner = if matches!(expr, PolicyExpr::Any(_)) {
                " OR "
            } else {
                " AND "
            };
            let parts = children
                .iter()
                .map(|c| compile_index_expr(c, g, rec, sql))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("({})", parts.join(joiner)))
        }
        PolicyExpr::FieldCmp { field, op, value } => field_cmp(g, rec, sql, field, *op, value),
    }
}

fn field_cmp(
    g: &IndexGuardCtx<'_>,
    rec: &str,
    sql: &mut SqlBuf,
    field: &str,
    op: CmpOp,
    value: &PolicyOperand,
) -> Result<String, DataError> {
    let f = g.schema.field(field).ok_or_else(|| {
        DataError::Internal(format!("row_policy が未知フィールド '{field}' を参照"))
    })?;
    let numeric = f.field_type == FieldType::Number;
    let table = if numeric {
        IndexTable::Num
    } else {
        IndexTable::Text
    };
    if !g.active.contains(&(f.id, table)) {
        // 索引が未完成: v1 の本体式で同じ葉を評価する（意味論は同一）。
        let body = compile_field_cmp(g.schema, g.material, g.user, "b", sql, field, op, value)?;
        return Ok(exists_body(rec, &body));
    }
    let jk = Some(f.id);
    // 値の行の条件（`x` = 索引行）。ne は「行が存在し、かつ eq 条件を満たさない」。
    let cond = match value {
        PolicyOperand::UserId => match op {
            CmpOp::Eq => text_eq_cond("x", g.user, jk, sql),
            CmpOp::Ne => format!("NOT {}", text_eq_cond("x", g.user, jk, sql)),
            CmpOp::In => {
                return Err(DataError::Internal(
                    "row_policy: $user.id に in は使えません（検証済みのはず）".into(),
                ))
            }
        },
        PolicyOperand::UserRoles { subtree } => {
            let roles = g.material.roles(*subtree);
            if roles.is_empty() {
                return Ok("FALSE".into());
            }
            text_in_cond("x", roles, jk, sql)
        }
        PolicyOperand::Lit(v) if numeric => {
            let n = v
                .as_number()
                .and_then(crate::numeric::canonical_number)
                .ok_or_else(|| DataError::Internal("row_policy: number リテラル不整合".into()))?;
            let ph = sql.typed(Bind::Numeric(n));
            match op {
                CmpOp::Eq => format!("x.val = {ph}"),
                CmpOp::Ne => format!("x.val <> {ph}"),
                CmpOp::In => {
                    return Err(DataError::Internal(
                        "row_policy: number の in は未対応（検証済みのはず）".into(),
                    ))
                }
            }
        }
        PolicyOperand::Lit(v) => match op {
            CmpOp::Eq | CmpOp::Ne => {
                let s = v
                    .as_str()
                    .ok_or_else(|| DataError::Internal("row_policy: text リテラル不整合".into()))?;
                let eq = text_eq_cond("x", s, jk, sql);
                if op == CmpOp::Eq {
                    eq
                } else {
                    format!("NOT {eq}")
                }
            }
            CmpOp::In => {
                let arr: Vec<String> = v
                    .as_array()
                    .ok_or_else(|| DataError::Internal("row_policy: in リテラル不整合".into()))?
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect();
                if arr.is_empty() {
                    return Ok("FALSE".into());
                }
                text_in_cond("x", &arr, jk, sql)
            }
        },
    };
    Ok(exists_index(table, f.id, rec, &cond, sql))
}

/// 索引行の存在検査（点検索）。`OFFSET 0` で半結合への引き上げ・ハッシュ化 SubPlan を止める
/// （query-optimizer.md §6.1）。
pub(crate) fn exists_index(
    table: IndexTable,
    field: FieldId,
    rec: &str,
    cond: &str,
    sql: &mut SqlBuf,
) -> String {
    let fph = sql.typed(Bind::I16(field_i16(field)));
    let rel = table.relation();
    let col = table.record_column();
    format!(
        "EXISTS (SELECT 1 FROM {rel} x WHERE x.tenant_int = $1 AND x.table_int = $2 \
         AND x.field_id = {fph} AND x.{col} = {rec} AND {cond} OFFSET 0)"
    )
}

/// 本体（`data_record`）の点検索。`cond` は別名 `b` を参照する。
pub(crate) fn exists_body(rec: &str, cond: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM data_record b WHERE b.tenant_int = $1 AND b.table_int = $2 \
         AND b.record_int = {rec} AND ({cond}) OFFSET 0)"
    )
}

/// `data_index_text` の行 `x` が値 `s` と等しいか（切り詰めを考慮・本体で再確認）。
///
/// - 短い値（≤ `MAX_INDEX_KEY_BYTES`）: `val = s AND NOT truncated`（切り詰め行の本体は長いので一致しない）
/// - 長い値: `val = prefix(s) AND truncated AND 本体 = s`（`field` が `None` の擬似列は長い値を持たない）
pub(crate) fn text_eq_cond(x: &str, s: &str, field: Option<FieldId>, sql: &mut SqlBuf) -> String {
    let (prefix, long) = crate::index::normalize::prefix_at_char_boundary(s, MAX_INDEX_KEY_BYTES);
    if !long {
        let ph = sql.push(Bind::Text(s.to_string()));
        return format!("({x}.val = {ph}::text COLLATE \"C\" AND NOT {x}.truncated)");
    }
    let Some(field) = field else {
        return "FALSE".into();
    };
    let pp = sql.push(Bind::Text(prefix.to_string()));
    let body = body_text_cmp(x, field, "=", s, sql);
    format!("({x}.val = {pp}::text COLLATE \"C\" AND {x}.truncated AND {body})")
}

/// 値のいずれかと等しいか（短い値は配列 1 本・長い値は個別に本体で再確認）。
pub(crate) fn text_in_cond(
    x: &str,
    vals: &[String],
    field: Option<FieldId>,
    sql: &mut SqlBuf,
) -> String {
    let (short, long): (Vec<&String>, Vec<&String>) =
        vals.iter().partition(|v| v.len() <= MAX_INDEX_KEY_BYTES);
    let mut parts = Vec::new();
    if !short.is_empty() {
        let ph = sql.push(Bind::TextArray(short.into_iter().cloned().collect()));
        parts.push(format!(
            "({x}.val = ANY({ph}::text[]) AND NOT {x}.truncated)"
        ));
    }
    for v in long {
        parts.push(text_eq_cond(x, v, field, sql));
    }
    format!("({})", parts.join(" OR "))
}

/// 切り詰め行の本体比較（`(b.data ->> 'f{id}') <op> s`）。JSONB キーもバインドで渡す。
pub(crate) fn body_text_cmp(
    x: &str,
    field: FieldId,
    op: &str,
    s: &str,
    sql: &mut SqlBuf,
) -> String {
    let kph = sql.push(Bind::Text(crate::field_key::json_key(field)));
    let vph = sql.push(Bind::Text(s.to_string()));
    format!(
        "EXISTS (SELECT 1 FROM data_record b WHERE b.tenant_int = $1 AND b.table_int = $2 \
         AND b.record_int = {x}.record_int AND (b.data ->> {kph}) COLLATE \"C\" {op} {vph}::text)"
    )
}

pub(crate) fn field_i16(field: FieldId) -> i16 {
    // FieldId は MAX_FIELD_ID（32,767）以下に検証済み。
    i16::try_from(field).unwrap_or(i16::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::FieldDef;
    use crate::policy::ast::RowPolicy;
    use std::sync::Arc;

    fn schema(read: PolicyExpr) -> TableSchema {
        let mut amount = FieldDef::new("amount", FieldType::Number);
        amount.indexed = true;
        let mut applicant = FieldDef::new("applicant", FieldType::UserRef);
        applicant.indexed = true;
        let mut s = TableSchema {
            fields: vec![applicant, amount],
            status_field: None,
            row_policy: Some(RowPolicy { read, write: None }),
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        s
    }

    fn material() -> PolicyMaterial {
        PolicyMaterial {
            roles_direct: Some(Arc::new(vec![])),
            roles_effective: Some(Arc::new(vec!["keiri".into()])),
            shared_record_ids: vec![],
            shares_truncated: false,
            role_members: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn ne_requires_value_row_and_binds_only() {
        let s = schema(PolicyExpr::FieldCmp {
            field: "applicant".into(),
            op: CmpOp::Ne,
            value: PolicyOperand::UserId,
        });
        let m = material();
        let active: HashSet<(FieldId, IndexTable)> =
            [(16, IndexTable::Text), (0, IndexTable::Text)].into();
        let g = IndexGuardCtx {
            schema: &s,
            material: &m,
            user: "alice",
            active: &active,
            shared_ints: &[7, 9],
        };
        let mut sql = SqlBuf::scoped(1, 2);
        let out = compile_index_read(&g, "d.record_int", &mut sql).unwrap();
        // ne は「行が存在し、かつ eq を満たさない」（行が無い＝値なしは不成立・v1 と同じ）。
        assert!(
            out.starts_with("(EXISTS (SELECT 1 FROM data_index_text x"),
            "{out}"
        );
        assert!(out.contains("AND NOT (x.val = $3::text"), "{out}");
        assert!(out.contains("OFFSET 0"));
        assert!(out.contains("d.record_int = ANY($5::int8[])"), "{out}");
        assert!(!out.contains("alice"));
    }

    #[test]
    fn inactive_index_falls_back_to_v1_body_expression() {
        let s = schema(PolicyExpr::All(vec![
            PolicyExpr::FieldCmp {
                field: "amount".into(),
                op: CmpOp::Ne,
                value: PolicyOperand::Lit(serde_json::json!(0)),
            },
            PolicyExpr::IsOwner,
            PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        ]));
        let m = material();
        let active = HashSet::new();
        let g = IndexGuardCtx {
            schema: &s,
            material: &m,
            user: "u",
            active: &active,
            shared_ints: &[],
        };
        let mut sql = SqlBuf::scoped(1, 2);
        let out = compile_index_read(&g, "d.record_int", &mut sql).unwrap();
        assert!(out.contains("FROM data_record b"), "{out}");
        assert!(
            out.contains("((b.data ->> 'f17'))::numeric <> $3::numeric"),
            "{out}"
        );
        assert!(out.contains("b.owner = $4"), "{out}");
        assert!(out.contains("TRUE"), "HasRole は定数: {out}");
    }

    #[test]
    fn long_literal_rechecks_body() {
        let mut sql = SqlBuf::scoped(1, 2);
        let long = "あ".repeat(1_000);
        let c = text_eq_cond("x", &long, Some(16), &mut sql);
        assert!(c.contains("x.truncated AND EXISTS"), "{c}");
        assert!(c.contains("(b.data ->> $4)"), "{c}");
        let mut sql = SqlBuf::scoped(1, 2);
        assert_eq!(text_eq_cond("x", &long, None, &mut sql), "FALSE");
    }
}
