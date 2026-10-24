//! ユーザー・部署（`user_ref` / `role_ref`）の列の比較値の型検査（検証 2-A-P2-3）。
//!
//! 比較値は**ユーザー・ロールの ID**。表示名（「山田 太郎」）や存在しない ID で絞ると、黙って 0 件になって
//! 利用者は誤りに気付けない。束縛（型・マスク・存在しない列の拒否）の後で、等値・`in` の比較値が
//! テナントのディレクトリに居るかを確かめ、居なければ 400 `value_type_mismatch` にする（書込の検証と同じ
//! 解決器・ディレクトリは利用者の選択肢として誰でも引けるので、存在の有無はオラクルにならない）。
//! リンク先の条件（`link`）の中は参照先のスキーマで束縛されるので対象外。

use std::collections::BTreeSet;

use authz::AuthContext;
use serde_json::Value;

use crate::model::{FieldType, TableSchema};
use crate::query::ir::{CondOp, Condition};
use crate::store::DataStore;
use crate::{DataError, InvalidCode};

/// 1 クエリで確かめる値の上限（条件の葉の上限 × `in` の上限より十分小さい・超えた分は確かめない）。
const MAX_CHECKED: usize = 64;

fn collect<'a>(
    c: &'a Condition,
    schema: &TableSchema,
    out: &mut BTreeSet<(&'a str, bool, &'a str)>,
) {
    match c {
        Condition::All(v) | Condition::Any(v) => v.iter().for_each(|x| collect(x, schema, out)),
        Condition::Not(x) => collect(x, schema, out),
        Condition::Link(_) => {}
        Condition::Cmp(cmp) => {
            let Some(f) = schema.field(&cmp.field) else {
                return;
            };
            if !matches!(f.field_type, FieldType::UserRef | FieldType::RoleRef) {
                return;
            }
            let vals: Vec<&Value> = match (cmp.op, &cmp.value) {
                (CondOp::Eq | CondOp::Neq, Some(v)) => vec![v],
                (CondOp::In, Some(Value::Array(a))) => a.iter().collect(),
                _ => Vec::new(),
            };
            for v in vals {
                if let Some(s) = v.as_str() {
                    out.insert((cmp.field.as_str(), f.field_type == FieldType::UserRef, s));
                }
            }
        }
    }
}

impl DataStore {
    /// 条件のユーザー・部署の比較値がディレクトリに居るかを確かめる（居なければ 400）。
    pub(crate) async fn check_ref_operands(
        &self,
        ctx: &AuthContext,
        schema: &TableSchema,
        filter: Option<&Condition>,
    ) -> Result<(), DataError> {
        let Some(filter) = filter else {
            return Ok(());
        };
        let mut vals = BTreeSet::new();
        collect(filter, schema, &mut vals);
        for (key, is_user, v) in vals.into_iter().take(MAX_CHECKED) {
            let found = if is_user {
                self.resolver.user_exists(ctx, v).await
            } else {
                self.resolver.role_exists(ctx, v).await
            };
            match found {
                Ok(true) => {}
                Ok(false) => {
                    let what = if is_user {
                        "ユーザー"
                    } else {
                        "部署（ロール）"
                    };
                    return Err(DataError::input_detail(
                        InvalidCode::ValueTypeMismatch,
                        Some(key),
                        format!(
                            "'{key}' の絞り込みは{what}の ID で指定してください（表示名・存在しない ID では絞り込めません）"
                        ),
                    ));
                }
                // ディレクトリに届かないときは絞り込みを止めない（結果は 0 件になり得るだけ・ログに残す）。
                Err(e) => {
                    tracing::warn!(error = %e, "絞り込みのユーザー・部署の確認に失敗（確かめずに続ける）");
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
