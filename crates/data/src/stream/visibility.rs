//! 購読者ごとの可視性判定（data-platform §9.3・§9.4・PIT-70・PIT-73）。
//!
//! # 述語の分解（グループ化が効く形）
//!
//! 素朴に「畳み込み後の SQL ＋ バインドのハッシュ」で購読者を束ねると、`IsOwner` は principal id が、
//! 個別共有は `shared_ids` がバインドに入るので、**購読者数だけグループができて削減が効かない**。
//! そこで行述語の葉を 3 種に分ける。
//!
//! | 葉 | 評価 | 共有 |
//! |---|---|---|
//! | `FieldCmp`（リテラル・`$user.roles`） | **SQL**（v1 のコンパイラ・同じ意味論） | ロール構成が同じ購読者でグループ化し、**グループごとに 1 回** |
//! | `HasRole` / `Public` | 材料から定数に畳む | グループの鍵に入る |
//! | `IsOwner` / `FieldCmp($user.id)` | SQL を発行しない（イベントの owner・旧値と principal id の O(1) 比較） | 購読者ごと |
//! | 個別共有 | `record_id ∈ 購読者の共有集合`（O(1) のハッシュ集合判定） | 購読者ごと |
//!
//! 木（`any` / `all`）は Rust で組み立て直す。v1 の SQL は 3 値論理だが、文法に `NOT` が無い
//! （単調）ので「葉の NULL を偽にして 2 値で畳む」と同じ結果になる（葉を `coalesce(…, false)`）。
//! これで**評価回数はロール構成の種類数に比例し、購読者数に比例しない**（1,000 人 × 3 ロール = 3 回）。
//!
//! # 事後検査
//!
//! 可視と判定した組は、REST と同じ Rust の独立した評価器（[`crate::query::planner::verify`]）で
//! 再評価し、不一致は落として警報する（二段 authz・PIT-76 と同じ構え）。

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::field_key::{is_json_key, json_key};
use crate::model::TableSchema;
use crate::policy::ast::{CmpOp, PolicyExpr, PolicyOperand};
use crate::policy::compile::{compile_field_cmp, RECORD_ALIAS};
use crate::policy::material::PolicyMaterial;
use crate::query::planner::verify::{eval_read, PolicyEnv, RowView};
use crate::query::sql::{bind_query, Bind, SqlBuf};
use crate::{map_db, DataError};

/// 葉の種別。
#[derive(Debug, Clone)]
enum LeafKind {
    /// 材料から定数に畳む（`has_role` / `public`）。
    Const(PolicyExpr),
    /// SQL で評価する（リテラル・`$user.roles` の比較）。
    Common(PolicyExpr),
    /// 購読者本人に依存する（`is_owner`）。
    Owner,
    /// `$user.id` との比較（保存形キー・演算子）。
    UserId { key: String, op: CmpOp },
}

/// 木（葉は `leaves` の添字）。
#[derive(Debug, Clone)]
enum Node {
    Any(Vec<Node>),
    All(Vec<Node>),
    Leaf(usize),
}

/// テーブルの行述語（read）を分解したもの。
#[derive(Debug, Clone)]
pub(crate) struct Decomposed {
    leaves: Vec<LeafKind>,
    root: Option<Node>,
}

impl Decomposed {
    pub(crate) fn of(schema: &TableSchema) -> Result<Self, DataError> {
        let mut leaves = Vec::new();
        let root = match &schema.row_policy {
            Some(p) => Some(build(&p.read, schema, &mut leaves)?),
            None => None,
        };
        Ok(Decomposed { leaves, root })
    }

    /// 行述語を持つか（無ければテーブル viewer 全員が全行可視・共有も効かない）。
    pub(crate) fn has_policy(&self) -> bool {
        self.root.is_some()
    }

    fn common_indices(&self) -> Vec<usize> {
        self.leaves
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l, LeafKind::Common(_)))
            .map(|(i, _)| i)
            .collect()
    }
}

