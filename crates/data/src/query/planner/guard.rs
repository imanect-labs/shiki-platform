//! `Guard(P)`（query-optimizer.md §7.1）: **生成される SQL の全ての出力行に、行述語 P 全体の検査が付く**
//! ことを型で保証する。
//!
//! - [`GuardSql`] は [`Guard::lower`] でしか作れず、候補・ソート駆動・件数・集計の各文の組み立て関数は
//!   `&GuardSql` を必須引数に取る（`Option` にしない）。P を省いた文は作れない。
//! - 認可駆動（[`Guard::drivers`]）で和集合から入るときも、P 全体の検査を冗長に残す（駆動の組み立ての
//!   誤りを漏洩にしない）。
//! - P の降下は**行述語専用**（[`crate::policy::compile_index`]・v1 の意味論）。クエリ条件の 2 値論理の
//!   降下（[`super::lower`]）は使わない（§3.3）。P に掛けるのは定数畳み込み（`HasRole`）だけ。

use crate::index::IndexTable;
use crate::model::{FieldType, SYSTEM_FIELD_OWNER};
use crate::policy::ast::{CmpOp, PolicyExpr, PolicyOperand};
use crate::policy::compile_index::{
    compile_index_read, field_i16, text_eq_cond, text_in_cond, IndexGuardCtx,
};
use crate::query::sql::{Bind, SqlBuf};
use crate::DataError;

/// 行述語の検査 SQL（[`Guard::lower`] だけが作る）。
#[derive(Debug, Clone)]
pub(crate) struct GuardSql(String);

impl GuardSql {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// 行述語（1 リクエスト分の材料つき）。
pub(crate) struct Guard<'a> {
    ctx: IndexGuardCtx<'a>,
}

/// P の定数畳み込みの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Folded {
    True,
    False,
    Expr(PolicyExpr),
}

/// 認可駆動の入口（P の選言 1 つ分の索引範囲）。
#[derive(Debug, Clone)]
pub(crate) enum Driver {
    /// `IsOwner` → 擬似列 owner。
    Owner(String),
    /// text 列の等値・`in`。
    Text {
        field: crate::model::FieldId,
        vals: Vec<String>,
    },
    /// number 列の等値。
    Num {
        field: crate::model::FieldId,
        val: String,
    },
    /// 個別共有（材料から件数が正確に分かる）。
    Shared(Vec<i64>),
}

impl Driver {
    /// 候補集合の SELECT（列 `record_int`）。
    pub(crate) fn source_sql(&self, sql: &mut SqlBuf) -> String {
        let (table, field, cond) = match self {
            Driver::Shared(ints) => {
                let ph = sql.push(Bind::I64Array(ints.clone()));
                return format!("SELECT unnest({ph}::int8[]) AS record_int");
            }
            Driver::Owner(user) => (
                IndexTable::Text,
                SYSTEM_FIELD_OWNER,
                text_eq_cond("x", user, None, sql),
            ),
            Driver::Text { field, vals } => (
                IndexTable::Text,
                *field,
                text_in_cond("x", vals, Some(*field), sql),
            ),
            Driver::Num { field, val } => {
                let ph = sql.typed(Bind::Numeric(val.clone()));
                (IndexTable::Num, *field, format!("x.val = {ph}"))
            }
        };
        let fph = sql.typed(Bind::I16(field_i16(field)));
        let t = sql.t();
        format!(
            "SELECT x.record_int FROM {} x WHERE x.tenant_int = $1 AND x.table_int = {t} \
             AND x.field_id = {fph} AND {cond}",
            table.relation()
        )
    }

    /// 件数探索の文（値索引の並びで `LIMIT` に固定する・[`super::lower::probe_union`]）。
    ///
    /// 値の集合（所属ロールなど・材料）は配列のバインドで渡し、値ごとの `LIMIT` 付き探索を
    /// `unnest` の `LATERAL` で並べる（**要素数で SQL テキストを変えない**・data-platform §16.3）。
    pub(crate) fn probe_sql(&self, cap: i64, sql: &mut SqlBuf) -> String {
        use super::lower::{probe_each, probe_union};
        match self {
            Driver::Shared(ints) => {
                let ph = sql.push(Bind::I64Array(ints.clone()));
                let cph = sql.typed(Bind::I64(cap));
                format!("SELECT count(*) FROM (SELECT unnest({ph}::int8[]) LIMIT {cph}) s")
            }
            Driver::Owner(user) => {
                let c = text_eq_cond("x", user, None, sql);
                probe_union(IndexTable::Text, SYSTEM_FIELD_OWNER, &[c], cap, sql)
            }
            Driver::Text { field, vals } => probe_each(
                IndexTable::Text,
                *field,
                Bind::TextArray(super::lower::index_prefixes(vals)),
                cap,
                sql,
            ),
            Driver::Num { field, val } => {
                let ph = sql.typed(Bind::Numeric(val.clone()));
                probe_union(
                    IndexTable::Num,
                    *field,
                    &[format!("x.val = {ph}")],
                    cap,
                    sql,
                )
            }
        }
    }

