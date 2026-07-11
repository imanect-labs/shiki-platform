//! CSV のパッチ操作（Task 11P.7・design §4.8.2）。
//!
//! セル/行/列のパッチ列を、ヘッダ付き CSV にストリームで適用して新しい CSV バイト列を作る。
//! CRDT 共同編集はしない（巨大ファイルで update log が破綻するため）。並行編集の衝突は
//! **rev 楽観ロック**（呼び出し側が `node.version` と base_rev を突合）で検出する。
//!
//! 行インデックスは**ヘッダを除いたデータ行の 0 始まり**。列インデックスは 0 始まり。

use serde::{Deserialize, Serialize};

use crate::error::TabularError;

/// 1 つのパッチ操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    /// セル更新（行・列を指定して値を置換）。
    CellUpdate {
        row: usize,
        col: usize,
        value: String,
    },
    /// 行挿入（`at` の位置に、列順の値配列で挿入。末尾は len を指定）。
    RowInsert { at: usize, values: Vec<String> },
    /// 行削除。
    RowDelete { row: usize },
    /// 列追加（末尾に `name` 列を足し、既存行は空文字で埋める）。
    ColumnAdd { name: String },
    /// 列削除。
    ColumnDelete { col: usize },
    /// 列名変更。
    ColumnRename { col: usize, name: String },
}

/// パッチ適用結果（新 CSV バイト列と、適用後の行数/列数）。
#[derive(Debug)]
pub struct PatchResult {
    pub csv: Vec<u8>,
    pub rows: usize,
    pub cols: usize,
}

/// ヘッダ付き CSV バイト列にパッチ列を適用し、新しい CSV を返す。
pub fn apply_patches(csv_bytes: &[u8], ops: &[PatchOp]) -> Result<PatchResult, TabularError> {
    let (mut header, mut rows) = parse_csv(csv_bytes)?;
    for op in ops {
        apply_one(&mut header, &mut rows, op)?;
    }
    let csv = write_csv(&header, &rows)?;
    Ok(PatchResult {
        csv,
        rows: rows.len(),
        cols: header.len(),
    })
}

fn apply_one(
    header: &mut Vec<String>,
    rows: &mut Vec<Vec<String>>,
    op: &PatchOp,
) -> Result<(), TabularError> {
    match op {
        PatchOp::CellUpdate { row, col, value } => {
            let r = rows
                .get_mut(*row)
                .ok_or_else(|| TabularError::InvalidPatch(format!("行 {row} は範囲外")))?;
            let cell = r
                .get_mut(*col)
                .ok_or_else(|| TabularError::InvalidPatch(format!("列 {col} は範囲外")))?;
            cell.clone_from(value);
        }
        PatchOp::RowInsert { at, values } => {
            if *at > rows.len() {
                return Err(TabularError::InvalidPatch(format!(
                    "挿入位置 {at} は範囲外"
                )));
            }
            if values.len() != header.len() {
                return Err(TabularError::InvalidPatch(format!(
                    "列数不一致（期待 {}, 実際 {}）",
                    header.len(),
                    values.len()
                )));
            }
            rows.insert(*at, values.clone());
        }
        PatchOp::RowDelete { row } => {
            if *row >= rows.len() {
                return Err(TabularError::InvalidPatch(format!("行 {row} は範囲外")));
            }
            rows.remove(*row);
        }
        PatchOp::ColumnAdd { name } => {
            header.push(name.clone());
            for r in rows.iter_mut() {
                r.push(String::new());
            }
        }
        PatchOp::ColumnDelete { col } => {
            if *col >= header.len() {
                return Err(TabularError::InvalidPatch(format!("列 {col} は範囲外")));
            }
            header.remove(*col);
            for r in rows.iter_mut() {
                if *col < r.len() {
                    r.remove(*col);
                }
            }
        }
        PatchOp::ColumnRename { col, name } => {
            let h = header
                .get_mut(*col)
                .ok_or_else(|| TabularError::InvalidPatch(format!("列 {col} は範囲外")))?;
            h.clone_from(name);
        }
    }
    Ok(())
}

