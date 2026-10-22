//! row_policy AST → SQL 述語コンパイラ（Task 9.3）。
//!
//! 出力はプレースホルダ付き SQL 断片と [`Bind`] 列。**値は必ずバインド**し、
//! SQL テキストへ埋め込むのは内部 ID 由来の JSONB キー（`"f{id}"`・Task 15.1）と
//! 固定エイリアスのみ（PIT-21: 文字列連結で値を埋め込む経路を作らない）。述語は参照名（key）で
//! 宣言され、コンパイル時にスキーマで `"f{id}"` へ解決する（key は不変なので解決は一意）。
//!
//! コンパイルは同期・材料（[`PolicyMaterial`]）は事前解決済みを受け取る。

use crate::field_key::is_json_key;
use crate::model::{FieldType, TableSchema};
use crate::policy::ast::{CmpOp, PolicyExpr, PolicyOperand};
use crate::policy::material::PolicyMaterial;
use crate::DataError;

pub(crate) use crate::query::sql::Bind;
use crate::query::sql::SqlBuf;

/// プレースホルダの採番先（v1 の [`BindSet`] と v2 の [`SqlBuf`] を同じコンパイラで使う）。
pub(crate) trait PushBind {
    fn push_bind(&mut self, b: Bind) -> String;
}

impl PushBind for SqlBuf {
    fn push_bind(&mut self, b: Bind) -> String {
        self.push(b)
    }
}

/// バインド列とプレースホルダ番号の採番器。
///
/// `offset` は既に消費済みのプレースホルダ数（例: `$1=tenant, $2=table` なら 2）。
pub(crate) struct BindSet {
    pub binds: Vec<Bind>,
    offset: usize,
}

impl BindSet {
    pub(crate) fn new(offset: usize) -> Self {
        BindSet {
            binds: Vec::new(),
            offset,
        }
    }

    /// 値を積み、対応するプレースホルダ（`$n`）を返す。
    pub(crate) fn push(&mut self, b: Bind) -> String {
        self.binds.push(b);
        format!("${}", self.offset + self.binds.len())
    }
}

impl PushBind for BindSet {
    fn push_bind(&mut self, b: Bind) -> String {
        self.push(b)
    }
}

/// レコード表の固定エイリアス（全読取クエリで共通・lookup 伝播では参照先が別名を使う）。
pub(crate) const RECORD_ALIAS: &str = "r";

/// 述語式を SQL 断片へコンパイルする。
///
/// 返る SQL は常に括弧で自己完結し、呼び出し側が `AND (...)` で合成できる。
/// JSONB の欠落フィールドは `->> = NULL` 経由で不成立（fail-closed）。
pub(crate) fn compile_expr<B: PushBind>(
    expr: &PolicyExpr,
    schema: &TableSchema,
    material: &PolicyMaterial,
    ctx_user_id: &str,
    alias: &str,
    binds: &mut B,
) -> Result<String, DataError> {
    match expr {
        PolicyExpr::Public => Ok("TRUE".to_string()),
        PolicyExpr::IsOwner => {
            let ph = binds.push_bind(Bind::Text(ctx_user_id.to_string()));
            Ok(format!("({alias}.owner = {ph})"))
        }
        PolicyExpr::HasRole { role, subtree } => {
            // ロール所属はホスト側で確定できる（材料に解決済み）。SQL には定数として畳み込む。
            let holds = material.has_role(role, *subtree);
            Ok(if holds { "TRUE" } else { "FALSE" }.to_string())
        }
        PolicyExpr::Any(children) => {
            let parts = children
                .iter()
                .map(|c| compile_expr(c, schema, material, ctx_user_id, alias, binds))
                .collect::<Result<Vec<_>, _>>()?;
            if parts.is_empty() {
                return Ok("FALSE".to_string()); // 検証で拒否済みだが防御的に不成立へ。
            }
            Ok(format!("({})", parts.join(" OR ")))
        }
        PolicyExpr::All(children) => {
            let parts = children
                .iter()
                .map(|c| compile_expr(c, schema, material, ctx_user_id, alias, binds))
                .collect::<Result<Vec<_>, _>>()?;
            if parts.is_empty() {
                return Ok("FALSE".to_string());
            }
            Ok(format!("({})", parts.join(" AND ")))
        }
        PolicyExpr::FieldCmp { field, op, value } => compile_field_cmp(
            schema,
            material,
            ctx_user_id,
            alias,
            binds,
            field,
            *op,
            value,
        ),
    }
}

