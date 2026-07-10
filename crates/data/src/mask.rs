//! フィールドマスク（Task 9.4・PIT-19）。
//!
//! 「表示を隠す」と「検索に使わせない」を**同一の判定**から強制する:
//! - 投影: 全読取応答（get/list/query/リビジョン差分/lookup 射影）から対象フィールドを除去
//! - 問い合わせ: filter/sort/group_by/metrics が対象フィールドを参照したら**実行前に 403**
//! - 書込: 読めないフィールドへの書込も拒否（盲目上書きの防止）
//!
//! `readable_by` はロール/ユーザーレベルの式（has_role/public とその any/all 合成）のみで、
//! リクエスト単位に一度だけホスト側で評価する（行の値に依存しない＝保存ビューでも
//! 実行時の閲覧者基準で再評価される）。

use std::collections::HashSet;

use authz::AuthContext;
use serde_json::Value;

use crate::model::{DataRecord, DataTable, FieldPatch};
use crate::policy::ast::PolicyExpr;
use crate::policy::material::{self, PolicyMaterial};
use crate::store::DataStore;
use crate::DataError;

/// ロール/ユーザーレベル式の評価（マスク判定・リクエストで一度だけ）。
///
/// 行依存の式（field_cmp / is_owner）はスキーマ検証で拒否済み。防御的に**マスク側に倒す**
/// （false = 読めない）。
fn eval_role_level(expr: &PolicyExpr, material: &PolicyMaterial) -> bool {
    match expr {
        PolicyExpr::Public => true,
        PolicyExpr::HasRole { role, subtree } => material.roles(*subtree).iter().any(|r| r == role),
        PolicyExpr::Any(children) => children.iter().any(|c| eval_role_level(c, material)),
        PolicyExpr::All(children) => children.iter().all(|c| eval_role_level(c, material)),
        PolicyExpr::FieldCmp { .. } | PolicyExpr::IsOwner => false,
    }
}

impl DataStore {
    /// 実行主体に対して**読めない**フィールド名集合を返す（field_policy 未定義なら空）。
    pub(crate) async fn masked_fields(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
    ) -> Result<HashSet<String>, DataError> {
        if table.schema.field_policy.is_empty() {
            return Ok(HashSet::new());
        }
        let exprs: Vec<&PolicyExpr> = table
            .schema
            .field_policy
            .iter()
            .map(|p| &p.readable_by)
            .collect();
        let m = material::resolve(ctx, self.authz.as_ref(), &exprs).await?;
        Ok(table
            .schema
            .field_policy
            .iter()
            .filter(|p| !eval_role_level(&p.readable_by, &m))
            .map(|p| p.field.clone())
            .collect())
    }

    /// 読取応答の投影段（レコード群からマスク対象フィールドを除去する単一点）。
    pub(crate) fn apply_mask_records(masked: &HashSet<String>, records: &mut [DataRecord]) {
        if masked.is_empty() {
            return;
        }
        for r in records {
            if let Some(obj) = r.data.as_object_mut() {
                obj.retain(|k, _| !masked.contains(k));
            }
        }
    }

    /// リビジョン差分の投影段（マスク対象フィールドの差分は old/new を伏せて残す）。
    ///
    /// 差分行そのものを消すと「何かが変わった」ことまで隠れて履歴の連続性が壊れるため、
    /// フィールド名は残し値のみ `null` に置換する（変更事実は監査対象・値は不可視）。
    pub(crate) fn apply_mask_patches(masked: &HashSet<String>, patches: &mut [FieldPatch]) {
        if masked.is_empty() {
            return;
        }
        for p in patches {
            if masked.contains(&p.field) {
                p.old = Value::Null;
                p.new = Value::Null;
            }
        }
    }

    /// 問い合わせ面の検査: `field` がマスク対象なら **403**（PIT-19: 検索に使わせない）。
    pub(crate) fn ensure_queryable(masked: &HashSet<String>, field: &str) -> Result<(), DataError> {
        if masked.contains(field) {
            return Err(DataError::Forbidden);
        }
        Ok(())
    }

    /// 書込面の検査: 読めないフィールドへの書込を拒否（盲目上書き防止）。
    pub(crate) fn ensure_writable_fields(
        masked: &HashSet<String>,
        payload: &serde_json::Map<String, Value>,
    ) -> Result<(), DataError> {
        for key in payload.keys() {
            if masked.contains(key) {
                return Err(DataError::Forbidden);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn material(roles: &[&str]) -> PolicyMaterial {
        PolicyMaterial {
            roles_direct: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
            roles_effective: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
            shared_record_ids: vec![],
            shares_truncated: false,
        }
    }

    #[test]
    fn role_level_eval() {
        let keiri = PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: true,
        };
        assert!(eval_role_level(&keiri, &material(&["keiri"])));
        assert!(!eval_role_level(&keiri, &material(&[])));
        assert!(eval_role_level(&PolicyExpr::Public, &material(&[])));
        // 行依存式は防御的に「読めない」へ倒す。
        assert!(!eval_role_level(
            &PolicyExpr::IsOwner,
            &material(&["keiri"])
        ));
        let any = PolicyExpr::Any(vec![keiri, PolicyExpr::Public]);
        assert!(eval_role_level(&any, &material(&[])));
    }

    #[test]
    fn patches_masked_values_but_keep_field() {
        let mut patches = vec![FieldPatch {
            field: "salary".into(),
            old: serde_json::json!(100),
            new: serde_json::json!(200),
        }];
        let masked: HashSet<String> = ["salary".to_string()].into();
        DataStore::apply_mask_patches(&masked, &mut patches);
        assert_eq!(patches[0].field, "salary");
        assert_eq!(patches[0].old, Value::Null);
        assert_eq!(patches[0].new, Value::Null);
    }
}
