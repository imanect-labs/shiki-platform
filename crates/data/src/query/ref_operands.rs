//! ユーザー・部署（`user_ref` / `role_ref`）の列の比較値の形式の検査（検証 2-A-P2-3）。
//!
//! 比較値は**ユーザー・ロールの ID**。表示名（「山田 太郎」）で絞ると黙って 0 件になり、利用者は誤りに
//! 気付けない。束縛（型・マスク・存在しない列の拒否）の後で、等値・`in` の比較値が書込と同じ ID の形式
//! （[`crate::ref_id`]）かを確かめ、合わなければ 400 `value_type_mismatch` にする。
//!
//! **存在は確かめない**: 退職者などディレクトリから消えた人の ID で過去の行を絞り込むのは正当な使い方。
//! ディレクトリを引かないので、他テナントはもちろん自テナントの在籍の有無も応答から分からない。
//! 部署の ID は空白や非 ASCII を含み得るため、部署の表示名は形式だけでは弾けないことがある（`ref_id` 参照）。
//! リンク先の条件（`link`）の中は参照先のスキーマで束縛されるので対象外。

use serde_json::Value;

use crate::model::{FieldType, TableSchema};
use crate::query::ir::{CondOp, Condition};
use crate::ref_id::{is_role_id, is_user_id};
use crate::{DataError, InvalidCode};

/// 条件のユーザー・部署の比較値が ID の形式かを確かめる（合わなければ 400 `value_type_mismatch`）。
pub(crate) fn check_ref_operands(
    schema: &TableSchema,
    filter: Option<&Condition>,
) -> Result<(), DataError> {
    match filter {
        None => Ok(()),
        Some(c) => check(c, schema),
    }
}

fn check(c: &Condition, schema: &TableSchema) -> Result<(), DataError> {
    match c {
        Condition::All(v) | Condition::Any(v) => v.iter().try_for_each(|x| check(x, schema)),
        Condition::Not(x) => check(x, schema),
        Condition::Link(_) => Ok(()),
        Condition::Cmp(cmp) => {
            let Some(f) = schema.field(&cmp.field) else {
                return Ok(());
            };
            let (is_id, what): (fn(&str) -> bool, &str) = match f.field_type {
                FieldType::UserRef => (is_user_id, "ユーザー"),
                FieldType::RoleRef => (is_role_id, "部署（ロール）"),
                _ => return Ok(()),
            };
            let vals: Vec<&Value> = match (cmp.op, &cmp.value) {
                (CondOp::Eq | CondOp::Neq, Some(v)) => vec![v],
                (CondOp::In, Some(Value::Array(a))) => a.iter().collect(),
                _ => Vec::new(),
            };
            // 文字列でない値は束縛が型で拒否する。ここは文字列の形だけを見る。
            if vals.iter().filter_map(|v| v.as_str()).all(is_id) {
                return Ok(());
            }
            let key = cmp.field.as_str();
            Err(DataError::input_detail(
                InvalidCode::ValueTypeMismatch,
                Some(key),
                format!("'{key}' の絞り込みは{what}の ID で指定してください（表示名では絞り込めません）"),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FieldDef;
    use serde_json::json;

    fn schema() -> TableSchema {
        TableSchema {
            fields: vec![
                FieldDef::new("who", FieldType::UserRef),
                FieldDef::new("dept", FieldType::RoleRef),
                FieldDef::new("title", FieldType::Text),
            ],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
            rag: None,
        }
    }

    fn code(f: &str, op: CondOp, v: Value) -> Option<InvalidCode> {
        let c = Condition::cmp(f, op, Some(v));
        check_ref_operands(&schema(), Some(&c))
            .err()
            .and_then(|e| e.invalid_code())
    }

    #[test]
    fn well_formed_ids_pass_without_a_directory() {
        // ディレクトリに居ない（退職した）人の ID も形式が正しければ通る。
        assert_eq!(code("who", CondOp::Eq, json!("departed-42")), None);
        assert_eq!(code("who", CondOp::In, json!(["alice", "u9"])), None);
        assert_eq!(code("dept", CondOp::Neq, json!("acme/営業 第1部")), None);
        assert_eq!(code("title", CondOp::Eq, json!("山田 太郎")), None);
    }

    #[test]
    fn display_names_and_malformed_ids_are_rejected() {
        let m = Some(InvalidCode::ValueTypeMismatch);
        assert_eq!(code("who", CondOp::Eq, json!("山田 太郎")), m);
        assert_eq!(code("who", CondOp::In, json!(["alice", "Taro Yamada"])), m);
        assert_eq!(code("dept", CondOp::Eq, json!("role:sales")), m);
        let nested = Condition::Not(Box::new(Condition::All(vec![Condition::cmp(
            "who",
            CondOp::Neq,
            Some(json!("山田")),
        )])));
        assert!(check_ref_operands(&schema(), Some(&nested)).is_err());
    }
}
