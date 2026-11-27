//! SQL の読み取り専用検証（Task 11P.7・PIT-39）。
//!
//! 決まりはランナーと共有するクレートの [`tabular_protocol::sql_guard`] の 1 か所にあり、ここはその違反を
//! [`TabularError::SqlRejected`] に写すだけ（GAP-95）。SQL が表として読む名前（[`table_names`]）も同じクレートの
//! ものを再エクスポートする。

pub use tabular_protocol::sql_guard::table_names;

use crate::error::TabularError;

/// 検証済み読み取り専用 SQL であることを確認する（違反は [`TabularError::SqlRejected`]）。
pub fn validate_read_only(sql: &str) -> Result<(), TabularError> {
    tabular_protocol::sql_guard::validate_read_only(sql)
        .map_err(|rejected| TabularError::SqlRejected(rejected.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全角空白入りの列名（警察庁 CSV の「発生日時　　年」）は、引用符なしだと解析に失敗し、二重引用符で
    /// 囲めば通る（csv.query の案内文と一致させる）。括弧入り（「曜日(発生年月日)」）は引用符なしでも
    /// 関数呼び出しとして解析は通り、DuckDB 側で「関数が無い」になる（どちらも引用符で直る）。
    #[test]
    fn quoted_japanese_column_names_pass_and_unquoted_fail_to_parse() {
        let unquoted =
            validate_read_only("SELECT 発生日時　　年 AS 年, COUNT(*) FROM data GROUP BY 1");
        assert!(
            matches!(&unquoted, Err(TabularError::SqlRejected(m)) if m.starts_with("解析に失敗")),
            "{unquoted:?}"
        );
        assert!(validate_read_only(
            "SELECT \"発生日時　　年\" AS 年, \"曜日(発生年月日)\", SUM(CAST(\"死者数\" AS INTEGER)) FROM data GROUP BY 1, 2"
        )
        .is_ok());
    }

    #[test]
    fn rejections_keep_the_reason_as_sql_rejected() {
        assert!(validate_read_only("SELECT * FROM data").is_ok());
        let dropped = validate_read_only("DROP TABLE data");
        assert!(
            matches!(&dropped, Err(TabularError::SqlRejected(r)) if r == "SELECT（読み取り）のみ実行できます"),
            "{dropped:?}"
        );
        let outside = validate_read_only("SELECT * FROM read_csv('/etc/passwd')");
        assert!(
            matches!(&outside, Err(TabularError::SqlRejected(r)) if r.contains("read_csv")),
            "{outside:?}"
        );
    }
}