#[allow(clippy::too_many_arguments)] // 内部関数・compile_expr からの一括委譲点。
pub(crate) fn compile_field_cmp<B: PushBind>(
    schema: &TableSchema,
    material: &PolicyMaterial,
    ctx_user_id: &str,
    alias: &str,
    binds: &mut B,
    field: &str,
    op: CmpOp,
    value: &PolicyOperand,
) -> Result<String, DataError> {
    let f = schema.field(field).ok_or_else(|| {
        DataError::Internal(format!("row_policy が未知フィールド '{field}' を参照"))
    })?;
    // 埋め込み直前の再検証（スキーマ検証と二重・PIT-21）。埋め込むのは "f{id}" のみ。
    let jk = f.json_key();
    if !is_json_key(&jk) {
        return Err(DataError::Internal(format!(
            "row_policy の JSONB キー '{jk}' が内部 ID 形式ではありません"
        )));
    }
    let accessor = format!("({alias}.data ->> '{jk}')");
    match value {
        PolicyOperand::UserId => {
            let ph = binds.push_bind(Bind::Text(ctx_user_id.to_string()));
            let sql = match op {
                CmpOp::Eq => format!("({accessor} = {ph})"),
                // Ne: 欠落フィールド（NULL）は不成立＝fail-closed（<> の NULL 伝播）。
                CmpOp::Ne => format!("({accessor} <> {ph})"),
                CmpOp::In => {
                    return Err(DataError::Internal(
                        "row_policy: $user.id に in は使えません（検証済みのはず）".into(),
                    ))
                }
            };
            Ok(sql)
        }
        PolicyOperand::UserRoles { subtree } => {
            let roles = material.roles(*subtree).to_vec();
            if roles.is_empty() {
                // 所属なし = この枝は不成立（SQL を発行せず定数化）。
                return Ok("FALSE".to_string());
            }
            let ph = binds.push_bind(Bind::TextArray(roles));
            Ok(format!("({accessor} = ANY({ph}::text[]))"))
        }
        PolicyOperand::Lit(v) => {
            let sql = match (f.field_type, op) {
                (FieldType::Number, CmpOp::Eq | CmpOp::Ne) => {
                    let n = v.as_f64().ok_or_else(|| {
                        DataError::Internal("row_policy: number リテラル不整合".into())
                    })?;
                    let ph = binds.push_bind(Bind::Number(n));
                    let op_sql = if op == CmpOp::Eq { "=" } else { "<>" };
                    format!("(({accessor})::numeric {op_sql} {ph}::numeric)")
                }
                (FieldType::Number, CmpOp::In) => {
                    return Err(DataError::Internal(
                        "row_policy: number の in は未対応（検証済みのはず）".into(),
                    ))
                }
                (_, CmpOp::Eq | CmpOp::Ne) => {
                    let s = v.as_str().ok_or_else(|| {
                        DataError::Internal("row_policy: text リテラル不整合".into())
                    })?;
                    let ph = binds.push_bind(Bind::Text(s.to_string()));
                    let op_sql = if op == CmpOp::Eq { "=" } else { "<>" };
                    format!("({accessor} {op_sql} {ph})")
                }
                (_, CmpOp::In) => {
                    let arr = v
                        .as_array()
                        .ok_or_else(|| DataError::Internal("row_policy: in リテラル不整合".into()))?
                        .iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect::<Vec<_>>();
                    if arr.is_empty() {
                        return Ok("FALSE".to_string());
                    }
                    let ph = binds.push_bind(Bind::TextArray(arr));
                    format!("({accessor} = ANY({ph}::text[]))")
                }
            };
            Ok(sql)
        }
    }
}