fn build(
    e: &PolicyExpr,
    schema: &TableSchema,
    leaves: &mut Vec<LeafKind>,
) -> Result<Node, DataError> {
    let leaf = |k: LeafKind, leaves: &mut Vec<LeafKind>| {
        leaves.push(k);
        Node::Leaf(leaves.len() - 1)
    };
    Ok(match e {
        PolicyExpr::Any(v) => Node::Any(
            v.iter()
                .map(|c| build(c, schema, leaves))
                .collect::<Result<_, _>>()?,
        ),
        PolicyExpr::All(v) => Node::All(
            v.iter()
                .map(|c| build(c, schema, leaves))
                .collect::<Result<_, _>>()?,
        ),
        PolicyExpr::Public | PolicyExpr::HasRole { .. } => leaf(LeafKind::Const(e.clone()), leaves),
        PolicyExpr::IsOwner => leaf(LeafKind::Owner, leaves),
        PolicyExpr::FieldCmp {
            field,
            op,
            value: PolicyOperand::UserId,
        } => {
            let f = schema.field(field).ok_or_else(|| {
                DataError::Internal(format!("row_policy が未知フィールド '{field}' を参照"))
            })?;
            let key = json_key(f.id);
            if !is_json_key(&key) {
                return Err(DataError::Internal(
                    "row_policy の JSONB キーが不正です".into(),
                ));
            }
            leaf(LeafKind::UserId { key, op: *op }, leaves)
        }
        PolicyExpr::FieldCmp { .. } => leaf(LeafKind::Common(e.clone()), leaves),
    })
}

/// 判定対象の行の状態（before は outbox の旧値・after は現在の本体。保存形）。
#[derive(Debug, Clone)]
pub(crate) struct RowState {
    pub id: Uuid,
    pub owner: String,
    pub data: Value,
}

/// 購読者 1 人の判定材料（TTL 内は持ち越した材料から作る）。
pub(crate) struct Viewer<'a> {
    pub principal: &'a str,
    pub material: &'a PolicyMaterial,
    pub shared: &'a HashSet<Uuid>,
}

/// グループ（ロール共通部の SQL ＋バインド＋定数葉が一致する購読者）の鍵と SQL。
pub(crate) struct GroupPlan {
    pub key: String,
    /// 共通葉の SQL 断片（`common_indices` の順）。
    sql: Vec<String>,
    binds: Vec<Bind>,
}

/// 購読者のグループの鍵と、そのグループで共通部を評価する SQL を組む。
///
/// 鍵にはロール共通部だけを入れる（principal id・共有集合は入れない）ので、ロール構成が同じ
/// 購読者は同じ鍵になる。
pub(crate) fn group_plan(
    d: &Decomposed,
    schema: &TableSchema,
    v: &Viewer<'_>,
) -> Result<GroupPlan, DataError> {
    // $1 は判定対象の行の集合（jsonb）。
    let mut buf = SqlBuf {
        binds: vec![Bind::Text(String::new())],
        table: None,
        aliases: 0,
        skip_text: false,
    };
    let mut sql = Vec::new();
    for i in d.common_indices() {
        let LeafKind::Common(PolicyExpr::FieldCmp { field, op, value }) = &d.leaves[i] else {
            continue;
        };
        // $user.id は共通葉に来ない（分解で UserId に振り分け済み）ので principal は SQL に入らない。
        sql.push(compile_field_cmp(
            schema,
            v.material,
            "",
            RECORD_ALIAS,
            &mut buf,
            field,
            *op,
            value,
        )?);
    }
    let consts: Vec<bool> = d
        .leaves
        .iter()
        .filter_map(|l| match l {
            LeafKind::Const(e) => Some(const_value(e, v.material)),
            _ => None,
        })
        .collect();
    let binds: Vec<Bind> = buf.binds.split_off(1);
    let key = format!("{consts:?}|{sql:?}|{binds:?}");
    Ok(GroupPlan { key, sql, binds })
}

fn const_value(e: &PolicyExpr, m: &PolicyMaterial) -> bool {
    match e {
        PolicyExpr::Public => true,
        PolicyExpr::HasRole { role, subtree } => m.has_role(role, *subtree),
        _ => false,
    }
}