/// (ヘッダ, データ行) のタプル。
type Parsed = (Vec<String>, Vec<Vec<String>>);

/// ヘッダ＋データ行に分解する（ヘッダ必須）。
fn parse_csv(bytes: &[u8]) -> Result<Parsed, TabularError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(bytes);
    let header: Vec<String> = reader
        .headers()
        .map_err(|e| TabularError::InvalidPatch(format!("ヘッダ解析に失敗: {e}")))?
        .iter()
        .map(str::to_string)
        .collect();
    let ncols = header.len();
    let mut rows = Vec::new();
    for rec in reader.records() {
        let rec = rec.map_err(|e| TabularError::InvalidPatch(format!("行解析に失敗: {e}")))?;
        let mut row: Vec<String> = rec.iter().map(str::to_string).collect();
        // 列数を header に合わせて正規化（不足は空・過剰は切詰め）。
        row.resize(ncols, String::new());
        rows.push(row);
    }
    Ok((header, rows))
}

fn write_csv(header: &[String], rows: &[Vec<String>]) -> Result<Vec<u8>, TabularError> {
    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    writer
        .write_record(header)
        .map_err(|e| TabularError::Internal(format!("CSV 書込に失敗: {e}")))?;
    for row in rows {
        writer
            .write_record(row)
            .map_err(|e| TabularError::Internal(format!("CSV 書込に失敗: {e}")))?;
    }
    writer
        .into_inner()
        .map_err(|e| TabularError::Internal(format!("CSV flush に失敗: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv_str(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn cell_update() {
        let r = apply_patches(
            b"a,b\n1,2\n3,4\n",
            &[PatchOp::CellUpdate {
                row: 1,
                col: 0,
                value: "99".into(),
            }],
        )
        .unwrap();
        assert_eq!(csv_str(&r.csv), "a,b\n1,2\n99,4\n");
    }

    #[test]
    fn row_insert_and_delete() {
        let r = apply_patches(
            b"a,b\n1,2\n",
            &[
                PatchOp::RowInsert {
                    at: 1,
                    values: vec!["5".into(), "6".into()],
                },
                PatchOp::RowDelete { row: 0 },
            ],
        )
        .unwrap();
        assert_eq!(csv_str(&r.csv), "a,b\n5,6\n");
        assert_eq!(r.rows, 1);
    }

    #[test]
    fn column_add_delete_rename() {
        let r = apply_patches(
            b"a,b\n1,2\n",
            &[
                PatchOp::ColumnAdd { name: "c".into() },
                PatchOp::ColumnRename {
                    col: 0,
                    name: "id".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(csv_str(&r.csv), "id,b,c\n1,2,\n");
        let r2 = apply_patches(&r.csv, &[PatchOp::ColumnDelete { col: 1 }]).unwrap();
        assert_eq!(csv_str(&r2.csv), "id,c\n1,\n");
    }

    #[test]
    fn out_of_range_rejected() {
        assert!(apply_patches(
            b"a\n1\n",
            &[PatchOp::CellUpdate {
                row: 9,
                col: 0,
                value: "x".into()
            }]
        )
        .is_err());
        assert!(apply_patches(
            b"a,b\n1,2\n",
            &[PatchOp::RowInsert {
                at: 0,
                values: vec!["only-one".into()]
            }]
        )
        .is_err());
    }

    #[test]
    fn quoting_roundtrip() {
        // カンマ・改行・引用符を含む値が壊れない。
        let r = apply_patches(
            b"a,b\n\"x,y\",\"line1\nline2\"\n",
            &[PatchOp::CellUpdate {
                row: 0,
                col: 0,
                value: "has \"quote\"".into(),
            }],
        )
        .unwrap();
        // 再パースして値が保たれることを確認。
        let (_h, rows) = parse_csv(&r.csv).unwrap();
        assert_eq!(rows[0][0], "has \"quote\"");
        assert_eq!(rows[0][1], "line1\nline2");
    }
}