/// 読取用の合成済み述語 `(row_policy OR 個別共有)` を組む（合成の単一点）。
///
/// row_policy 未定義のテーブルは「テーブル viewer 全員が全行可視」（従来どおり・
/// 制限はオプトイン）。定義済みなら `(<policy>) OR r.id = ANY(<共有id>)`。
pub(crate) fn compile_read_predicate<B: PushBind>(
    schema: &TableSchema,
    material: &PolicyMaterial,
    ctx_user_id: &str,
    alias: &str,
    binds: &mut B,
) -> Result<String, DataError> {
    let Some(policy) = &schema.row_policy else {
        return Ok("TRUE".to_string());
    };
    let policy_sql = compile_expr(&policy.read, schema, material, ctx_user_id, alias, binds)?;
    if material.shared_record_ids.is_empty() {
        return Ok(format!("({policy_sql})"));
    }
    let ph = binds.push_bind(Bind::UuidArray(material.shared_record_ids.clone()));
    Ok(format!("({policy_sql} OR {alias}.id = ANY({ph}::uuid[]))"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FieldDef;
    use crate::policy::ast::RowPolicy;
    use serde_json::json;
    use std::sync::Arc;

    fn field(name: &str, ty: FieldType) -> FieldDef {
        FieldDef::new(name, ty)
    }

    /// applicant = f16 / dept = f17 / amount = f18（宣言順に 16 から採番）。
    fn schema_with_policy(read: PolicyExpr) -> TableSchema {
        let mut s = TableSchema {
            fields: vec![
                field("applicant", FieldType::UserRef),
                field("dept", FieldType::RoleRef),
                field("amount", FieldType::Number),
            ],
            status_field: None,
            row_policy: Some(RowPolicy { read, write: None }),
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        };
        crate::field_key::assign_field_ids(None, &mut s).unwrap();
        s
    }

    fn material(roles: &[&str], shared: &[uuid::Uuid]) -> PolicyMaterial {
        PolicyMaterial {
            roles_direct: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
            roles_effective: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
            shared_record_ids: shared.to_vec(),
            shares_truncated: false,
            role_members: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn compiles_typical_policy_with_binds_only() {
        let schema = schema_with_policy(PolicyExpr::Any(vec![
            PolicyExpr::FieldCmp {
                field: "applicant".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            PolicyExpr::FieldCmp {
                field: "dept".into(),
                op: CmpOp::In,
                value: PolicyOperand::UserRoles { subtree: true },
            },
        ]));
        let m = material(&["sales"], &[]);
        let mut binds = BindSet::new(2);
        let sql = compile_read_predicate(&schema, &m, "alice", "r", &mut binds).unwrap();
        assert_eq!(
            sql,
            "((((r.data ->> 'f16') = $3) OR ((r.data ->> 'f17') = ANY($4::text[]))))"
        );
        assert_eq!(binds.binds.len(), 2);
        // 値は SQL テキストに現れない（全てバインド）。
        assert!(!sql.contains("alice"));
        assert!(!sql.contains("sales"));
        // 参照名は SQL テキストに現れない（埋め込むのは内部 ID 由来のキーのみ）。
        assert!(!sql.contains("applicant") && !sql.contains("dept"));
    }

    #[test]
    fn shared_ids_add_or_branch() {
        let id = uuid::Uuid::new_v4();
        let schema = schema_with_policy(PolicyExpr::FieldCmp {
            field: "applicant".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        });
        let m = material(&[], &[id]);
        let mut binds = BindSet::new(0);
        let sql = compile_read_predicate(&schema, &m, "alice", "r", &mut binds).unwrap();
        assert!(sql.ends_with("OR r.id = ANY($2::uuid[]))"), "{sql}");
        // 共有 ID は SQL テキストでなく配列バインドに乗る（PIT-18）。
        assert!(!sql.contains(&id.to_string()));
    }

    #[test]
    fn has_role_folds_to_constant_and_empty_roles_fail_closed() {
        let schema = schema_with_policy(PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: true,
        });
        let mut binds = BindSet::new(0);
        let allowed =
            compile_read_predicate(&schema, &material(&["keiri"], &[]), "u", "r", &mut binds)
                .unwrap();
        assert_eq!(allowed, "(TRUE)");
        let denied =
            compile_read_predicate(&schema, &material(&[], &[]), "u", "r", &mut binds).unwrap();
        assert_eq!(denied, "(FALSE)");

        // 所属ロールが空のとき UserRoles 比較は定数 FALSE（空 ANY を発行しない）。
        let schema = schema_with_policy(PolicyExpr::FieldCmp {
            field: "dept".into(),
            op: CmpOp::In,
            value: PolicyOperand::UserRoles { subtree: false },
        });
        let sql =
            compile_read_predicate(&schema, &material(&[], &[]), "u", "r", &mut binds).unwrap();
        assert_eq!(sql, "(FALSE)");
    }

    #[test]
    fn no_policy_means_table_visibility() {
        let schema = TableSchema {
            fields: vec![field("t", FieldType::Text)],
            status_field: None,
            row_policy: None,
            field_policy: vec![],
            aggregate_min_rows: None,
            fsm_ref: None,
        };
        let mut binds = BindSet::new(0);
        let sql = compile_read_predicate(&schema, &PolicyMaterial::default(), "u", "r", &mut binds)
            .unwrap();
        assert_eq!(sql, "TRUE");
        assert!(binds.binds.is_empty());
    }

    #[test]
    fn lit_number_and_in_compile() {
        let schema = schema_with_policy(PolicyExpr::All(vec![
            PolicyExpr::FieldCmp {
                field: "amount".into(),
                op: CmpOp::Ne,
                value: PolicyOperand::Lit(json!(0)),
            },
            PolicyExpr::FieldCmp {
                field: "dept".into(),
                op: CmpOp::In,
                value: PolicyOperand::Lit(json!(["sales", "dev"])),
            },
        ]));
        let mut binds = BindSet::new(0);
        let sql =
            compile_read_predicate(&schema, &material(&[], &[]), "u", "r", &mut binds).unwrap();
        assert!(
            sql.contains("((r.data ->> 'f18'))::numeric <> $1::numeric"),
            "{sql}"
        );
        assert!(
            sql.contains("(r.data ->> 'f17') = ANY($2::text[])"),
            "{sql}"
        );
    }
}