    /// 件数が材料だけで分かるか（個別共有）。
    pub(crate) fn known_count(&self) -> Option<u64> {
        match self {
            Driver::Shared(v) => Some(v.len() as u64),
            _ => None,
        }
    }
}

impl<'a> Guard<'a> {
    pub(crate) fn new(ctx: IndexGuardCtx<'a>) -> Self {
        Guard { ctx }
    }

    /// 駆動行 `rec` についての P の検査 SQL。
    pub(crate) fn lower(&self, rec: &str, sql: &mut SqlBuf) -> Result<GuardSql, DataError> {
        compile_index_read(&self.ctx, rec, sql).map(GuardSql)
    }

    fn folded(&self) -> Folded {
        match &self.ctx.schema.row_policy {
            None => Folded::True,
            Some(p) => fold(&p.read, self.ctx.material, true),
        }
    }

    /// 行述語が全行を通す（row_policy なし・`Public`・成立する `HasRole`）。
    pub(crate) fn is_unrestricted(&self) -> bool {
        self.folded() == Folded::True
    }

    /// 誰の行も見えない（P が FALSE・個別共有なし）。クエリを実行せず空を返してよい（§3.2 規則 3）。
    pub(crate) fn is_empty(&self) -> bool {
        self.folded() == Folded::False && self.ctx.shared_ints.is_empty()
    }

    /// 認可駆動の入口（P の各選言が索引で引けるときだけ）。共有があれば選言に足す。
    pub(crate) fn drivers(&self) -> Option<Vec<Driver>> {
        // 入口の形は `HasRole` の畳み込みだけで決める（所属ロールの有無で選言を消さない＝空の配列の入口を
        // 残す）。所属ロールの集合（材料）の有無で SQL テキストの形を変えない（data-platform §16.3）。
        let shape = match &self.ctx.schema.row_policy {
            None => Folded::True,
            Some(p) => fold(&p.read, self.ctx.material, false),
        };
        let mut out = match shape {
            Folded::True => return None,
            Folded::False => Vec::new(),
            Folded::Expr(e) => {
                let disjuncts = match e {
                    PolicyExpr::Any(v) => v,
                    other => vec![other],
                };
                let mut out = Vec::with_capacity(disjuncts.len());
                for d in &disjuncts {
                    out.push(self.driver_of(d)?);
                }
                out
            }
        };
        // 個別共有は空でも入口に入れる（空の配列は 0 件。共有の有無で SQL テキストを変えない・§16.3）。
        // P が FALSE で共有も無いときは `is_empty` が実行前に空を返す。
        if out.is_empty() && self.ctx.shared_ints.is_empty() {
            return None;
        }
        out.push(Driver::Shared(self.ctx.shared_ints.to_vec()));
        Some(out)
    }

