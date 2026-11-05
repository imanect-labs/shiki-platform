//! 複数 CSV クエリの追加テーブル指定（#1037・#247 の「複数 CSV へのクエリ層」）。
//!
//! 主ファイルは常に [`PRIMARY_TABLE`]（`data`）。追加ファイルは利用者が付けた名前のテーブルとして
//! 同じ隔離ランナーにロードされ、`SELECT ... FROM data JOIN other ON ...` で結合できる。
//!
//! # 不変条件
//! - **全ファイルを個別に viewer 認可**する（[`crate::TabularService::query_tables`]）。1 つでも
//!   読めなければ全体を fail-closed（部分実行しない）。
//! - テーブル名は**識別子として安全な形だけ**許可する（`^[a-z_][a-z0-9_]{0,62}$`）。名前は
//!   ランナーで `CREATE TABLE "<name>"` に埋め込まれるため、api 側とランナー側の両方で同じ
//!   [`is_valid_table_name`] を通す（多層防御）。
//! - 追加テーブル数は [`MAX_EXTRA_TABLES`] まで（1 プロセスに載せるファイル数の上限）。

use std::collections::HashSet;

use uuid::Uuid;

use crate::error::TabularError;

/// 主ファイルのテーブル名（既存の単一 CSV クエリと同じ）。
pub const PRIMARY_TABLE: &str = "data";

/// 1 クエリで追加できるテーブル数の上限（主ファイルを含まない）。
pub const MAX_EXTRA_TABLES: usize = 8;

/// テーブル名の最大長（DuckDB の識別子としても十分短い）。
const MAX_NAME_LEN: usize = 63;

/// [`is_valid_table_name`] と同じ規則の正規表現（JSON Schema 等へ出す用・単一定義）。
pub const TABLE_NAME_PATTERN: &str = "^[a-z_][a-z0-9_]{0,62}$";

/// 追加テーブル（利用者が付けた名前 → CSV ファイル）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    /// SQL から参照するテーブル名（[`is_valid_table_name`] を満たすこと）。
    pub name: String,
    /// 対象 CSV のファイル id（viewer 認可で取得する）。
    pub file_id: Uuid,
}

/// テーブル名として安全か（英小文字/数字/`_`・先頭は数字以外・63 文字以内）。
///
/// 引用符・空白・ドット・非 ASCII を一切許さないため、`CREATE TABLE "<name>"` へ埋め込んでも
/// 識別子の外へ出られない。ランナーもこの関数で再検証する。
pub fn is_valid_table_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_LEN {
        return false;
    }
    let head_ok = bytes[0].is_ascii_lowercase() || bytes[0] == b'_';
    head_ok
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// 追加テーブル指定を検証する（名前の形・`data` との衝突・重複・上限）。
pub fn validate_tables(tables: &[TableRef]) -> Result<(), TabularError> {
    if tables.len() > MAX_EXTRA_TABLES {
        return Err(TabularError::InvalidTables(format!(
            "追加できるテーブルは {MAX_EXTRA_TABLES} 個までです（{} 個指定）",
            tables.len()
        )));
    }
    let mut seen = HashSet::with_capacity(tables.len());
    for t in tables {
        if !is_valid_table_name(&t.name) {
            return Err(TabularError::InvalidTables(format!(
                "テーブル名 `{}` は使えません（英小文字・数字・_ のみ、先頭は数字以外、63 文字以内）",
                t.name
            )));
        }
        if t.name == PRIMARY_TABLE {
            return Err(TabularError::InvalidTables(format!(
                "テーブル名 `{PRIMARY_TABLE}` は主ファイルに予約されています"
            )));
        }
        if !seen.insert(t.name.as_str()) {
            return Err(TabularError::InvalidTables(format!(
                "テーブル名 `{}` が重複しています",
                t.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> TableRef {
        TableRef {
            name: name.into(),
            file_id: Uuid::nil(),
        }
    }

    /// 正規表現の定数と判定関数が同じ規則であること（片方だけ変えて食い違わせない）。
    #[test]
    fn pattern_matches_validator_rules() {
        // 長さの上限は MAX_NAME_LEN と同じ（先頭 1 文字＋残り MAX_NAME_LEN-1 文字）。
        assert!(TABLE_NAME_PATTERN.contains(&format!("{{0,{}}}", MAX_NAME_LEN - 1)));
        assert!(TABLE_NAME_PATTERN.starts_with("^[a-z_][a-z0-9_]"));
    }

    #[test]
    fn accepts_simple_identifiers() {
        for n in ["orders", "_tmp", "t1", "customer_master", &"a".repeat(63)] {
            assert!(is_valid_table_name(n), "should accept: {n}");
        }
    }

    #[test]
    fn rejects_unsafe_identifiers() {
        for n in [
            "",
            "1abc",
            "Orders",
            "my table",
            "a\"b",
            "a;drop",
            "main.data",
            "顧客",
            "a-b",
            &"a".repeat(64),
        ] {
            assert!(!is_valid_table_name(n), "should reject: {n:?}");
        }
    }

    #[test]
    fn validate_tables_ok_for_distinct_names() {
        assert!(validate_tables(&[]).is_ok());
        assert!(validate_tables(&[t("orders"), t("customers")]).is_ok());
    }

    #[test]
    fn validate_tables_rejects_reserved_duplicate_and_bad_names() {
        assert!(matches!(
            validate_tables(&[t("data")]),
            Err(TabularError::InvalidTables(_))
        ));
        assert!(matches!(
            validate_tables(&[t("a"), t("a")]),
            Err(TabularError::InvalidTables(_))
        ));
        assert!(matches!(
            validate_tables(&[t("A")]),
            Err(TabularError::InvalidTables(_))
        ));
    }

    #[test]
    fn validate_tables_enforces_limit() {
        let ok: Vec<TableRef> = (0..MAX_EXTRA_TABLES).map(|i| t(&format!("t{i}"))).collect();
        assert!(validate_tables(&ok).is_ok());
        let over: Vec<TableRef> = (0..=MAX_EXTRA_TABLES)
            .map(|i| t(&format!("t{i}")))
            .collect();
        assert!(matches!(
            validate_tables(&over),
            Err(TabularError::InvalidTables(_))
        ));
    }
}
