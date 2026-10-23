//! パースと allowlist 検証（data-platform §16.1 ①②）: SQL テキスト → [`super::ast::Select`]。
//!
//! **閉じた部分集合だけを写す**。sqlparser の AST のうち、ここで明示的に受け付けた形だけが
//! [`super::ast`] に移り、それ以外（DML / DDL・CTE・ウィンドウ関数・副問い合わせ・`UNION`・未許可の関数・
//! `pg_*`・型キャスト・`SELECT *`・`OFFSET`・`DISTINCT` …）は理由つきの 422 になる。
//! `crates/tabular/src/sql_guard.rs` の「Query を通して危険関数を denylist で弾く」方式とは**逆**で、
//! 列挙していない構文・関数は通らない（IR 変換自体が唯一の壁なので）。
//!
//! 入口の資源上限（§16.1.1）: 長さ（パーサへ渡す前）→ トークン数・リテラル数（字句解析の後・AST を作る前）→
//! 入れ子の深さ（パーサの再帰上限と、写した条件木の深さ）→ ノード数・リテラル数（写した後）。

use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr,
    JoinConstraint, JoinOperator, LimitClause, OrderByKind, Query, SelectFlavor, SelectItem,
    SetExpr, Spanned, Statement, TableFactor, UnaryOperator, Value,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::{Parser, ParserError};
use sqlparser::tokenizer::{Location, Token, Tokenizer};

use super::ast::{Agg, BExpr, Cmp, Item, JoinAst, Lit, Loc, Operand, OrdTarget, Select, TableRef};
use super::names::{col_of, ident_text, is_predicate, join_on, object_name, snippet, strip};
use crate::query::ir::Metric;
use crate::query::limits::QueryLimits;
use crate::query::reject::{QueryRejection, RejectCode, SqlLocation};

type R<T> = Result<T, QueryRejection>;

/// 受け付ける集計関数（allowlist）。
pub(crate) const ALLOWED_FUNCTIONS: &[&str] = &["COUNT", "SUM", "AVG", "MIN", "MAX"];

pub(crate) fn loc(l: Location) -> Loc {
    (l.line > 0).then_some(SqlLocation {
        line: l.line,
        column: l.column,
    })
}

pub(crate) fn at<T: Spanned>(n: &T) -> Loc {
    loc(n.span().start)
}

pub(crate) fn unsupported(what: impl Into<String>, l: Loc) -> QueryRejection {
    QueryRejection::new(RejectCode::Unsupported, what).at(l)
}

/// エラー文言から位置（`Line: n, Column: m`）を拾う。
fn loc_in(msg: &str) -> Loc {
    let line = msg
        .split("Line: ")
        .nth(1)?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()?;
    let column = msg
        .split("Column: ")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some(SqlLocation { line, column })
}

fn syntax(msg: &str) -> QueryRejection {
    QueryRejection::new(
        RejectCode::SyntaxError,
        format!("SQL として読めません: {msg}"),
    )
    .at(loc_in(msg))
    .hint("SELECT 列 FROM テーブル [JOIN …] [WHERE …] [GROUP BY …] [HAVING …] [ORDER BY …] [LIMIT n] の形で書いてください")
}

