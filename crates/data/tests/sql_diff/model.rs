//! 生成テストの 1 件（表の形・行・SQL）と、その SQL のテキスト化。
//!
//! 同じ [`Case`] から、Shiki の SQL 前段面へ渡すテキストと素の PostgreSQL へ渡すテキストを作る。
//! 違うのは表名と、[`super::allow`] に書いた「わざと違えている箇所」の書き換えだけ。

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// 列の型（Shiki の型・PostgreSQL の型は [`Ty::pg`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ty {
    Text,
    Number,
    /// `YYYY-MM-DD`（Shiki は文字列のまま保存し、辞書順＝日付順で比べる）。
    Date,
    Select,
}

impl Ty {
    pub fn numeric(self) -> bool {
        self == Ty::Number
    }

    /// 参照表の列型。文字列は Shiki の索引（`collate "C"`）と同じ照合順にする。
    pub fn pg(self) -> &'static str {
        match self {
            Ty::Number => "numeric",
            Ty::Text | Ty::Date | Ty::Select => "text COLLATE \"C\"",
        }
    }
}

/// 列（`k` は必ずある一意の整数列で、全順序の並べ替えに使う）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Col {
    pub name: String,
    pub ty: Ty,
    /// Shiki で値索引を張るか（張ると索引から読む戦略が選ばれ得る）。
    pub indexed: bool,
}

/// セルの値（数値は 10 進表記のまま持つ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Val {
    Str(String),
    Num(String),
}

/// 比較演算子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    pub const ALL: [Op; 6] = [Op::Eq, Op::Ne, Op::Lt, Op::Le, Op::Gt, Op::Ge];

    pub fn sql(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "<>",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
        }
    }
}

/// 条件式（SQL の 3 値論理のまま）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Pred {
    Cmp(String, Op, Val),
    IsNull(String, bool),
    In(String, Vec<Val>, bool),
    Between(String, Val, Val, bool),
    /// 列・パターン・否定・ILIKE か。
    Like(String, String, bool, bool),
    Const(bool),
    /// リテラルどうしの比較（`4 = 5` など）。
    LitCmp(i64, Op, i64),
    Not(Box<Pred>),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    /// `(p) IS [NOT] NULL`。
    Unknown(Box<Pred>, bool),
    /// `(p) IS [NOT] TRUE / FALSE`（値・否定）。
    Truth(Box<Pred>, bool, bool),
}

/// 集計関数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Func {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl Func {
    pub fn sql(self) -> &'static str {
        match self {
            Func::Count => "COUNT",
            Func::Sum => "SUM",
            Func::Avg => "AVG",
            Func::Min => "MIN",
            Func::Max => "MAX",
        }
    }
}

/// 集計（`arg = None` は `COUNT(*)`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agg {
    pub func: Func,
    pub arg: Option<String>,
}

/// 問い合わせ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Query {
    /// 行を読む。`order` は (列, 降順か)。末尾に `k` があれば全順序。
    Rows {
        cols: Vec<String>,
        filter: Option<Pred>,
        order: Vec<(String, bool)>,
        limit: Option<u32>,
    },
    /// 集計する（グループ化するなら先頭の列がグループの値・グループの昇順で並べる）。
    Agg {
        group: Option<String>,
        aggs: Vec<Agg>,
        filter: Option<Pred>,
        /// `HAVING COUNT(*) <op> n`。
        having: Option<(Op, i64)>,
    },
}

/// 生成テストの 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    /// 先頭は必ず `k`（一意の整数・値なしにならない）。
    pub cols: Vec<Col>,
    /// 行（`cols` と同じ並び・`None` は値なし）。
    pub rows: Vec<Vec<Option<Val>>>,
    pub query: Query,
}

/// どちらへ渡すテキストか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Shiki,
    Pg,
}

pub fn lit(v: &Val) -> String {
    match v {
        Val::Str(s) => format!("'{}'", s.replace('\'', "''")),
        Val::Num(n) => n.clone(),
    }
}

fn op_sql(op: Op) -> &'static str {
    op.sql()
}

