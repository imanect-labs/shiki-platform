//! 保存先に同じ名前のファイルがあるときの扱いと、番号を付けた名前の規則（#381・BUG-97・D-112）。
//!
//! 番号を付けるのはチョークポイント（[`StorageService`](crate::StorageService)）の書込の入口だけ。ドライブの
//! 新規作成（ノート・スライド・Office・CSV）とワークフローの「ファイルを保存」が、同じ規則で `メモ (2).md` 〜
//! `メモ (50).md` を試す。呼び出し側で名前を変えて呼び直さない（規則が分かれる）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 保存先の同じフォルダーに同じ名前のファイルがあるときの扱い。
///
/// 値の名前（`fail`・`add_number`）はワークフローの `on_name_taken` と同じ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnNameTaken {
    /// 保存せずに [`StorageError::Conflict`](crate::StorageError::Conflict) を返す（既存のファイルは変えない）。
    #[default]
    Fail,
    /// 拡張子の前に ` (2)` ` (3)` … を付けて、空いている名前で保存する（上限 [`MAX_NUMBERED_NAME`]）。
    /// 上限まで埋まっていれば [`StorageError::Conflict`](crate::StorageError::Conflict)（黙って上書きしない）。
    AddNumber,
}

impl OnNameTaken {
    /// 試す名前の数（1 回目は元の名前）。
    #[must_use]
    pub const fn tries(self) -> u32 {
        match self {
            OnNameTaken::Fail => 1,
            OnNameTaken::AddNumber => MAX_NUMBERED_NAME,
        }
    }
}

/// 番号を付けて試す上限（` (2)` 〜 ` (50)`）。
pub const MAX_NUMBERED_NAME: u32 = 50;

/// `attempt` 回目に試す名前（1 回目は元の名前・2 回目からは最後の `.` の前に ` (n)`）。
///
/// `.` の無い名前は末尾に付ける（`メモ (2)`）。末尾が `.` の名前も最後の `.` の前に付ける（`a (2).`）。
#[must_use]
pub fn numbered_name(file_name: &str, attempt: u32) -> String {
    if attempt <= 1 {
        return file_name.to_string();
    }
    match file_name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem} ({attempt}).{ext}"),
        None => format!("{file_name} ({attempt})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 回目は元の名前、2 回目からは最後の `.` の前に番号を付ける（ドライブの新規作成とワークフローで同じ）。
    #[test]
    fn numbers_before_the_last_dot() {
        for (name, attempt, want) in [
            ("週報.md", 1, "週報.md"),
            ("週報.md", 2, "週報 (2).md"),
            ("週報.md", 50, "週報 (50).md"),
            ("無題のノート", 3, "無題のノート (3)"),
            ("a.tar.gz", 2, "a.tar (2).gz"),
            (".env", 2, " (2).env"),
            ("a.", 2, "a (2)."),
            ("x", 0, "x"),
        ] {
            assert_eq!(
                numbered_name(name, attempt),
                want,
                "{name} の {attempt} 回目"
            );
        }
    }

    /// 番号を付けない扱いは 1 回だけ、付ける扱いは (50) まで試す。既定は失敗にする。
    #[test]
    fn tries_by_policy() {
        assert_eq!(OnNameTaken::Fail.tries(), 1);
        assert_eq!(OnNameTaken::AddNumber.tries(), MAX_NUMBERED_NAME);
        assert_eq!(OnNameTaken::default(), OnNameTaken::Fail);
        assert_eq!(
            serde_json::to_value(OnNameTaken::AddNumber).unwrap(),
            serde_json::json!("add_number")
        );
    }
}
