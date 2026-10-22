//! 派生フィールドの依存 DAG（data-platform §7.3・§7.4・PIT-71）。
//!
//! - **循環検出**: スキーマ保存時に、保存するテーブルから参照先テーブルを推移的に辿って依存グラフを作り、
//!   循環があれば 422（`a → b → a`・テーブルを跨ぐ循環も含む）。
//! - **型推論**: ロールアップの演算と対象の型・索引に載せる表（num / text）を決める。
//! - **材料化可否**: 4 階層すべて（①テーブル ReBAC の包含 ②参照先の `row_policy` ③参照先の `field_policy`
//!   ④参照先のレコード個別共有）を見て、1 つでも閲覧者依存があれば `per_viewer`。`now` を含めば
//!   `volatile`。①の包含の証明は呼び出し側（OpenFGA のタプル比較）が渡す。
//!
//! 多段参照（参照先の派生列を対象にするロールアップ等）は**常に `per_viewer`**（推移的な判定を手抜きせず、
//! 安全側へ倒す）。

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::types::{Materialization, RollupOp};
use crate::formula::FormulaExpr;
use crate::model::{FieldDef, FieldType, TableKeys, TableSchema};
use crate::DataError;

/// 依存を辿るノード数の上限（テーブルを跨ぐ連鎖の深さの防御）。
pub(crate) const MAX_DERIVED_CHAIN: usize = 32;
/// スキーマ保存時に読み込む参照先テーブル数の上限。
pub(crate) const MAX_CATALOG_TABLES: usize = 16;

/// 検証に使うテーブルの情報（保存中のテーブルは保存しようとしているスキーマで上書きする）。
#[derive(Debug, Clone)]
pub(crate) struct CatalogTable {
    pub schema: TableSchema,
    pub app_id: Option<Uuid>,
    pub has_record_shares: bool,
    pub keys: TableKeys,
}

/// 依存グラフの対象（テーブル群）。
#[derive(Debug, Default)]
pub(crate) struct Catalog {
    pub tables: HashMap<Uuid, CatalogTable>,
}

/// 値の型（推論結果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VType {
    Number,
    Text,
    Date,
    DateTime,
    Bool,
    /// 多値（multi_select・多値リンク越しの lookup）。
    List,
    Unknown,
}

impl VType {
    /// 外部契約の型（不明は `None`）。
    pub(crate) fn result_type(self) -> Option<super::types::ResultType> {
        use super::types::ResultType as R;
        match self {
            VType::Number => Some(R::Number),
            VType::Text => Some(R::Text),
            VType::Date => Some(R::Date),
            VType::DateTime => Some(R::DateTime),
            VType::Bool => Some(R::Bool),
            VType::List => Some(R::List),
            VType::Unknown => None,
        }
    }
}

impl Catalog {
    fn field(&self, table: Uuid, key: &str) -> Option<&FieldDef> {
        self.tables.get(&table)?.schema.field(key)
    }

    /// リンク列 `link` の参照先テーブル。
    pub(crate) fn ref_of(&self, table: Uuid, link: &str) -> Option<Uuid> {
        self.field(table, link)
            .filter(|f| f.field_type == FieldType::RecordRef)
            .and_then(|f| f.ref_table)
    }

    /// ノード `(table, key)` が直接依存するノード。
    fn deps(&self, table: Uuid, key: &str) -> Vec<(Uuid, String)> {
        let Some(f) = self.field(table, key) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut cross = |link: &str, target: Option<&String>| {
            out.push((table, link.to_string()));
            if let (Some(rt), Some(t)) = (self.ref_of(table, link), target) {
                out.push((rt, t.clone()));
            }
        };
        match f.field_type {
            FieldType::Lookup => {
                if let Some(d) = &f.lookup {
                    cross(&d.via_field, Some(&d.target_field));
                }
            }
            FieldType::Rollup => {
                if let Some(d) = &f.rollup {
                    cross(&d.link, d.target.as_ref());
                }
            }
            FieldType::Computed => {
                if let Some(d) = &f.computed {
                    out.extend(d.fields.iter().map(|k| (table, k.clone())));
                }
            }
            FieldType::Formula => {
                if let Some(e) = &f.formula {
                    e.walk(&mut |n| match n {
                        FormulaExpr::Field(k) => out.push((table, k.clone())),
                        FormulaExpr::Rollup { link, target, .. } => {
                            out.push((table, link.clone()));
                            if let (Some(rt), Some(t)) = (self.ref_of(table, link), target) {
                                out.push((rt, t.clone()));
                            }
                        }
                        _ => {}
                    });
                }
            }
            _ => {}
        }
        out
    }

