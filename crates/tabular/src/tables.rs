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

use uuid::Uuid;

use crate::error::TabularError;

// 名前の決まり（形・上限・主ファイルの名前）はランナーと共有するクレートにある（ランナーも同じ関数で再検証する・GAP-95）。
pub use tabular_protocol::tables::{
    is_valid_table_name, table_name_issue, too_many_tables, TableNameIssue, MAX_EXTRA_TABLES,
    PRIMARY_TABLE, TABLE_NAME_PATTERN,
};

/// 追加テーブル（利用者が付けた名前 → CSV ファイル）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    /// SQL から参照するテーブル名（[`is_valid_table_name`] を満たすこと）。
    pub name: String,
    /// 対象 CSV のファイル id（viewer 認可で取得する）。
    pub file_id: Uuid,
}

/// 追加テーブル指定を検証する（名前の形・`data` との衝突・重複・上限）。
pub fn validate_tables(tables: &[TableRef]) -> Result<(), TabularError> {
    if too_many_tables(tables.len()) {
        return Err(TabularError::InvalidTables(format!(
            "追加できるテーブルは {MAX_EXTRA_TABLES} 個までです（{} 個指定）",
            tables.len()
        )));
    }
    for (i, t) in tables.iter().enumerate() {
        let earlier = tables[..i].iter().map(|e| e.name.as_str());
        let message = match table_name_issue(&t.name, earlier) {
            None => continue,
            Some(TableNameIssue::Shape) => format!(
                "テーブル名 `{}` は使えません（英小文字・数字・_ のみ、先頭は数字以外、63 文字以内）",
                t.name
            ),
            Some(TableNameIssue::Reserved) => {
                format!("テーブル名 `{PRIMARY_TABLE}` は主ファイルに予約されています")
            }
            Some(TableNameIssue::Duplicate) => {
                format!("テーブル名 `{}` が重複しています", t.name)
            }
        };
        return Err(TabularError::InvalidTables(message));
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
