//! row_policy の宣言 AST（Task 9.3・閉じた文法）。
//!
//! # 設計上の制約（意図的な非機能）
//!
//! - **`Not` は持たない**: 否定はフィールドマスク（Task 9.4）と組み合わさると
//!   「マスクで見えないはずの値の否定条件」で情報が漏れる相互作用を生むため、
//!   v1 の文法から除外する（必要になったら脅威モデルを更新してから足す）。
//! - **自由式・関数呼び出しは持たない**: SQL へコンパイルする材料は
//!   「宣言済みフィールド × 閉じた演算子 × リテラル/実行主体属性」のみ。
//!   LLM/開発者由来の任意式がそのまま SQL になる経路を作らない（PIT-21）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 行ポリシー（テーブルスキーマの `row_policy`）。
///
/// `read` は取得・一覧・集計・リビジョン・lookup 解決のすべてに強制合成される。
/// `write` は update/delete/遷移（Task 9.10）の対象行に適用される（未指定は read と同じ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RowPolicy {
    pub read: PolicyExpr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<PolicyExpr>,
}

impl RowPolicy {
    /// write 述語（未指定なら read を流用）。
    pub fn write_expr(&self) -> &PolicyExpr {
        self.write.as_ref().unwrap_or(&self.read)
    }
}

/// 述語式（閉じた文法・ネスト上限は検証で強制）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyExpr {
    /// いずれかを満たす（OR）。空は不成立（fail-closed）。
    Any(Vec<PolicyExpr>),
    /// すべてを満たす（AND）。空は不成立（fail-closed）。
    All(Vec<PolicyExpr>),
    /// フィールド比較。
    FieldCmp {
        field: String,
        op: CmpOp,
        value: PolicyOperand,
    },
    /// 実行主体が指定ロール（subtree=true なら配下ロール込み＝FGA の member 展開）に属する。
    HasRole { role: String, subtree: bool },
    /// レコード作成者（`data_record.owner`）本人。
    IsOwner,
    /// テーブル viewer 全員に公開。
    Public,
}

/// 比較演算子（閉じた集合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    In,
}

/// 比較オペランド。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOperand {
    /// リテラル（text/select 値・text 配列は op=in 用）。
    Lit(serde_json::Value),
    /// 実行主体のユーザー id（`$user.id`）。
    UserId,
    /// 実行主体の所属ロール集合（`$user.roles`・subtree=true は配下込みの実効集合）。
    UserRoles { subtree: bool },
}

/// ネスト深さ上限（検証・コンパイルの再帰保護）。
pub(crate) const MAX_POLICY_DEPTH: usize = 8;
/// Any/All の子要素数上限。
pub(crate) const MAX_POLICY_BRANCHES: usize = 32;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serde_snake_case_and_closed() {
        let expr: PolicyExpr = serde_json::from_value(json!({
            "any": [
                { "field_cmp": { "field": "applicant", "op": "eq", "value": "user_id" } },
                { "has_role": { "role": "経理", "subtree": true } }
            ]
        }))
        .unwrap();
        match &expr {
            PolicyExpr::Any(children) => assert_eq!(children.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
        // 未知バリアントは fail-closed。
        let bad: Result<PolicyExpr, _> =
            serde_json::from_value(json!({ "raw_sql": "1=1" }));
        assert!(bad.is_err());
    }

    #[test]
    fn write_falls_back_to_read() {
        let p = RowPolicy {
            read: PolicyExpr::Public,
            write: None,
        };
        assert_eq!(p.write_expr(), &PolicyExpr::Public);
    }
}