    /// `table` の派生フィールドから辿って循環が無いことを確かめる（あれば 422）。
    pub(crate) fn check_cycles(&self, table: Uuid) -> Result<(), DataError> {
        let Some(t) = self.tables.get(&table) else {
            return Ok(());
        };
        // 0 = 未訪問, 1 = 訪問中, 2 = 完了。
        let mut state: HashMap<(Uuid, String), u8> = HashMap::new();
        for f in t.schema.fields.iter().filter(|f| f.field_type.is_derived()) {
            let mut path = Vec::new();
            self.dfs((table, f.key.clone()), &mut state, &mut path)?;
        }
        Ok(())
    }

    fn dfs(
        &self,
        node: (Uuid, String),
        state: &mut HashMap<(Uuid, String), u8>,
        path: &mut Vec<(Uuid, String)>,
    ) -> Result<(), DataError> {
        match state.get(&node) {
            Some(2) => return Ok(()),
            Some(1) => {
                let start = path.iter().position(|n| *n == node).unwrap_or(0);
                let names: Vec<&str> = path[start..]
                    .iter()
                    .map(|(_, k)| k.as_str())
                    .chain(std::iter::once(node.1.as_str()))
                    .collect();
                return Err(DataError::Invalid(format!(
                    "派生フィールドの依存が循環しています（{}）",
                    names.join(" → ")
                )));
            }
            _ => {}
        }
        if path.len() >= MAX_DERIVED_CHAIN {
            return Err(DataError::Invalid(format!(
                "派生フィールドの依存が深すぎます（最大 {MAX_DERIVED_CHAIN}）"
            )));
        }
        state.insert(node.clone(), 1);
        path.push(node.clone());
        for d in self.deps(node.0, &node.1) {
            self.dfs(d, state, path)?;
        }
        path.pop();
        state.insert(node, 2);
        Ok(())
    }

    /// 値の型を推論する（循環は検出済みの前提・深さで打ち切る）。
    pub(crate) fn infer(&self, table: Uuid, key: &str) -> VType {
        self.infer_at(table, key, 0)
    }

    fn infer_at(&self, table: Uuid, key: &str, depth: usize) -> VType {
        if depth > MAX_DERIVED_CHAIN {
            return VType::Unknown;
        }
        let Some(f) = self.field(table, key) else {
            return VType::Unknown;
        };
        match f.field_type {
            FieldType::Number => VType::Number,
            FieldType::Date => VType::Date,
            FieldType::DateTime => VType::DateTime,
            FieldType::MultiSelect => VType::List,
            FieldType::RecordRef => {
                if f.link.is_some() {
                    VType::List
                } else {
                    VType::Text
                }
            }
            FieldType::Text
            | FieldType::Select
            | FieldType::UserRef
            | FieldType::RoleRef
            | FieldType::FileRef => VType::Text,
            FieldType::Computed => match f.computed.as_ref().map(|c| c.op) {
                Some(crate::model::ComputedOp::Sum) => VType::Number,
                _ => VType::Text,
            },
            FieldType::Lookup => {
                let Some(d) = &f.lookup else {
                    return VType::Unknown;
                };
                if !self.single_link(table, &d.via_field) {
                    return VType::List;
                }
                match self.ref_of(table, &d.via_field) {
                    Some(rt) => self.infer_at(rt, &d.target_field, depth + 1),
                    None => VType::Unknown,
                }
            }
            FieldType::Rollup => match &f.rollup {
                Some(d) => self.rollup_type(table, &d.link, d.target.as_deref(), d.op, depth),
                None => VType::Unknown,
            },
            FieldType::Formula => match &f.formula {
                Some(e) => self.infer_expr(table, e, depth),
                None => VType::Unknown,
            },
        }
    }

