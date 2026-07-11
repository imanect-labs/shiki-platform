//! SQL の読み取り専用検証（Task 11P.7・PIT-39）。
//!
//! **多層防御**:
//! 1. sqlparser で構文解析し、**ちょうど 1 文かつ `Statement::Query`（SELECT/WITH/VALUES）**
//!    のみを許可する。DDL/DML・ATTACH・PRAGMA・COPY・INSTALL・LOAD・SET・CALL 等は
//!    それぞれ別の `Statement` 変種になるため、この 1 条件で全て弾ける（fail-closed）。
//! 2. 生 SQL に対し、ファイルシステム/ネットワークに到達し得る**関数呼び出しの denylist**
//!    （`read_csv(` 等）を拒否する。実行系の最終防壁はランナーの `enable_external_access=false`
//!    だが、そこへ届く前に拒否して多層化する。
//!
//! ランナー側テーブル名は常に `data`。ユーザーは `SELECT ... FROM data ...` を書く。

use sqlparser::ast::Statement;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::error::TabularError;

/// FS/ネットワークに到達し得る DuckDB 関数など（小文字・`(` を付けて呼び出し形のみ照合）。
/// 列名 `load` 等の誤検知を避けるため、必ず開き括弧を伴う呼び出しパターンで判定する。
const DANGEROUS_FNS: &[&str] = &[
    "read_csv",
    "read_csv_auto",
    "read_parquet",
    "parquet_scan",
    "read_json",
    "read_json_auto",
    "read_ndjson",
    "read_ndjson_auto",
    "read_text",
    "read_blob",
    "sniff_csv",
    "glob",
    "csv_scan",
    "parquet_metadata",
    "parquet_schema",
    "httpfs",
    "read_parquet_mr",
];

/// 検証済み読み取り専用 SQL であることを確認する（違反は [`TabularError::SqlRejected`]）。
pub fn validate_read_only(sql: &str) -> Result<(), TabularError> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return Err(TabularError::SqlRejected("空の SQL です".into()));
    }
    // 1) 構文レベル: ちょうど 1 文の Query（SELECT/WITH/VALUES）のみ。
    let statements = Parser::parse_sql(&GenericDialect {}, trimmed)
        .map_err(|e| TabularError::SqlRejected(format!("解析に失敗: {e}")))?;
    if statements.len() != 1 {
        return Err(TabularError::SqlRejected(
            "複数文は実行できません（単一の SELECT のみ）".into(),
        ));
    }
    if !matches!(statements[0], Statement::Query(_)) {
        return Err(TabularError::SqlRejected(
            "SELECT（読み取り）のみ実行できます".into(),
        ));
    }
    // 2) 生 SQL レベル: 危険関数の呼び出しを拒否（外部アクセス・実行系の多層防御）。
    let lower = trimmed.to_lowercase();
    for func in DANGEROUS_FNS {
        if contains_call(&lower, func) {
            return Err(TabularError::SqlRejected(format!(
                "関数 `{func}` は使用できません（外部アクセス禁止）"
            )));
        }
    }
    Ok(())
}

/// `func(`（間に空白可）の呼び出しパターンが含まれるか（識別子境界を考慮）。
fn contains_call(lower_sql: &str, func: &str) -> bool {
    let bytes = lower_sql.as_bytes();
    let mut from = 0;
    while let Some(pos) = lower_sql[from..].find(func) {
        let start = from + pos;
        let end = start + func.len();
        // 前が識別子文字なら別の語（例: my_read_csv）→ スキップ。
        let prev_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        // 後続（空白を飛ばして）が `(` なら関数呼び出し。
        let mut i = end;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n') {
            i += 1;
        }
        let next_is_paren = i < bytes.len() && bytes[i] == b'(';
        if prev_ok && next_is_paren {
            return true;
        }
        from = end;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_plain_select() {
        assert!(validate_read_only("SELECT * FROM data").is_ok());
        assert!(validate_read_only("select a, count(*) from data group by a").is_ok());
        assert!(validate_read_only("WITH t AS (SELECT * FROM data) SELECT * FROM t").is_ok());
    }

    #[test]
    fn rejects_dml_ddl() {
        for sql in [
            "INSERT INTO data VALUES (1)",
            "UPDATE data SET a = 1",
            "DELETE FROM data",
            "DROP TABLE data",
            "CREATE TABLE x (a int)",
            "ALTER TABLE data ADD COLUMN b int",
        ] {
            assert!(validate_read_only(sql).is_err(), "should reject: {sql}");
        }
    }

    #[test]
    fn rejects_duckdb_control_statements() {
        for sql in [
            "ATTACH 'x.db'",
            "PRAGMA database_list",
            "INSTALL httpfs",
            "LOAD httpfs",
            "SET enable_external_access=true",
            "COPY data TO '/tmp/x.csv'",
            "CALL pragma_version()",
        ] {
            assert!(validate_read_only(sql).is_err(), "should reject: {sql}");
        }
    }

    #[test]
    fn rejects_multiple_statements() {
        assert!(validate_read_only("SELECT 1; SELECT 2").is_err());
        assert!(validate_read_only("SELECT * FROM data; DROP TABLE data").is_err());
    }

    #[test]
    fn rejects_external_access_functions() {
        for sql in [
            "SELECT * FROM read_csv('/etc/passwd')",
            "SELECT * FROM read_parquet('s3://x/y')",
            "SELECT read_text('/etc/hosts')",
            "SELECT * FROM glob('/*')",
            "SELECT * FROM read_json_auto('http://evil/x')",
        ] {
            assert!(validate_read_only(sql).is_err(), "should reject: {sql}");
        }
    }

    #[test]
    fn does_not_falsely_reject_similar_identifiers() {
        // 列/別名に紛らわしい名前があっても、呼び出し形でなければ通す。
        assert!(validate_read_only("SELECT glob_count FROM data").is_ok());
        assert!(validate_read_only("SELECT my_read_csv FROM data").is_ok());
    }
}
