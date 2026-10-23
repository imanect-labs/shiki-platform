#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;
use crate::query::reject::RejectCode;

fn ok(sql: &str) -> Select {
    parse(sql, &QueryLimits::default()).unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}

fn code(sql: &str) -> RejectCode {
    parse(sql, &QueryLimits::default())
        .map(|_| panic!("受け付けてはいけない: {sql}"))
        .unwrap_err()
        .code
}

fn limit(sql: &str, l: &QueryLimits) -> String {
    let e = parse(sql, l).unwrap_err();
    assert_eq!(e.code, RejectCode::LimitExceeded, "{sql}: {e:?}");
    e.limit.unwrap().name
}

#[test]
fn accepts_the_closed_subset() {
    let s = ok("SELECT o.title, c.name AS customer FROM orders o \
                JOIN customers c ON o.customer = c._id \
                WHERE o.amount >= 100 AND c.city IN ('Tokyo', 'Osaka') AND o.title LIKE 'A%' \
                ORDER BY o.amount DESC LIMIT 20");
    assert_eq!(s.joins.len(), 1);
    assert_eq!(s.limit, Some(20));
    let s = ok("SELECT status, COUNT(*), SUM(amount) AS total FROM orders \
                GROUP BY status HAVING SUM(amount) > 1000 ORDER BY total DESC LIMIT 5");
    assert_eq!(s.group_by.len(), 1);
    assert!(s.having.is_some());
    ok("SELECT title FROM orders WHERE NOT (amount < 5) AND (title = 'x') IS NULL");
    ok("SELECT title FROM orders WHERE due BETWEEN DATE '2026-01-01' AND '2026-03-31'");
    ok("SELECT 経費.title FROM 経費 WHERE 1 = 1");
}

#[test]
fn rejects_outside_the_subset_with_structured_reasons() {
    for sql in [
        // DML / DDL
        "DELETE FROM orders",
        "UPDATE orders SET title = 'x'",
        "INSERT INTO orders (title) VALUES ('x')",
        "DROP TABLE orders",
        "CREATE TABLE x (a int)",
        // CTE
        "WITH a AS (SELECT title FROM orders) SELECT title FROM a",
        // UNION
        "SELECT title FROM orders UNION SELECT name FROM customers",
        // ウィンドウ関数
        "SELECT title, row_number() OVER (ORDER BY amount) FROM orders",
        "SELECT title, SUM(amount) OVER () FROM orders",
        // 相関副問い合わせ・副問い合わせ
        "SELECT title FROM orders o WHERE EXISTS (SELECT 1 FROM customers c WHERE c._id = o.customer)",
        "SELECT title FROM orders WHERE customer IN (SELECT _id FROM customers)",
        "SELECT title FROM (SELECT title FROM orders) t",
        // 未 allowlist の関数
        "SELECT lower(title) FROM orders",
        "SELECT title FROM orders WHERE length(title) > 3",
        "SELECT COUNT(DISTINCT title) FROM orders",
        // pg_*
        "SELECT relname FROM pg_class",
        "SELECT title FROM orders WHERE pg_sleep(1) IS NULL",
        "SELECT pg_read_file('/etc/passwd') FROM orders",
        "SELECT title FROM information_schema.tables",
        // 型キャスト
        "SELECT title FROM orders WHERE amount::text = '1'",
        "SELECT title FROM orders WHERE CAST(amount AS text) = '1'",
        // SELECT * / OFFSET / DISTINCT
        "SELECT * FROM orders",
        "SELECT o.* FROM orders o",
        "SELECT title FROM orders LIMIT 10 OFFSET 10",
        "SELECT DISTINCT title FROM orders",
        // 外部結合・カンマ結合・USING
        "SELECT o.title FROM orders o LEFT JOIN customers c ON o.customer = c._id",
        "SELECT o.title FROM orders o, customers c",
        "SELECT o.title FROM orders o JOIN customers c USING (customer)",
        // 算術・連結・ILIKE・NULLS FIRST
        "SELECT title FROM orders WHERE amount + 1 > 3",
        "SELECT title FROM orders WHERE title || 'x' = 'ax'",
        "SELECT title FROM orders WHERE title ILIKE 'a%'",
        "SELECT title FROM orders ORDER BY amount NULLS FIRST",
        // 複数文
        "SELECT title FROM orders; SELECT title FROM orders",
    ] {
        let e = parse(sql, &QueryLimits::default()).unwrap_err();
        assert!(
            matches!(e.code, RejectCode::Unsupported | RejectCode::InvalidJoin),
            "{sql}: {e:?}"
        );
        assert!(!e.message.is_empty());
    }
    assert_eq!(code("SELEC title FROM orders"), RejectCode::SyntaxError);
    assert_eq!(
        code(
            "SELECT o.title FROM orders o JOIN customers c ON o.customer = c._id AND c.city = 'x'"
        ),
        RejectCode::InvalidJoin
    );
    assert_eq!(
        code("SELECT o.title FROM orders o JOIN customers c ON lower(o.customer) = c._id"),
        RejectCode::InvalidJoin
    );
    assert_eq!(
        code("SELECT o.title FROM orders o JOIN customers c ON o.customer = c._id OR TRUE"),
        RejectCode::InvalidJoin
    );
}