    /// リンク列の src 側が単一値か（v1 の record_ref・`one_to_one` / `many_to_one`）。
    pub(crate) fn single_link(&self, table: Uuid, link: &str) -> bool {
        self.field(table, link)
            .is_some_and(|f| f.link.as_ref().is_none_or(|l| l.cardinality.src_single()))
    }

    fn rollup_type(
        &self,
        table: Uuid,
        link: &str,
        target: Option<&str>,
        op: RollupOp,
        depth: usize,
    ) -> VType {
        match op {
            RollupOp::Count | RollupOp::Sum | RollupOp::Avg => VType::Number,
            RollupOp::Concat => VType::Text,
            RollupOp::Min | RollupOp::Max => match (self.ref_of(table, link), target) {
                (Some(rt), Some(t)) => self.infer_at(rt, t, depth + 1),
                _ => VType::Unknown,
            },
        }
    }

    fn infer_expr(&self, table: Uuid, e: &FormulaExpr, depth: usize) -> VType {
        use FormulaExpr as E;
        match e {
            E::Field(k) => self.infer_at(table, k, depth + 1),
            E::Lit(v) => match v {
                serde_json::Value::Number(_) => VType::Number,
                serde_json::Value::String(_) => VType::Text,
                serde_json::Value::Bool(_) => VType::Bool,
                _ => VType::Unknown,
            },
            E::Add(..) | E::Sub(..) | E::Mul(..) | E::Div(..) | E::Round(..) | E::DateDiff(..) => {
                VType::Number
            }
            E::Concat(_) | E::Upper(_) | E::Lower(_) => VType::Text,
            E::DateAdd(d, _) => match self.infer_expr(table, d, depth + 1) {
                VType::DateTime => VType::DateTime,
                _ => VType::Date,
            },
            E::Now => VType::DateTime,
            E::And(_) | E::Or(_) | E::Not(_) | E::Cmp(..) => VType::Bool,
            E::If(_, then, other) => {
                let then_ty = self.infer_expr(table, then, depth + 1);
                if then_ty == self.infer_expr(table, other, depth + 1) {
                    then_ty
                } else {
                    VType::Unknown
                }
            }
            E::Switch { cases, default, .. } => {
                let mut ty: Option<VType> = None;
                for t in cases.iter().map(|c| &c.then).chain(default.as_deref()) {
                    let x = self.infer_expr(table, t, depth + 1);
                    ty = match ty {
                        None => Some(x),
                        Some(y) if y == x => Some(y),
                        Some(_) => Some(VType::Unknown),
                    };
                }
                ty.unwrap_or(VType::Unknown)
            }
            E::Rollup { link, target, op } => {
                self.rollup_type(table, link, target.as_deref(), *op, depth)
            }
        }
    }

    /// ロールアップの演算と対象の型の組を検証する（保存時・422）。
    pub(crate) fn check_rollup(
        &self,
        table: Uuid,
        name: &str,
        link: &str,
        target: Option<&str>,
        op: RollupOp,
    ) -> Result<(), DataError> {
        let Some(rt) = self.ref_of(table, link) else {
            return Err(DataError::Invalid(format!(
                "'{name}': ロールアップのリンク列 '{link}' は record_ref である必要があります"
            )));
        };
        let Some(rtab) = self.tables.get(&rt) else {
            return Err(DataError::Invalid(format!(
                "'{name}': ロールアップの参照先テーブルが見つかりません"
            )));
        };
        let Some(t) = target else {
            if op == RollupOp::Count {
                return Ok(());
            }
            return Err(DataError::Invalid(format!(
                "'{name}': {op:?} には target が必要です"
            )));
        };
        if rtab.schema.field(t).is_none() {
            return Err(DataError::Invalid(format!(
                "'{name}': ロールアップの対象 '{t}' が参照先にありません"
            )));
        }
        let ty = self.infer(rt, t);
        let ok = match op {
            RollupOp::Count | RollupOp::Concat => true,
            RollupOp::Sum | RollupOp::Avg => ty == VType::Number,
            RollupOp::Min | RollupOp::Max => matches!(
                ty,
                VType::Number | VType::Text | VType::Date | VType::DateTime
            ),
        };
        if !ok {
            return Err(DataError::Invalid(format!(
                "'{name}': {op:?} の対象 '{t}' の型が合いません"
            )));
        }
        Ok(())
    }

