//! 複数 CSV クエリの追加テーブルの名前の決まり（#1037）。
//!
//! 名前はランナーで `CREATE TABLE "<name>"` に埋め込まれるため、api 側（`shiki-tabular` の `validate_tables`）と
//! ランナー側の両方で同じ [`is_valid_table_name`] を通す（多層防御）。ランナーはこのクレートだけに依存するので、
//! 決まりの実体はここに置き、`shiki-tabular` は再エクスポートする（GAP-95）。

/// 主ファイルのテーブル名（既存の単一 CSV クエリと同じ）。
pub const PRIMARY_TABLE: &str = "data";

/// 1 クエリで追加できるテーブル数の上限（主ファイルを含まない）。
pub const MAX_EXTRA_TABLES: usize = 8;

/// テーブル名の最大長（DuckDB の識別子としても十分短い）。
const MAX_NAME_LEN: usize = 63;

/// [`is_valid_table_name`] と同じ規則の正規表現（JSON Schema 等へ出す用・単一定義）。
pub const TABLE_NAME_PATTERN: &str = "^[a-z_][a-z0-9_]{0,62}$";

/// テーブル名として安全か（英小文字/数字/`_`・先頭は数字以外・63 文字以内）。
///
/// 引用符・空白・ドット・非 ASCII を一切許さないため、`CREATE TABLE "<name>"` へ埋め込んでも
/// 識別子の外へ出られない。ランナーもこの関数で再検証する。
#[must_use]
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

/// 追加テーブルの数が上限（[`MAX_EXTRA_TABLES`]）を超えるか。
#[must_use]
pub fn too_many_tables(count: usize) -> bool {
    count > MAX_EXTRA_TABLES
}

/// 追加テーブルの名前に使えない理由（[`table_name_issue`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableNameIssue {
    /// 形が決まりに合わない（[`is_valid_table_name`]）。
    Shape,
    /// 主ファイルの名前（[`PRIMARY_TABLE`]）。
    Reserved,
    /// 同じ指定の前に並ぶ名前と同じ。
    Duplicate,
}

/// 追加テーブルの名前 `name` を、同じ指定で前に並ぶ名前 `earlier` の後に使えるか（使えなければ理由）。
///
/// 確かめる順は形 → 主ファイルの名前 → 重なり。api 側の `shiki-tabular` の `validate_tables` と、ワークフローの
/// 保存時の照合（`csv.query` の `tables`）が同じこの関数で決める（決まりを写さない・GAP-126）。
pub fn table_name_issue<'a>(
    name: &str,
    earlier: impl IntoIterator<Item = &'a str>,
) -> Option<TableNameIssue> {
    if !is_valid_table_name(name) {
        return Some(TableNameIssue::Shape);
    }
    if name == PRIMARY_TABLE {
        return Some(TableNameIssue::Reserved);
    }
    earlier
        .into_iter()
        .any(|e| e == name)
        .then_some(TableNameIssue::Duplicate)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 名前の理由は形 → 主ファイルの名前 → 重なりの順。数の上限は主ファイルを含まない。
    #[test]
    fn table_name_issues_are_checked_in_order() {
        assert_eq!(table_name_issue("orders", []), None);
        assert_eq!(table_name_issue("Orders", []), Some(TableNameIssue::Shape));
        assert_eq!(
            table_name_issue(PRIMARY_TABLE, []),
            Some(TableNameIssue::Reserved)
        );
        assert_eq!(
            table_name_issue("data", ["data"]),
            Some(TableNameIssue::Reserved)
        );
        assert_eq!(
            table_name_issue("orders", ["items", "orders"]),
            Some(TableNameIssue::Duplicate)
        );
        assert!(!too_many_tables(MAX_EXTRA_TABLES));
        assert!(too_many_tables(MAX_EXTRA_TABLES + 1));
    }
}
