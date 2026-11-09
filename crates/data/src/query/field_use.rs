//! 列をクエリのどこに書けるか（束縛が列を断る規則の単一の定義・BUG-244）。
//!
//! 束縛（[`super::bound`]・[`super::bound_query`]）は、ここの関数で列を断る。ワークフローの保存時の照合
//! （`data.query` のテーブルがリテラルのとき）も、同じ [`check_field_use`] で列を確かめる（規則を写さない）。
//!
//! - 絞り込み・並べ替え・まとめる列・集計の列は、索引のある列（`indexed`）か重複を許さない列（`unique`）、
//!   作成者の列、材料化した派生列、システムの擬似列（`_owner`・`_created_at`・`_updated_at`）だけ。
//!   対称列（逆向きのリンク）は使えない（[`super::bound`] の `resolve`）。
//! - 部分一致（`contains`・`ends_with`）は検索の索引（`searchable`）のある文字の列だけ。
//! - 大小比較・並べ替え・まとめる列に、多値の列とリンクの列は使えない。まとめる列にシステムの擬似列は使えない。
//!   集計の列は数（派生列は数の結果のもの）だけ。派生列は複数のキーの並べ替えに使えない。
//! - リンクの条件（`filter` の `link`）と結合（`joins`）で辿る列は、リンクの列（`record_ref`・対称列も可）だけ
//!   （[`check_link_field`]）。その先の列（リンクの条件の中・結合の条件・`別名.参照名` のまとめる列と集計の列）は、
//!   参照先のテーブルの定義で [`check_field_use`] に問う（束縛も参照先の定義で同じ規則を当てる・GAP-129）。
//!
//! 値（比較の右辺）に依る誤り（型の不一致・長さ）は、ここでは扱わない（値は実行の直前まで決まらないことがある）。

use std::collections::HashSet;

use uuid::Uuid;

use super::bound::{BField, Binder, FieldKind};
use super::ir::CondOp;
use crate::model::TableSchema;
use crate::{DataError, InvalidCode};

/// クエリの中で列を書く位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldUse {
    /// 絞り込みの比較（`filter` の `cmp`）。演算子で要る索引が変わる（部分一致は検索の索引）。
    Filter(CondOp),
    /// 並べ替え（`sort`）。`keys` は並べ替えのキーの数（派生列は 1 つのときだけ使える）。
    Sort { keys: usize },
    /// まとめる列（`group_by`）。
    GroupBy,
    /// 集計の列（`aggregate`・`having`・`group_sort` の指標の `field`・`count` 以外）。
    Metric,
}

/// 列 `key` を `at` に書いたクエリを束縛が列のために断るなら、束縛と同じ誤りを返す。
///
/// `schema` は読む人の見え方の定義（読めない列を外したもの・[`crate::DataStore::get_table_for_viewer`]）。読めない列と
/// 無い列は同じ [`DataError::FieldUnavailable`] になる（区別させない・data-platform §16.1）。`key` は `schema` の表の
/// 列の参照名。結合先の列（`別名.参照名`）とリンクの条件の中の列は、参照先のテーブルの定義と別名を外した参照名で問う
/// （束縛も結合先・参照先の束縛器で同じ関数を当てる）。
pub fn check_field_use(schema: &TableSchema, key: &str, at: FieldUse) -> Result<(), DataError> {
    let masked = HashSet::new();
    let mut b = Binder::new(schema, &masked);
    match at {
        FieldUse::Filter(op) => op_fits(&b.cmp_field(key, op)?, op),
        FieldUse::Sort { keys } => {
            let f = sort_field(&mut b, key)?;
            sort_derived_fits(&f, key, keys)
        }
        FieldUse::GroupBy => group_key_fits(&b.queryable(key)?, key),
        FieldUse::Metric => metric_fits(&b.queryable(key)?, key),
    }
}