/// SQL テキストを閉じた部分集合の AST にする。
pub(crate) fn parse(sql: &str, l: &QueryLimits) -> R<Select> {
    if sql.len() > l.max_sql_bytes {
        return Err(l.bytes());
    }
    let dialect = PostgreSqlDialect {};
    // AST を作る前に字句の数で弾く（巨大な IN リスト・横に広い式のメモリを確保させない）。
    let tokens = Tokenizer::new(&dialect, sql)
        .tokenize_with_location()
        .map_err(|e| syntax(&e.to_string()))?;
    let mut n_tokens = 0usize;
    let mut n_literals = 0usize;
    for t in &tokens {
        match &t.token {
            Token::Whitespace(_) | Token::EOF => {}
            Token::Number(..)
            | Token::SingleQuotedString(_)
            | Token::DoubleQuotedString(_)
            | Token::NationalStringLiteral(_)
            | Token::EscapedStringLiteral(_)
            | Token::HexStringLiteral(_)
            | Token::DollarQuotedString(_) => {
                n_tokens += 1;
                n_literals += 1;
            }
            _ => n_tokens += 1,
        }
    }
    if n_literals > l.max_literals {
        return Err(l.literals());
    }
    if n_tokens > l.max_ast_nodes {
        return Err(l.nodes());
    }
    super::lex::nesting(&tokens, l)?;
    // 入れ子は字句の段階で上限内に収めてあるので、パーサの再帰上限には余裕を持たせる（上限付近で
    // 投機的解析が再帰上限に当たって後戻りを繰り返すと指数時間になる）。
    let stmts = Parser::new(&dialect)
        .with_recursion_limit(l.max_ast_depth * 8 + 64)
        .with_tokens_with_locations(tokens)
        .parse_statements()
        .map_err(|e| match e {
            ParserError::RecursionLimitExceeded => l.depth(),
            ParserError::TokenizerError(m) | ParserError::ParserError(m) => syntax(&m),
        })?;
    let [stmt] = stmts.as_slice() else {
        return Err(unsupported(
            "1 回に受け付ける文は SELECT 1 つだけです",
            None,
        ));
    };
    let Statement::Query(q) = stmt else {
        return Err(unsupported(
            "SELECT 文だけを受け付けます（INSERT / UPDATE / DELETE / DDL などは使えません）",
            at(stmt),
        ));
    };
    let mut c = Conv {
        l,
        nodes: 0,
        literals: 0,
    };
    let sel = c.query(q)?;
    let depth = sel
        .filter
        .iter()
        .chain(sel.having.iter())
        .map(depth_of)
        .max()
        .unwrap_or(0);
    if depth > l.max_ast_depth {
        return Err(l.depth());
    }
    if c.nodes > l.max_ast_nodes {
        return Err(l.nodes());
    }
    if c.literals > l.max_literals {
        return Err(l.literals());
    }
    Ok(sel)
}

/// 条件木の深さ（同じ演算子の連鎖は 1 段・括弧は数えない）。
fn depth_of(e: &BExpr) -> usize {
    match e {
        BExpr::And(v) | BExpr::Or(v) => 1 + v.iter().map(depth_of).max().unwrap_or(0),
        BExpr::Not(x) | BExpr::Unknown { inner: x, .. } | BExpr::Truth { inner: x, .. } => {
            1 + depth_of(x)
        }
        _ => 1,
    }
}

/// AST の写し（ノード数・リテラル数を数える）。
struct Conv<'l> {
    l: &'l QueryLimits,
    nodes: usize,
    literals: usize,
}