/// グループの共通葉を、行の集合に対して **SQL 1 文**で評価する（グループごとに 1 回）。
///
/// 返り値は行ごとの共通葉の真偽（`common_indices` の順）。共通葉が無ければ SQL は発行しない。
pub(crate) async fn eval_group(
    conn: &mut PgConnection,
    plan: &GroupPlan,
    rows: &[RowState],
) -> Result<Vec<Vec<bool>>, DataError> {
    if plan.sql.is_empty() || rows.is_empty() {
        return Ok(vec![Vec::new(); rows.len()]);
    }
    let set: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| json!({ "ord": i, "id": r.id, "owner": r.owner, "data": r.data }))
        .collect();
    let cols: Vec<String> = plan
        .sql
        .iter()
        .map(|s| format!("coalesce({s}, false)"))
        .collect();
    let text = format!(
        "SELECT {a}.ord, ARRAY[{cols}]::bool[] FROM jsonb_to_recordset($1::jsonb) \
         AS {a}(ord int4, id uuid, owner text, data jsonb) ORDER BY {a}.ord",
        a = RECORD_ALIAS,
        cols = cols.join(", ")
    );
    let mut binds = vec![Bind::Text(Value::Array(set).to_string())];
    binds.extend(plan.binds.iter().cloned());
    let rows_out = bind_query(sqlx::query(&text), &binds)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
    let mut out = vec![Vec::new(); rows.len()];
    for r in rows_out {
        let ord: i32 = r.try_get(0).map_err(map_db)?;
        let vals: Vec<bool> = r.try_get(1).map_err(map_db)?;
        if let Some(slot) = usize::try_from(ord).ok().and_then(|i| out.get_mut(i)) {
            *slot = vals;
        }
    }
    Ok(out)
}

/// `->>` 相当の text（null・欠落は None・v1 と同じ）。
fn scalar_text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// 1 行 × 1 購読者の可視性（共通葉は [`eval_group`] の結果を使う）。
pub(crate) fn visible(d: &Decomposed, v: &Viewer<'_>, row: &RowState, common: &[bool]) -> bool {
    let Some(root) = &d.root else {
        return true;
    };
    let common_idx: BTreeMap<usize, bool> = d
        .common_indices()
        .into_iter()
        .zip(common.iter().copied())
        .collect();
    let leaf = |i: usize| -> bool {
        match &d.leaves[i] {
            LeafKind::Const(e) => const_value(e, v.material),
            LeafKind::Common(_) => common_idx.get(&i).copied().unwrap_or(false),
            LeafKind::Owner => row.owner == v.principal,
            LeafKind::UserId { key, op } => {
                let text = scalar_text(row.data.get(key));
                match op {
                    CmpOp::Eq => text.as_deref() == Some(v.principal),
                    // 値なしは不成立（v1 の `<>` の NULL 伝播・fail-closed）。
                    CmpOp::Ne => text.is_some_and(|t| t != v.principal),
                    CmpOp::In => false,
                }
            }
        }
    };
    fn walk(n: &Node, leaf: &dyn Fn(usize) -> bool) -> bool {
        match n {
            Node::Any(c) => !c.is_empty() && c.iter().any(|x| walk(x, leaf)),
            Node::All(c) => !c.is_empty() && c.iter().all(|x| walk(x, leaf)),
            Node::Leaf(i) => leaf(*i),
        }
    }
    walk(root, &leaf) || v.shared.contains(&row.id)
}

/// 事後検査（REST と同じ独立した評価器）。可視と判定した組だけを確かめる。
pub(crate) fn verify(schema: &TableSchema, v: &Viewer<'_>, row: &RowState) -> bool {
    let env = PolicyEnv {
        schema,
        material: v.material,
        user: v.principal,
        shared: v.shared,
    };
    eval_read(
        &env,
        &RowView {
            id: row.id,
            owner: &row.owner,
            data: &row.data,
        },
    )
}

#[cfg(test)]
#[path = "visibility_tests.rs"]
mod tests;