    fn driver_of(&self, e: &PolicyExpr) -> Option<Driver> {
        let active = |f, t| self.ctx.active.contains(&(f, t));
        match e {
            PolicyExpr::IsOwner if active(SYSTEM_FIELD_OWNER, IndexTable::Text) => {
                Some(Driver::Owner(self.ctx.user.to_string()))
            }
            // 積は 1 項で駆動し（上位集合）、残りは Guard が検査する。
            PolicyExpr::All(v) => v.iter().find_map(|c| self.driver_of(c)),
            PolicyExpr::FieldCmp { field, op, value } => {
                let f = self.ctx.schema.field(field)?;
                let numeric = f.field_type == FieldType::Number;
                let table = if numeric {
                    IndexTable::Num
                } else {
                    IndexTable::Text
                };
                if !active(f.id, table) {
                    return None;
                }
                match (op, value, numeric) {
                    (CmpOp::Eq, PolicyOperand::UserId, false) => Some(Driver::Text {
                        field: f.id,
                        vals: vec![self.ctx.user.to_string()],
                    }),
                    (CmpOp::In, PolicyOperand::UserRoles { subtree }, false) => {
                        Some(Driver::Text {
                            field: f.id,
                            vals: self.ctx.material.roles(*subtree).to_vec(),
                        })
                    }
                    (CmpOp::Eq, PolicyOperand::Lit(v), false) => Some(Driver::Text {
                        field: f.id,
                        vals: vec![v.as_str()?.to_string()],
                    }),
                    (CmpOp::In, PolicyOperand::Lit(v), false) => Some(Driver::Text {
                        field: f.id,
                        vals: v
                            .as_array()?
                            .iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect(),
                    }),
                    (CmpOp::Eq, PolicyOperand::Lit(v), true) => Some(Driver::Num {
                        field: f.id,
                        val: crate::numeric::canonical_number(v.as_number()?)?,
                    }),
                    // ne は値の無い行を含まない範囲の補集合で、索引の範囲にならない。
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// `HasRole` / `Public` を定数に畳み込み、`Any` / `All` を簡約する（それ以外の書き換えはしない）。
/// `roles` が真なら、所属ロールが空の `UserRoles` 比較も不成立に畳む（空判定・全行可視の判定用）。
fn fold(e: &PolicyExpr, m: &crate::policy::material::PolicyMaterial, roles: bool) -> Folded {
    match e {
        PolicyExpr::Public => Folded::True,
        PolicyExpr::HasRole { role, subtree } => {
            if m.has_role(role, *subtree) {
                Folded::True
            } else {
                Folded::False
            }
        }
        PolicyExpr::FieldCmp {
            value: PolicyOperand::UserRoles { subtree },
            ..
        } if roles && m.roles(*subtree).is_empty() => Folded::False,
        PolicyExpr::Any(v) => {
            let mut kids = Vec::new();
            for c in v {
                match fold(c, m, roles) {
                    Folded::True => return Folded::True,
                    Folded::False => {}
                    Folded::Expr(x) => kids.push(x),
                }
            }
            match kids.len() {
                0 => Folded::False,
                1 => Folded::Expr(kids.remove(0)),
                _ => Folded::Expr(PolicyExpr::Any(kids)),
            }
        }
        PolicyExpr::All(v) => {
            let mut kids = Vec::new();
            for c in v {
                match fold(c, m, roles) {
                    Folded::False => return Folded::False,
                    Folded::True => {}
                    Folded::Expr(x) => kids.push(x),
                }
            }
            match kids.len() {
                0 if v.is_empty() => Folded::False,
                0 => Folded::True,
                1 => Folded::Expr(kids.remove(0)),
                _ => Folded::Expr(PolicyExpr::All(kids)),
            }
        }
        other => Folded::Expr(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::{FieldDef, FieldId, TableSchema};
    use crate::policy::ast::RowPolicy;
    use crate::policy::material::PolicyMaterial;
    use std::collections::HashSet;
    use std::sync::Arc;

    fn schema(read: PolicyExpr) -> TableSchema {
        let mut a = FieldDef::new("applicant", FieldType::UserRef);
        a.indexed = true;
        let mut s = TableSchema {
            fields: vec![a],
            status_field: None,
            row_policy: Some(RowPolicy { read, write: None }),
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        s
    }

    fn m(roles: &[&str]) -> PolicyMaterial {
        PolicyMaterial {
            roles_direct: Some(Arc::new(vec![])),
            roles_effective: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
            shared_record_ids: vec![],
            shares_truncated: false,
            role_members: std::collections::HashMap::new(),
        }
    }

    fn policy() -> PolicyExpr {
        PolicyExpr::Any(vec![
            PolicyExpr::FieldCmp {
                field: "applicant".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        ])
    }

    #[test]
    fn drivers_follow_material() {
        let s = schema(policy());
        let active: HashSet<(FieldId, IndexTable)> = [(16, IndexTable::Text)].into();
        let mat = m(&[]);
        let g = Guard::new(IndexGuardCtx {
            schema: &s,
            material: &mat,
            user: "bob",
            active: &active,
            shared_ints: &[3],
        });
        let d = g.drivers().unwrap();
        assert_eq!(d.len(), 2, "applicant=bob と共有");
        // 共有が無くても入口の形は同じ（空の共有の入口を持つ）。
        let g0 = Guard::new(IndexGuardCtx {
            schema: &s,
            material: &mat,
            user: "bob",
            active: &active,
            shared_ints: &[],
        });
        assert_eq!(g0.drivers().unwrap().len(), 2);
        assert!(!g.is_unrestricted() && !g.is_empty());
        // 経理ロールなら P は TRUE（全行可視）で認可駆動は候補にならない。
        let keiri = m(&["keiri"]);
        let g = Guard::new(IndexGuardCtx {
            schema: &s,
            material: &keiri,
            user: "c",
            active: &active,
            shared_ints: &[],
        });
        assert!(g.is_unrestricted());
        assert!(g.drivers().is_none());
    }

    #[test]
    fn ne_and_inactive_are_not_drivable_but_guard_is_still_lowered() {
        let s = schema(PolicyExpr::FieldCmp {
            field: "applicant".into(),
            op: CmpOp::Ne,
            value: PolicyOperand::UserId,
        });
        let active: HashSet<(FieldId, IndexTable)> = HashSet::new();
        let mat = m(&[]);
        let g = Guard::new(IndexGuardCtx {
            schema: &s,
            material: &mat,
            user: "bob",
            active: &active,
            shared_ints: &[],
        });
        assert!(g.drivers().is_none());
        let mut sql = SqlBuf::scoped(1, 2);
        let out = g.lower("d.record_int", &mut sql).unwrap();
        assert!(out.as_str().contains("<>"), "{}", out.as_str());
    }
}