/// リンクの条件（`filter` の `link`）と結合（`joins` の `field`）で辿る列 `key` の参照先のテーブル。束縛
/// （`Binder::link_field`）と同じ関数で断る: 無い列と読めない列は [`DataError::FieldUnavailable`]、リンクの列
/// （`record_ref`・対称列も可）でなければ 400 `invalid_input`。
///
/// `schema` は読む人の見え方の定義。参照先を読む人が開けるか（開けなければ実行では 403）は、ここでは問わない。
pub fn check_link_field(schema: &TableSchema, key: &str) -> Result<Uuid, DataError> {
    let masked = HashSet::new();
    Binder::new(schema, &masked)
        .link_field(key)
        .map(|(_, rt)| rt)
}

/// 比較の演算子を列に使えるか（値に依らない誤り）。大小比較は単値の列だけ、前方一致・部分一致は文字の列だけ、
/// 部分一致（`contains`・`ends_with`）は検索の索引のある列だけ。
pub(crate) fn op_fits(f: &BField, op: CondOp) -> Result<(), DataError> {
    let key = &f.key;
    match op {
        CondOp::Lt | CondOp::Lte | CondOp::Gt | CondOp::Gte if f.multi() => {
            Err(DataError::input_detail(
                InvalidCode::FieldNotSortable,
                Some(key),
                format!("'{key}'（多値・リンク）に大小比較は使えません"),
            ))
        }
        CondOp::StartsWith | CondOp::Contains | CondOp::EndsWith
            if !matches!(f.kind, FieldKind::Text | FieldKind::Multi) =>
        {
            Err(DataError::input_detail(
                InvalidCode::ValueTypeMismatch,
                Some(key),
                format!("'{key}' は文字列の部分一致に使えません"),
            ))
        }
        CondOp::Contains | CondOp::EndsWith if !f.searchable => Err(DataError::input_detail(
            InvalidCode::FieldNotIndexed,
            Some(key),
            format!("'{key}' は部分一致検索の対象ではありません"),
        )),
        _ => Ok(()),
    }
}

/// 並べ替えのキーの列を引く（結合先の列・多値の列・リンクの列は使えない）。
pub(crate) fn sort_field(b: &mut Binder<'_>, key: &str) -> Result<BField, DataError> {
    if key.contains('.') {
        return Err(DataError::input_detail(InvalidCode::InvalidInput, None, format!(
            "ソートキー '{key}': 結合先の列では並べ替えられません（FROM の表の列を指定してください）"
        )));
    }
    let f = b.queryable(key)?;
    if f.multi() {
        return Err(DataError::input_detail(
            InvalidCode::FieldNotSortable,
            Some(key),
            format!("フィールド '{key}' はソートに使えません（多値・リンク）"),
        ));
    }
    Ok(f)
}

/// 材料化した派生列は複数ソートの複合索引を持たない（第 1 キーか単独のソートだけ）。
pub(crate) fn sort_derived_fits(f: &BField, key: &str, keys: usize) -> Result<(), DataError> {
    if f.derived && keys > 1 {
        return Err(DataError::input_detail(
            InvalidCode::FieldNotSortable,
            Some(key),
            format!("派生フィールド '{key}' は複数キーのソートに使えません"),
        ));
    }
    Ok(())
}

/// まとめる列に使えるか（多値の列・リンクの列・システムの擬似列は使えない）。`name` は書いた名前（`別名.参照名` を含む）。
pub(crate) fn group_key_fits(f: &BField, name: &str) -> Result<(), DataError> {
    if f.multi() || f.is_pseudo() {
        return Err(DataError::input_detail(
            InvalidCode::FieldNotSortable,
            None,
            format!("フィールド '{name}' は集計/グループ化に使えません"),
        ));
    }
    Ok(())
}

/// 集計の列に使えるか（数の列だけ）。`name` は書いた名前（`別名.参照名` を含む）。
pub(crate) fn metric_fits(f: &BField, name: &str) -> Result<(), DataError> {
    if f.kind != FieldKind::Num {
        return Err(DataError::input_detail(
            InvalidCode::ValueTypeMismatch,
            None,
            format!("集計対象 '{name}' は number である必要があります"),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "field_use_tests.rs"]
mod tests;
