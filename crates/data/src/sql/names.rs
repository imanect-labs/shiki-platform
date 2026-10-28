//! SQL 前段面の名前・式の小さな部品（列の参照・テーブル名・`ON` 式・`pg_*` の拒否）。

use sqlparser::ast::{BinaryOperator, Expr, Ident, ObjectName, UnaryOperator};

use super::ast::{Col, Loc};
use super::parse::{at, unsupported};
use crate::query::reject::{QueryRejection, RejectCode};

type R<T> = Result<T, QueryRejection>;

/// 括弧を外す。
pub(crate) fn strip(e: &Expr) -> &Expr {
    let mut e = e;
    while let Expr::Nested(x) = e {
        e = x;
    }
    e
}

/// 条件式（真偽を返す式）か。
pub(crate) fn is_predicate(e: &Expr) -> bool {
    matches!(
        strip(e),
        Expr::BinaryOp { .. }
            | Expr::UnaryOp {
                op: UnaryOperator::Not,
                ..
            }
            | Expr::InList { .. }
            | Expr::Between { .. }
            | Expr::Like { .. }
            | Expr::IsNull(_)
            | Expr::IsNotNull(_)
    )
}

pub(crate) fn snippet(e: &Expr) -> String {
    let s = e.to_string();
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s
    }
}

pub(crate) fn ident_text(i: &Ident) -> String {
    i.value.clone()
}

/// `pg_*`（システムカタログ）の名前（存在に依らず同じ拒否）。
fn system_name(s: &str) -> bool {
    s.to_ascii_lowercase().starts_with("pg_") || s.eq_ignore_ascii_case("information_schema")
}

fn pg_reject(l: Loc) -> QueryRejection {
    unsupported(
        "システムカタログ（pg_* / information_schema）は参照できません",
        l,
    )
}

/// 1 要素の名前（`schema.table` は受け付けない）。
pub(crate) fn object_name(n: &ObjectName) -> R<(String, bool)> {
    let parts: Vec<&Ident> = n.0.iter().filter_map(|p| p.as_ident()).collect();
    if parts.len() != n.0.len() || parts.is_empty() {
        return Err(unsupported("この名前の書き方は使えません", at(n)));
    }
    if parts.iter().any(|p| system_name(&p.value)) {
        return Err(pg_reject(at(n)));
    }
    let [one] = parts.as_slice() else {
        return Err(unsupported(
            "スキーマ修飾（schema.table）は使えません",
            at(n),
        ));
    };
    Ok((one.value.clone(), one.quote_style.is_some()))
}

/// 列の参照（`列` / `別名.列`）。
pub(crate) fn col_of(e: &Expr, what: &str) -> R<Col> {
    let col = match strip(e) {
        Expr::Identifier(i) => Col {
            qual: None,
            name: i.value.clone(),
            quoted: i.quote_style.is_some(),
            loc: at(e),
        },
        Expr::CompoundIdentifier(v) => match v.as_slice() {
            [q, n] => Col {
                qual: Some(q.value.clone()),
                name: n.value.clone(),
                quoted: n.quote_style.is_some(),
                loc: at(e),
            },
            _ => {
                return Err(unsupported(
                    "列は `列` か `別名.列` で書いてください",
                    at(e),
                ))
            }
        },
        Expr::Cast { .. } => return Err(unsupported("型キャスト（CAST / ::）は使えません", at(e))),
        Expr::Function(f) => {
            let (name, _) = object_name(&f.name)?;
            return Err(unsupported(
                format!("関数 '{name}' はここでは使えません（{what}）"),
                at(e),
            ));
        }
        Expr::Subquery(_) | Expr::Exists { .. } => {
            return Err(unsupported("副問い合わせは使えません", at(e)))
        }
        _ => return Err(unsupported(what.to_string(), at(e))),
    };
    if system_name(&col.name) || col.qual.as_deref().is_some_and(system_name) {
        return Err(pg_reject(col.loc));
    }
    Ok(col)
}

/// `ON 左 = 右`（等値 1 個・両辺とも `別名.列`）。
pub(crate) fn join_on(e: &Expr) -> R<(Col, Col)> {
    let bad = || {
        QueryRejection::new(
            RejectCode::InvalidJoin,
            "ON に書けるのは宣言済みリンク列の等値 1 個だけです（追加の条件は WHERE に書いてください）",
        )
        .at(at(e))
        .hint("例: JOIN customers c ON o.customer = c._id（リンク列 = 参照先の _id）")
    };
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    } = strip(e)
    else {
        return Err(bad());
    };
    let side = |x: &Expr| -> R<Col> {
        match strip(x) {
            Expr::CompoundIdentifier(v) if v.len() == 2 => col_of(x, ""),
            _ => Err(bad()),
        }
    };
    Ok((side(left)?, side(right)?))
}