    /// 派生フィールドの材料化可否（§7.4）。`contained` は「参照先テーブルの viewer 集合が
    /// 参照元を包含すると証明できた参照先」（OpenFGA の直接タプルの比較・呼び出し側が渡す）。
    pub(crate) fn materialization(
        &self,
        table: Uuid,
        key: &str,
        contained: &HashSet<Uuid>,
    ) -> Materialization {
        self.mat_at(table, key, contained, 0)
    }

    fn mat_at(
        &self,
        table: Uuid,
        key: &str,
        contained: &HashSet<Uuid>,
        depth: usize,
    ) -> Materialization {
        if depth > MAX_DERIVED_CHAIN {
            return Materialization::PerViewer;
        }
        let Some(t) = self.tables.get(&table) else {
            return Materialization::PerViewer;
        };
        let Some(f) = t.schema.field(key) else {
            return Materialization::PerViewer;
        };
        // 自テーブルのマスク列（③）を材料にすると、マスクされた閲覧者に値が漏れる。
        let masked_local = |k: &str| t.schema.field_policy.iter().any(|p| p.field == k);
        if !f.field_type.is_derived() {
            return if masked_local(key) {
                Materialization::PerViewer
            } else {
                Materialization::Invariant
            };
        }
        // 自テーブルの依存（マスクされた派生列を材料にしても漏れるので、派生列も含めて見る）。
        let local = |k: &str| {
            if masked_local(k) {
                Materialization::PerViewer
            } else {
                self.mat_at(table, k, contained, depth + 1)
            }
        };
        match f.field_type {
            FieldType::Lookup => f.lookup.as_ref().map_or(Materialization::PerViewer, |d| {
                local(&d.via_field).join(self.cross(
                    table,
                    &d.via_field,
                    Some(&d.target_field),
                    contained,
                ))
            }),
            FieldType::Rollup => f.rollup.as_ref().map_or(Materialization::PerViewer, |d| {
                local(&d.link).join(self.cross(table, &d.link, d.target.as_deref(), contained))
            }),
            FieldType::Computed => f.computed.as_ref().map_or(Materialization::PerViewer, |d| {
                d.fields
                    .iter()
                    .fold(Materialization::Invariant, |m, k| m.join(local(k)))
            }),
            FieldType::Formula => f.formula.as_ref().map_or(Materialization::PerViewer, |e| {
                let mut m = if e.uses_now() {
                    Materialization::Volatile
                } else {
                    Materialization::Invariant
                };
                e.walk(&mut |n| match n {
                    FormulaExpr::Field(k) => m = m.join(local(k)),
                    FormulaExpr::Rollup { link, target, .. } => {
                        m = m.join(local(link)).join(self.cross(
                            table,
                            link,
                            target.as_deref(),
                            contained,
                        ));
                    }
                    _ => {}
                });
                m
            }),
            _ => Materialization::PerViewer,
        }
    }