#[test]
fn syntax_errors_carry_a_location() {
    let e = parse(
        "SELECT title\nFROM orders WHERE amount > > 3",
        &QueryLimits::default(),
    )
    .unwrap_err();
    assert_eq!(e.code, RejectCode::SyntaxError);
    assert_eq!(e.location.map(|l| l.line), Some(2));
}

#[test]
fn each_resource_limit_rejects_with_its_name() {
    let l = QueryLimits::default();
    // MAX_SQL_BYTES
    let big = format!(
        "SELECT title FROM orders WHERE title = '{}'",
        "x".repeat(70_000)
    );
    assert_eq!(limit(&big, &l), "max_sql_bytes");
    // MAX_AST_DEPTH（括弧の入れ子と NOT の入れ子）
    let deep = format!(
        "SELECT title FROM orders WHERE {}amount = 1{}",
        "(".repeat(60),
        ")".repeat(60)
    );
    assert_eq!(limit(&deep, &l), "max_ast_depth");
    let nots = format!(
        "SELECT title FROM orders WHERE {}amount = 1{}",
        "NOT (".repeat(40),
        ")".repeat(40)
    );
    assert_eq!(limit(&nots, &l), "max_ast_depth");
    // MAX_AST_NODES（リテラルを使わない横に広い式）
    let wide = format!(
        "SELECT title FROM orders WHERE {}",
        vec!["title IS NULL"; 1_500].join(" OR ")
    );
    assert_eq!(limit(&wide, &l), "max_ast_nodes");
    // MAX_LITERALS（IN の要素）
    let many: Vec<String> = (0..1_100).map(|i| i.to_string()).collect();
    let lits = format!(
        "SELECT title FROM orders WHERE amount IN ({})",
        many.join(", ")
    );
    assert_eq!(limit(&lits, &l), "max_literals");
    // MAX_COMPILE_MS（予算 0 では必ず超える）
    let tight = QueryLimits {
        max_compile: std::time::Duration::ZERO,
        ..QueryLimits::default()
    };
    let mut b = CompileBudget::new(&tight);
    let e = b
        .stage(&tight, || parse("SELECT title FROM orders", &tight))
        .unwrap_err();
    assert_eq!(e.limit.unwrap().name, "max_compile_ms");
}

use crate::query::limits::CompileBudget;

#[test]
fn nesting_that_makes_the_parser_super_linear_is_rejected_before_parsing() {
    use std::time::Instant;
    let l = QueryLimits::default();
    // 関数の入れ子は部分集合の外（パーサの投機的解析が指数時間になる形でもある）。
    for d in [2usize, 16, 30] {
        let sql = format!(
            "SELECT title FROM orders WHERE {}amount{} = 1",
            "lower(".repeat(d),
            ")".repeat(d)
        );
        let t = Instant::now();
        let e = parse(&sql, &l).unwrap_err();
        assert_eq!(e.code, RejectCode::Unsupported, "{d}");
        assert!(t.elapsed().as_millis() < 50, "{d}: {:?}", t.elapsed());
    }
    // 上限付近の入れ子（NOT ( の連鎖）も線形時間で受理・拒否する。
    for d in [20usize, 30, 31, 40, 200] {
        let sql = format!(
            "SELECT title FROM orders WHERE {}amount = 1{}",
            "NOT (".repeat(d),
            ")".repeat(d)
        );
        let t = Instant::now();
        let r = parse(&sql, &l);
        assert!(t.elapsed().as_millis() < 50, "{d}: {:?}", t.elapsed());
        if d > 32 {
            assert_eq!(r.unwrap_err().limit.unwrap().name, "max_ast_depth");
        }
    }
}