impl Conv<'_> {
    fn node(&mut self) -> R<()> {
        self.nodes += 1;
        if self.nodes > self.l.max_ast_nodes {
            return Err(self.l.nodes());
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // 句ごとの許可・拒否を 1 か所に並べて漏れを見つけやすくする。
    fn query(&mut self, q: &Query) -> R<Select> {
        if q.with.is_some() {
            return Err(unsupported("WITH（CTE）は使えません", at(q)));
        }
        if q.fetch.is_some()
            || !q.locks.is_empty()
            || q.for_clause.is_some()
            || q.settings.is_some()
            || q.format_clause.is_some()
            || !q.pipe_operators.is_empty()
        {
            return Err(unsupported(
                "FETCH / FOR UPDATE / SETTINGS などは使えません",
                at(q),
            ));
        }
        let sel = match q.body.as_ref() {
            SetExpr::Select(s) => s,
            SetExpr::SetOperation { .. } => {
                return Err(unsupported(
                    "UNION / INTERSECT / EXCEPT は使えません",
                    at(q.body.as_ref()),
                ))
            }
            SetExpr::Query(_) => {
                return Err(unsupported("副問い合わせは使えません", at(q.body.as_ref())))
            }
            _ => {
                return Err(unsupported(
                    "SELECT 以外の問い合わせ（VALUES など）は使えません",
                    at(q.body.as_ref()),
                ))
            }
        };
        if sel.distinct.is_some() {
            return Err(unsupported("DISTINCT は使えません", at(sel.as_ref()))
                .hint("重複を除くには GROUP BY を使ってください"));
        }
        if sel.top.is_some()
            || sel.into.is_some()
            || !sel.lateral_views.is_empty()
            || sel.prewhere.is_some()
            || !sel.connect_by.is_empty()
            || !sel.cluster_by.is_empty()
            || !sel.distribute_by.is_empty()
            || !sel.sort_by.is_empty()
            || !sel.named_window.is_empty()
            || sel.qualify.is_some()
            || sel.value_table_mode.is_some()
            || sel.exclude.is_some()
            || sel.select_modifiers.is_some()
            || !sel.optimizer_hints.is_empty()
            || sel.flavor != SelectFlavor::Standard
        {
            return Err(unsupported(
                "この SELECT の書き方（TOP / INTO / WINDOW / QUALIFY / 方言の拡張など）は使えません",
                at(sel.as_ref()),
            ));
        }
        let [from] = sel.from.as_slice() else {
            return Err(unsupported(
                if sel.from.is_empty() {
                    "FROM にテーブルを 1 つ指定してください"
                } else {
                    "FROM に並べられるテーブルは 1 つです（結合は JOIN … ON で書いてください）"
                },
                at(sel.as_ref()),
            ));
        };
        let from_ref = self.table(&from.relation)?;
        let mut joins = Vec::with_capacity(from.joins.len());
        for j in &from.joins {
            self.node()?;
            let on = match &j.join_operator {
                JoinOperator::Join(JoinConstraint::On(e))
                | JoinOperator::Inner(JoinConstraint::On(e))
                    if !j.global =>
                {
                    e
                }
                JoinOperator::Join(_) | JoinOperator::Inner(_) => {
                    return Err(QueryRejection::new(
                        RejectCode::InvalidJoin,
                        "JOIN には ON（宣言済みリンク列の等値 1 個）が要ります",
                    )
                    .at(at(&j.relation))
                    .hint("例: JOIN customers c ON o.customer = c._id"))
                }
                _ => {
                    return Err(unsupported(
                        "使える結合は INNER JOIN（JOIN）だけです（LEFT / RIGHT / FULL / CROSS / NATURAL / USING は使えません）",
                        at(&j.relation),
                    ))
                }
            };
            let table = self.table(&j.relation)?;
            let (left, right) = join_on(on)?;
            joins.push(JoinAst {
                loc: at(&j.relation),
                table,
                left,
                right,
            });
        }
        let mut items = Vec::with_capacity(sel.projection.len());
        for it in &sel.projection {
            self.node()?;
            let (e, alias) = match it {
                SelectItem::UnnamedExpr(e) => (e, None),
                SelectItem::ExprWithAlias { expr, alias } => (expr, Some(ident_text(alias))),
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {
                    return Err(unsupported(
                        "SELECT * は使えません（返す列を明示してください）",
                        at(it),
                    ))
                }
                SelectItem::ExprWithAliases { .. } => {
                    return Err(unsupported("この射影は使えません", at(it)))
                }
            };
            items.push(match strip(e) {
                Expr::Function(_) => Item::Agg {
                    agg: self.agg(e)?,
                    alias,
                },
                _ => Item::Col {
                    col: col_of(
                        e,
                        "射影に使えるのは列と集計関数（COUNT / SUM / AVG / MIN / MAX）だけです",
                    )?,
                    alias,
                },
            });
        }
        let filter = match &sel.selection {
            None => None,
            Some(e) => Some(self.bexpr(e, false)?),
        };
        let group_by = match &sel.group_by {
            GroupByExpr::Expressions(v, mods) if mods.is_empty() => {
                let mut out = Vec::with_capacity(v.len());
                for e in v {
                    self.node()?;
                    out.push(col_of(e, "GROUP BY に書けるのは列だけです")?);
                }
                out
            }
            other => return Err(unsupported("この GROUP BY は使えません", at(other))),
        };
        let having = match &sel.having {
            None => None,
            Some(e) => Some(self.bexpr(e, true)?),
        };
        let mut order_by = Vec::new();
        if let Some(ob) = &q.order_by {
            if ob.interpolate.is_some() {
                return Err(unsupported("INTERPOLATE は使えません", None));
            }
            let OrderByKind::Expressions(v) = &ob.kind else {
                return Err(unsupported("ORDER BY ALL は使えません", None));
            };
            for o in v {
                self.node()?;
                if o.with_fill.is_some() {
                    return Err(unsupported("WITH FILL は使えません", at(&o.expr)));
                }
                if o.options.nulls_first == Some(true) {
                    return Err(unsupported(
                        "NULLS FIRST は使えません（値の無い行は昇順・降順とも末尾）",
                        at(&o.expr),
                    ));
                }
                let target = match strip(&o.expr) {
                    Expr::Function(_) => OrdTarget::Agg(self.agg(&o.expr)?),
                    Expr::Value(v) => match &v.value {
                        Value::Number(n, _) => OrdTarget::Pos(n.parse().map_err(|_| {
                            unsupported("ORDER BY の位置は 1 以上の整数です", at(&o.expr))
                        })?),
                        _ => {
                            return Err(unsupported(
                                "ORDER BY に書けるのは列・集計関数・射影の位置だけです",
                                at(&o.expr),
                            ))
                        }
                    },
                    e => OrdTarget::Col(col_of(
                        e,
                        "ORDER BY に書けるのは列・集計関数・射影の位置だけです",
                    )?),
                };
                order_by.push((target, o.options.asc == Some(false), at(&o.expr)));
            }
        }
        let limit = match &q.limit_clause {
            None => None,
            Some(LimitClause::LimitOffset {
                limit,
                offset,
                limit_by,
            }) => {
                if offset.is_some() {
                    return Err(unsupported("OFFSET は使えません", at(q)).hint(
                        "続きは応答の next_cursor を使って読んでください（keyset ページング）",
                    ));
                }
                if !limit_by.is_empty() {
                    return Err(unsupported("LIMIT BY は使えません", at(q)));
                }
                match limit {
                    None => None,
                    Some(e) => match strip(e) {
                        Expr::Value(v) => match &v.value {
                            Value::Number(n, _) => Some(n.parse::<u64>().map_err(|_| {
                                unsupported("LIMIT は 0 以上の整数で指定してください", at(e))
                            })?),
                            _ => {
                                return Err(unsupported(
                                    "LIMIT は整数のリテラルで指定してください",
                                    at(e),
                                ))
                            }
                        },
                        _ => {
                            return Err(unsupported(
                                "LIMIT は整数のリテラルで指定してください",
                                at(e),
                            ))
                        }
                    },
                }
            }
            Some(LimitClause::OffsetCommaLimit { .. }) => {
                return Err(unsupported("OFFSET は使えません", at(q))
                    .hint("続きは応答の next_cursor を使って読んでください（keyset ページング）"))
            }
        };
        Ok(Select {
            items,
            from: from_ref,
            joins,
            filter,
            group_by,
            having,
            order_by,
            limit,
        })
    }

    fn table(&mut self, t: &TableFactor) -> R<TableRef> {
        self.node()?;
        let TableFactor::Table {
            name,
            alias,
            args,
            with_hints,
            version,
            with_ordinality,
            partitions,
            json_path,
            sample,
            index_hints,
        } = t
        else {
            return Err(unsupported(
                "FROM / JOIN に書けるのはテーブル名だけです（副問い合わせ・関数・LATERAL は使えません）",
                at(t),
            ));
        };
        if args.is_some()
            || !with_hints.is_empty()
            || version.is_some()
            || *with_ordinality
            || !partitions.is_empty()
            || json_path.is_some()
            || sample.is_some()
            || !index_hints.is_empty()
        {
            return Err(unsupported("このテーブル参照の書き方は使えません", at(t)));
        }
        let (n, quoted) = object_name(name)?;
        let alias = match alias {
            None => None,
            Some(a) => {
                if !a.columns.is_empty() {
                    return Err(unsupported("列の別名リストは使えません", at(t)));
                }
                Some(ident_text(&a.name))
            }
        };
        Ok(TableRef {
            name: n,
            quoted,
            alias,
            loc: at(t),
        })
    }

    fn agg(&mut self, e: &Expr) -> R<Agg> {
        self.node()?;
        let Expr::Function(f) = strip(e) else {
            return Err(unsupported("集計関数が必要です", at(e)));
        };
        let (name, _) = object_name(&f.name)?;
        let upper = name.to_ascii_uppercase();
        if f.over.is_some() {
            return Err(unsupported("ウィンドウ関数（OVER）は使えません", at(e)));
        }
        let func = match upper.as_str() {
            "COUNT" => Metric::Count,
            "SUM" => Metric::Sum,
            "AVG" => Metric::Avg,
            "MIN" => Metric::Min,
            "MAX" => Metric::Max,
            _ => {
                return Err(unsupported(
                    format!("関数 '{name}' は使えません（使える関数は COUNT / SUM / AVG / MIN / MAX だけです）"),
                    at(e),
                )
                .expected(ALLOWED_FUNCTIONS.iter().map(ToString::to_string).collect()))
            }
        };
        if f.uses_odbc_syntax
            || f.filter.is_some()
            || f.null_treatment.is_some()
            || !f.within_group.is_empty()
            || !matches!(f.parameters, FunctionArguments::None)
        {
            return Err(unsupported(
                "集計関数の FILTER / WITHIN GROUP などは使えません",
                at(e),
            ));
        }
        let FunctionArguments::List(list) = &f.args else {
            return Err(unsupported("集計関数の引数が不正です", at(e)));
        };
        if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
            return Err(unsupported(
                "集計関数の DISTINCT / ORDER BY などは使えません",
                at(e),
            ));
        }
        let arg = match list.args.as_slice() {
            [FunctionArg::Unnamed(FunctionArgExpr::Wildcard)] if func == Metric::Count => None,
            [FunctionArg::Unnamed(FunctionArgExpr::Expr(a))] => match strip(a) {
                // COUNT(1) は COUNT(*) と同じ。
                Expr::Value(v) if func == Metric::Count && matches!(v.value, Value::Number(..)) => {
                    None
                }
                a => Some(col_of(a, "集計関数の引数は列 1 つです")?),
            },
            _ => {
                return Err(unsupported(
                    "集計関数の引数は列 1 つ（COUNT は * も可）です",
                    at(e),
                ))
            }
        };
        Ok(Agg {
            func,
            arg,
            loc: at(e),
        })
    }

    fn lit(&mut self, e: &Expr) -> R<Option<Lit>> {
        Ok(Some(match strip(e) {
            Expr::Value(v) => {
                self.literals += 1;
                match &v.value {
                    Value::Number(n, _) => Lit::Num(n.clone()),
                    Value::SingleQuotedString(s) => Lit::Str(s.clone()),
                    Value::Boolean(b) => Lit::Bool(*b),
                    Value::Null => Lit::Null,
                    _ => return Err(unsupported(
                        "このリテラルの書き方は使えません（'文字列'・数値・TRUE / FALSE・NULL）",
                        at(e),
                    )),
                }
            }
            // DATE '2026-01-01' / TIMESTAMP '…' は文字列として受ける（列の型で正規化する）。
            Expr::TypedString(ts) => {
                self.literals += 1;
                match &ts.value.value {
                    Value::SingleQuotedString(s) => Lit::Str(s.clone()),
                    _ => return Err(unsupported("この型つきリテラルは使えません", at(e))),
                }
            }
            Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr,
            } => match strip(expr) {
                Expr::Value(v) => match &v.value {
                    Value::Number(n, _) => {
                        self.literals += 1;
                        Lit::Num(format!("-{n}"))
                    }
                    _ => return Err(unsupported("負の符号は数値にだけ付けられます", at(e))),
                },
                _ => return Err(unsupported("算術式は使えません", at(e))),
            },
            _ => return Ok(None),
        }))
    }

    fn operand(&mut self, e: &Expr, aggs: bool) -> R<Operand> {
        self.node()?;
        if let Some(l) = self.lit(e)? {
            return Ok(Operand::Lit(l));
        }
        match strip(e) {
            Expr::Function(_) if aggs => Ok(Operand::Agg(self.agg(e)?)),
            Expr::Function(f) => {
                let (name, _) = object_name(&f.name).unwrap_or_default();
                if ALLOWED_FUNCTIONS
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(&name))
                {
                    Err(unsupported(
                        "集計関数は WHERE に書けません（HAVING に書いてください）",
                        at(e),
                    ))
                } else {
                    Err(unsupported(
                        format!("関数 '{name}' は使えません（WHERE に関数は書けません）"),
                        at(e),
                    ))
                }
            }
            other => Ok(Operand::Col(col_of(
                other,
                "比較の項に書けるのは列とリテラルだけです（算術・関数・副問い合わせは使えません）",
            )?)),
        }
    }

    /// 条件式（`aggs` は HAVING＝集計関数を項に許す）。AND / OR の連鎖は平坦にする（再帰しない）。
    #[allow(clippy::too_many_lines)] // 受け付ける式の種類を 1 か所に並べる（allowlist の見通し）。
    fn bexpr(&mut self, e: &Expr, aggs: bool) -> R<BExpr> {
        self.node()?;
        let e = strip(e);
        Ok(match e {
            Expr::BinaryOp {
                op: op @ (BinaryOperator::And | BinaryOperator::Or),
                ..
            } => {
                // 左に深い連鎖を反復で平坦化する（巨大な OR 連鎖でスタックを使い切らない）。
                let mut kids = Vec::new();
                let mut stack = vec![e];
                while let Some(x) = stack.pop() {
                    match strip(x) {
                        Expr::BinaryOp { left, op: o, right } if o == op => {
                            stack.push(right);
                            stack.push(left);
                        }
                        other => kids.push(self.bexpr(other, aggs)?),
                    }
                }
                if *op == BinaryOperator::And {
                    BExpr::And(kids)
                } else {
                    BExpr::Or(kids)
                }
            }
            Expr::BinaryOp { left, op, right } => {
                let cmp = match op {
                    BinaryOperator::Eq => Cmp::Eq,
                    BinaryOperator::NotEq => Cmp::Ne,
                    BinaryOperator::Lt => Cmp::Lt,
                    BinaryOperator::LtEq => Cmp::Le,
                    BinaryOperator::Gt => Cmp::Gt,
                    BinaryOperator::GtEq => Cmp::Ge,
                    other => {
                        return Err(unsupported(
                            format!("演算子 '{other}' は使えません（使えるのは = <> < <= > >= AND OR NOT・IN・LIKE・BETWEEN・IS NULL）"),
                            at(e),
                        ))
                    }
                };
                BExpr::Cmp {
                    lhs: self.operand(left, aggs)?,
                    op: cmp,
                    rhs: self.operand(right, aggs)?,
                    loc: at(e),
                }
            }
            Expr::UnaryOp {
                op: UnaryOperator::Not,
                expr,
            } => BExpr::Not(Box::new(self.bexpr(expr, aggs)?)),
            Expr::IsNull(x) | Expr::IsNotNull(x) => {
                let negated = matches!(e, Expr::IsNotNull(_));
                if is_predicate(x) {
                    BExpr::Unknown {
                        inner: Box::new(self.bexpr(x, aggs)?),
                        negated,
                    }
                } else {
                    BExpr::ColNull {
                        col: col_of(x, "IS NULL の左辺は列か条件式です")?,
                        negated,
                    }
                }
            }
            Expr::IsTrue(x) | Expr::IsNotTrue(x) | Expr::IsFalse(x) | Expr::IsNotFalse(x) => {
                BExpr::Truth {
                    inner: Box::new(self.bexpr(x, aggs)?),
                    value: matches!(e, Expr::IsTrue(_) | Expr::IsNotTrue(_)),
                    negated: matches!(e, Expr::IsNotTrue(_) | Expr::IsNotFalse(_)),
                }
            }
            Expr::InList {
                expr,
                list,
                negated,
            } => {
                let col = col_of(expr, "IN の左辺は列です")?;
                let mut vals = Vec::with_capacity(list.len());
                for x in list {
                    self.node()?;
                    let l = self.lit(x)?.ok_or_else(|| {
                        unsupported(
                            "IN の要素はリテラルだけです（副問い合わせは使えません）",
                            at(x),
                        )
                    })?;
                    vals.push(l);
                }
                BExpr::In {
                    col,
                    list: vals,
                    negated: *negated,
                    loc: at(e),
                }
            }
            Expr::Between {
                expr,
                negated,
                low,
                high,
            } => {
                let col = col_of(expr, "BETWEEN の左辺は列です")?;
                let lo = self
                    .lit(low)?
                    .ok_or_else(|| unsupported("BETWEEN の両端はリテラルです", at(low.as_ref())))?;
                let hi = self.lit(high)?.ok_or_else(|| {
                    unsupported("BETWEEN の両端はリテラルです", at(high.as_ref()))
                })?;
                BExpr::Between {
                    col,
                    lo,
                    hi,
                    negated: *negated,
                    loc: at(e),
                }
            }
            Expr::Like {
                negated,
                any,
                expr,
                pattern,
                escape_char,
            } => {
                if *any || escape_char.is_some() {
                    return Err(unsupported("LIKE ANY / ESCAPE は使えません", at(e)));
                }
                let col = col_of(expr, "LIKE の左辺は列です")?;
                let Some(Lit::Str(p)) = self.lit(pattern)? else {
                    return Err(unsupported(
                        "LIKE のパターンは文字列のリテラルです",
                        at(pattern.as_ref()),
                    ));
                };
                BExpr::Like {
                    col,
                    pattern: p,
                    negated: *negated,
                    loc: at(e),
                }
            }
            Expr::ILike { .. } => {
                return Err(unsupported(
                    "ILIKE は使えません（大文字小文字は区別して比べます。LIKE を使ってください）",
                    at(e),
                ))
            }
            Expr::Value(v) => match &v.value {
                Value::Boolean(b) => {
                    self.literals += 1;
                    BExpr::Const(*b)
                }
                _ => {
                    return Err(unsupported(
                        "条件にはリテラル単独を書けません（TRUE / FALSE だけ）",
                        at(e),
                    ))
                }
            },
            Expr::Exists { .. } | Expr::Subquery(_) | Expr::InSubquery { .. } => {
                return Err(unsupported(
                    "副問い合わせ（EXISTS / IN (SELECT …) / 相関副問い合わせ）は使えません",
                    at(e),
                )
                .hint("リンク列で辿る条件は JOIN … ON <リンク列> = <別名>._id で書いてください"))
            }
            Expr::Cast { .. } => {
                return Err(unsupported("型キャスト（CAST / ::）は使えません", at(e)))
            }
            Expr::Function(_) => {
                self.operand(e, aggs)?;
                return Err(unsupported("関数は条件に使えません", at(e)));
            }
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                return Err(unsupported(
                    "列単独は条件になりません（列 = 値 のように比較してください）",
                    at(e),
                ))
            }
            other => {
                return Err(unsupported(
                    format!("この式は使えません: {}", snippet(other)),
                    at(other),
                ))
            }
        })
    }
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