    /// 参照先テーブルを跨ぐ依存の 4 階層判定（1 つでも閲覧者依存があれば `per_viewer`）。
    fn cross(
        &self,
        table: Uuid,
        link: &str,
        target: Option<&str>,
        contained: &HashSet<Uuid>,
    ) -> Materialization {
        let pv = Materialization::PerViewer;
        let (Some(src), Some(lf)) = (self.tables.get(&table), self.field(table, link)) else {
            return pv;
        };
        // 対称列（逆方向）は参照元の行集合が閲覧者ごとに変わる。
        if lf.is_reverse_link() {
            return pv;
        }
        let Some(rt) = self.ref_of(table, link) else {
            return pv;
        };
        let Some(r) = self.tables.get(&rt) else {
            return pv;
        };
        // ① 同一ミニアプリの所有テーブルで、viewer 集合の包含を証明できたものだけ。
        if src.app_id.is_none() || src.app_id != r.app_id || !contained.contains(&rt) {
            return pv;
        }
        // ② 行述語 ④ 個別共有。
        if r.schema.row_policy.is_some() || r.has_record_shares {
            return pv;
        }
        if let Some(tk) = target {
            let Some(tf) = r.schema.field(tk) else {
                return pv;
            };
            // ③ マスク列。多段参照（参照先の派生列）は安全側へ倒す。
            if tf.field_type.is_derived() || r.schema.field_policy.iter().any(|p| p.field == tk) {
                return pv;
            }
        }
        Materialization::Invariant
    }

    /// 材料化した列が辿る (参照先テーブル, リンク列の参照名)。依存登録（参照先の変更 Tx から引く）に使う。
    pub(crate) fn materialized_refs(&self, table: Uuid, key: &str) -> Vec<(Uuid, String)> {
        let mut out: Vec<(Uuid, String)> = Vec::new();
        let mut push = |link: &str| {
            if let Some(rt) = self.ref_of(table, link) {
                if !out.iter().any(|(t, l)| *t == rt && l == link) {
                    out.push((rt, link.to_string()));
                }
            }
        };
        let mut stack = vec![key.to_string()];
        let mut seen = HashSet::new();
        while let Some(k) = stack.pop() {
            if !seen.insert(k.clone()) {
                continue;
            }
            let Some(f) = self.field(table, &k) else {
                continue;
            };
            match f.field_type {
                FieldType::Lookup => {
                    if let Some(d) = &f.lookup {
                        push(&d.via_field);
                    }
                }
                FieldType::Rollup => {
                    if let Some(d) = &f.rollup {
                        push(&d.link);
                    }
                }
                FieldType::Computed => {
                    if let Some(d) = &f.computed {
                        stack.extend(d.fields.iter().cloned());
                    }
                }
                FieldType::Formula => {
                    if let Some(e) = &f.formula {
                        e.walk(&mut |n| match n {
                            FormulaExpr::Field(k) => stack.push(k.clone()),
                            FormulaExpr::Rollup { link, .. } => push(link),
                            _ => {}
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// 派生フィールドを評価順（依存が先）に並べる（自テーブル内の数式どうしの依存だけを見る）。
pub(crate) fn eval_order(schema: &TableSchema) -> Vec<&FieldDef> {
    let derived: Vec<&FieldDef> = schema
        .fields
        .iter()
        .filter(|f| f.field_type.is_derived())
        .collect();
    let mut done: HashSet<&str> = HashSet::new();
    let mut out: Vec<&FieldDef> = Vec::with_capacity(derived.len());
    // 循環は保存時に拒否済み。万一残っていても有限回で抜ける（残りは末尾に並べる）。
    for _ in 0..=derived.len() {
        for f in &derived {
            if done.contains(f.key.as_str()) {
                continue;
            }
            let ready = local_derived_deps(schema, f)
                .iter()
                .all(|k| done.contains(k.as_str()));
            if ready {
                done.insert(f.key.as_str());
                out.push(f);
            }
        }
    }
    for f in derived {
        if !done.contains(f.key.as_str()) {
            out.push(f);
        }
    }
    out
}

/// 自テーブルの派生フィールドへの依存（評価順の決定用）。
fn local_derived_deps(schema: &TableSchema, f: &FieldDef) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |k: &str| {
        if schema
            .field(k)
            .is_some_and(|d| d.field_type.is_derived() && d.key != f.key)
        {
            out.push(k.to_string());
        }
    };
    if let Some(e) = &f.formula {
        for k in e.local_refs() {
            add(k);
        }
    }
    if let Some(c) = &f.computed {
        for k in &c.fields {
            add(k);
        }
    }
    out
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