/// 条件式のテキスト（両側で同じ）。
pub fn pred_sql(p: &Pred) -> String {
    match p {
        Pred::Cmp(c, op, v) => format!("{c} {} {}", op_sql(*op), lit(v)),
        Pred::IsNull(c, neg) => format!("{c} IS {}NULL", if *neg { "NOT " } else { "" }),
        Pred::In(c, vs, neg) => format!(
            "{c} {}IN ({})",
            if *neg { "NOT " } else { "" },
            vs.iter().map(lit).collect::<Vec<_>>().join(", ")
        ),
        Pred::Between(c, lo, hi, neg) => format!(
            "{c} {}BETWEEN {} AND {}",
            if *neg { "NOT " } else { "" },
            lit(lo),
            lit(hi)
        ),
        Pred::Like(c, pat, neg, ilike) => format!(
            "{c} {}{} {}",
            if *neg { "NOT " } else { "" },
            if *ilike { "ILIKE" } else { "LIKE" },
            lit(&Val::Str(pat.clone()))
        ),
        Pred::Const(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Pred::LitCmp(a, op, b) => format!("{a} {} {b}", op_sql(*op)),
        Pred::Not(x) => format!("NOT ({})", pred_sql(x)),
        Pred::And(a, b) => format!("({}) AND ({})", pred_sql(a), pred_sql(b)),
        Pred::Or(a, b) => format!("({}) OR ({})", pred_sql(a), pred_sql(b)),
        Pred::Unknown(x, neg) => {
            format!(
                "({}) IS {}NULL",
                pred_sql(x),
                if *neg { "NOT " } else { "" }
            )
        }
        Pred::Truth(x, v, neg) => format!(
            "({}) IS {}{}",
            pred_sql(x),
            if *neg { "NOT " } else { "" },
            if *v { "TRUE" } else { "FALSE" }
        ),
    }
}

fn agg_sql(a: &Agg) -> String {
    format!("{}({})", a.func.sql(), a.arg.as_deref().unwrap_or("*"))
}

/// 出力列の別名（両側で同じ・集計の名前の違いに左右されない）。
pub fn out_name(i: usize) -> String {
    format!("o{i}")
}

impl Case {
    pub fn col(&self, name: &str) -> Option<&Col> {
        self.cols.iter().find(|c| c.name == name)
    }

    /// 出力列が数値か（比べるときに数値として正規化する）。
    pub fn out_numeric(&self) -> Vec<bool> {
        let num = |c: &str| self.col(c).is_some_and(|c| c.ty.numeric());
        match &self.query {
            Query::Rows { cols, .. } => cols.iter().map(|c| num(c)).collect(),
            Query::Agg { group, aggs, .. } => group
                .iter()
                .map(|g| num(g))
                .chain(aggs.iter().map(|a| match a.func {
                    Func::Count | Func::Sum | Func::Avg => true,
                    Func::Min | Func::Max => a.arg.as_deref().is_some_and(num),
                }))
                .collect(),
        }
    }

    /// 結果の並びが一意に決まるか（決まらなければ多重集合として比べる）。
    pub fn ordered(&self) -> bool {
        match &self.query {
            Query::Rows { order, .. } => order.last().is_some_and(|(c, _)| c == "k"),
            Query::Agg { group, .. } => group.is_some(),
        }
    }

    /// SQL テキスト。`table` は表名、`side` で [`super::allow`] の書き換えを選ぶ。
    pub fn sql(&self, table: &str, side: Side) -> String {
        // 参照側は出力を text にして受け取る（型の違いに左右されず、比べる前に正規化する）。
        let item = |expr: String, i: usize| match side {
            Side::Shiki => format!("{expr} AS {}", out_name(i)),
            Side::Pg => format!("({expr})::text AS {}", out_name(i)),
        };
        match &self.query {
            Query::Rows {
                cols,
                filter,
                order,
                limit,
            } => {
                let items: Vec<String> = cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| item(c.clone(), i))
                    .collect();
                let mut s = format!("SELECT {} FROM {table}", items.join(", "));
                if let Some(p) = filter {
                    let _ = write!(s, " WHERE {}", pred_sql(p));
                }
                if !order.is_empty() {
                    let keys: Vec<String> = order
                        .iter()
                        .map(|(c, desc)| super::allow::order_key(c, *desc, side))
                        .collect();
                    let _ = write!(s, " ORDER BY {}", keys.join(", "));
                }
                if let Some(n) = limit {
                    let _ = write!(s, " LIMIT {n}");
                }
                s
            }
            Query::Agg {
                group,
                aggs,
                filter,
                having,
            } => {
                let mut items = Vec::new();
                if let Some(g) = group {
                    items.push(item(g.clone(), 0));
                }
                let off = items.len();
                for (i, a) in aggs.iter().enumerate() {
                    items.push(item(agg_sql(a), off + i));
                }
                let mut s = format!("SELECT {} FROM {table}", items.join(", "));
                if let Some(p) = filter {
                    let _ = write!(s, " WHERE {}", pred_sql(p));
                }
                if let Some(g) = group {
                    let _ = write!(s, " GROUP BY {g}");
                    if let Some((op, n)) = having {
                        let _ = write!(s, " HAVING COUNT(*) {} {n}", op.sql());
                    }
                    let _ = write!(s, " ORDER BY {}", super::allow::order_key(g, false, side));
                }
                s
            }
        }
    }

    /// 参照表の DDL と行の INSERT 文。
    pub fn pg_setup(&self, table: &str) -> Vec<String> {
        let cols: Vec<String> = self
            .cols
            .iter()
            .map(|c| format!("{} {}", c.name, c.ty.pg()))
            .collect();
        let mut out = vec![format!("CREATE TABLE {table} ({})", cols.join(", "))];
        for r in &self.rows {
            let vals: Vec<String> = r
                .iter()
                .map(|v| v.as_ref().map_or_else(|| "NULL".into(), lit))
                .collect();
            out.push(format!("INSERT INTO {table} VALUES ({})", vals.join(", ")));
        }
        out
    }

    /// 人が読む形（再現の保存・失敗の表示用）。
    pub fn describe(&self) -> String {
        let mut s = format!("-- SQL（Shiki 側）\n{}\n", self.sql("t", Side::Shiki));
        let _ = write!(s, "-- SQL（参照側）\n{}\n", self.sql("t", Side::Pg));
        s.push_str("-- データ\n");
        for st in self.pg_setup("t") {
            s.push_str(&st);
            s.push_str(";\n");
        }
        s
    }
}
