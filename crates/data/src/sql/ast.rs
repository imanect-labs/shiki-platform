//! SQL 前段面の閉じた部分集合の AST（[`super::parse`] が sqlparser の AST から**許可したものだけ**を写す）。
//!
//! ここに無い構文は表せない（allowlist）。名前はまだ解決していない（[`super::resolve`] が束縛する）。

use crate::query::ir::Metric;
use crate::query::reject::SqlLocation;

/// SQL テキスト上の位置（分からなければ `None`）。
pub(crate) type Loc = Option<SqlLocation>;

/// 列の参照（`列` / `別名.列`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Col {
    pub qual: Option<String>,
    pub name: String,
    /// 二重引用符で囲まれていた（大文字小文字をそのまま比べる）。
    pub quoted: bool,
    pub loc: Loc,
}

/// 集計関数（`COUNT(*)` / `SUM(列)` など・`arg = None` は `*`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Agg {
    pub func: Metric,
    pub arg: Option<Col>,
    pub loc: Loc,
}

/// リテラル。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lit {
    Str(String),
    /// 10 進表記のまま（二進誤差を持ち込まない）。
    Num(String),
    Bool(bool),
    Null,
}

/// 比較の項。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Operand {
    Col(Col),
    Agg(Agg),
    Lit(Lit),
}

/// 比較演算子。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// 条件式（SQL の 3 値論理のまま。2 値論理の IR への翻訳は [`super::translate`]）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BExpr {
    And(Vec<BExpr>),
    Or(Vec<BExpr>),
    Not(Box<BExpr>),
    Const(bool),
    Cmp {
        lhs: Operand,
        op: Cmp,
        rhs: Operand,
        loc: Loc,
    },
    In {
        col: Col,
        list: Vec<Lit>,
        negated: bool,
        loc: Loc,
    },
    /// `列 IS [NOT] NULL`。
    ColNull {
        col: Col,
        negated: bool,
    },
    Like {
        col: Col,
        pattern: String,
        negated: bool,
        loc: Loc,
    },
    Between {
        col: Col,
        lo: Lit,
        hi: Lit,
        negated: bool,
        loc: Loc,
    },
    /// `(条件) IS [NOT] NULL`（3 値論理の「不明」かどうか・TLP の 3 区分目）。
    Unknown {
        inner: Box<BExpr>,
        negated: bool,
    },
    /// `(条件) IS [NOT] TRUE / FALSE`。
    Truth {
        inner: Box<BExpr>,
        value: bool,
        negated: bool,
    },
}

/// テーブルの参照（`名前 [AS 別名]`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableRef {
    pub name: String,
    pub quoted: bool,
    pub alias: Option<String>,
    pub loc: Loc,
}

impl TableRef {
    /// 列の修飾に使う名前（別名があれば別名・無ければテーブル名）。
    pub(crate) fn qualifier(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

/// `JOIN 表 ON 左 = 右`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JoinAst {
    pub table: TableRef,
    pub left: Col,
    pub right: Col,
    pub loc: Loc,
}

/// 射影の 1 項目。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    Col { col: Col, alias: Option<String> },
    Agg { agg: Agg, alias: Option<String> },
}

/// 並べ替えの対象。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OrdTarget {
    Col(Col),
    Agg(Agg),
    /// `ORDER BY 2`（射影の位置・1 始まり）。
    Pos(usize),
}

/// 受け付けた SELECT 文。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Select {
    pub items: Vec<Item>,
    pub from: TableRef,
    pub joins: Vec<JoinAst>,
    pub filter: Option<BExpr>,
    pub group_by: Vec<Col>,
    pub having: Option<BExpr>,
    pub order_by: Vec<(OrdTarget, bool, Loc)>,
    pub limit: Option<u64>,
}
