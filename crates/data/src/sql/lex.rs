//! 字句の段階の検査（パーサへ渡す前・data-platform §16.1.1）。

use sqlparser::tokenizer::{Token, TokenWithSpan};

use super::parse::{loc, unsupported};
use crate::query::limits::QueryLimits;
use crate::query::reject::QueryRejection;

type R<T> = Result<T, QueryRejection>;

/// 括弧の入れ子と関数呼び出しの入れ子を字句の段階で数える（パーサへ渡す前・§16.1.1）。
///
/// sqlparser は関数呼び出しの入れ子や再帰上限付近の入れ子で投機的解析の後戻りが指数的に増える
/// （`lower(lower(…))` の 16 段で数百 ms）。部分集合では関数（集計）は入れ子にならないので、関数の
/// 入れ子は字句の段階で拒否し、括弧の入れ子は `max_ast_depth` で切る。
pub(crate) fn nesting(tokens: &[TokenWithSpan], l: &QueryLimits) -> R<()> {
    const STRUCTURAL: &[&str] = &[
        "AND",
        "OR",
        "NOT",
        "IN",
        "EXISTS",
        "ON",
        "WHERE",
        "HAVING",
        "BY",
        "SELECT",
        "FROM",
        "JOIN",
        "AS",
        "BETWEEN",
        "IS",
        "LIKE",
        "ILIKE",
        "WHEN",
        "THEN",
        "ELSE",
        "CASE",
        "VALUES",
        "ALL",
        "ANY",
        "SOME",
        "USING",
        "LIMIT",
        "OFFSET",
        "OVER",
        "FILTER",
        "WITHIN",
        "ROW",
        "ROWS",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "DISTINCT",
        "END",
        "ASC",
        "DESC",
        "LATERAL",
        "TABLE",
        "INTO",
        "SET",
        "RETURNING",
        "WITH",
        "AND",
        "INNER",
        "LEFT",
        "RIGHT",
        "FULL",
        "CROSS",
        "OUTER",
        "NATURAL",
    ];
    let mut stack: Vec<bool> = Vec::new();
    let mut fn_depth = 0usize;
    let mut prev: Option<&Token> = None;
    for t in tokens {
        match &t.token {
            Token::Whitespace(_) => continue,
            Token::LParen | Token::LBracket => {
                let is_fn = matches!(prev, Some(Token::Word(w))
                    if w.quote_style.is_some()
                        || !STRUCTURAL.iter().any(|k| w.value.eq_ignore_ascii_case(k)));
                stack.push(is_fn);
                if stack.len() > l.max_ast_depth {
                    return Err(l.depth());
                }
                if is_fn {
                    fn_depth += 1;
                    if fn_depth > 1 {
                        return Err(unsupported(
                            "関数の入れ子（関数の引数の中の関数）は使えません",
                            loc(t.span.start),
                        ));
                    }
                }
            }
            Token::RParen | Token::RBracket if stack.pop() == Some(true) => {
                fn_depth = fn_depth.saturating_sub(1);
            }
            _ => {}
        }
        prev = Some(&t.token);
    }
    Ok(())
}
