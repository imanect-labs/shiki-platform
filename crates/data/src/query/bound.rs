//! 束縛・検証（query-optimizer.md §2 ①・data-platform §5.2 の索引到達可能性）。
//!
//! 外部の参照名（key）を内部 ID（`field_id`）へ解決し、型ごとに値を正規化した式 [`Expr`] を作る。
//! ここを通らない条件はコンパイラに届かない（未宣言・未索引・型不整合・マスク列は実行前に拒否）。
//!
//! - filter / sort / group_by / aggregate の対象は `indexed || unique` 宣言済みのみ（422）
//! - マスク列は filter / sort / group_by / aggregate / search のいずれにも出せない（**403**・PIT-19）
//! - `contains` / `ends_with` は `searchable` 宣言済みの text 列のみ（trigram GIN を要する）
//! - `matches`（正規表現）は採用しない（422）
//! - システム擬似列 `_owner` / `_created_at` / `_updated_at` は常に索引済みで、駆動になれる

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::ir::{
    CondOp, Condition, LinkCond, Metric, DEFAULT_LIMIT, MAX_CONDITION_DEPTH, MAX_CONDITION_LEAVES,
    MAX_IN_VALUES,
};
use super::limits::charge;
use super::scopes::ScopeTables;
use crate::derived::types::ResultType;
use crate::index::{IndexTable, SortPrecision};
use crate::model::{
    FieldId, FieldType, TableSchema, SYSTEM_FIELD_CREATED_AT, SYSTEM_FIELD_OWNER,
    SYSTEM_FIELD_UPDATED_AT,
};
use crate::numeric::Dec;
use crate::query::planner::lower::SAFE_BOUND_BYTES;
use crate::DataError;

/// 擬似列の参照名（ユーザー定義の key は `[a-z]` 始まりなので衝突しない）。
pub(crate) const PSEUDO_OWNER: &str = "_owner";
pub(crate) const PSEUDO_CREATED_AT: &str = "_created_at";
pub(crate) const PSEUDO_UPDATED_AT: &str = "_updated_at";

/// 索引上の値の持ち方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    /// `data_index_text` に単値（擬似列を含む）。
    Text,
    /// `data_index_text` に要素ごと（multi_select）。
    Multi,
    /// `data_index_num`。
    Num,
    /// `data_link`（record_ref）。
    Link,
}

/// 束縛済みフィールド。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BField {
    pub id: FieldId,
    /// 参照名（擬似列は `_owner` 等）。
    pub key: String,
    pub ty: Option<FieldType>,
    pub kind: FieldKind,
    pub searchable: bool,
    /// record_ref の参照先テーブル。
    pub ref_table: Option<Uuid>,
    /// 材料化した派生フィールド（索引行は再計算が書く・再計算待ちの間はクエリ対象外）。
    pub derived: bool,
}

impl BField {
    pub(crate) fn is_pseudo(&self) -> bool {
        self.ty.is_none()
    }

    /// 値索引の表（フィルタ・ソートの到達先）。
    pub(crate) fn value_table(&self) -> IndexTable {
        match self.kind {
            FieldKind::Text | FieldKind::Multi => IndexTable::Text,
            FieldKind::Num => IndexTable::Num,
            FieldKind::Link => IndexTable::Link,
        }
    }

    pub(crate) fn multi(&self) -> bool {
        matches!(self.kind, FieldKind::Multi | FieldKind::Link)
    }
}

/// 比較値（型で正規化済み）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Val {
    Text(String),
    Num(Dec),
    /// record_ref の参照先（外部 ID。`record_int` への引き直しは実行時・外へ出さない）。
    Ref(Uuid),
}

impl Val {
    pub(crate) fn as_text(&self) -> Option<&str> {
        match self {
            Val::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// 範囲の端（値・含むか）。
pub(crate) type Bound = Option<(Val, bool)>;

/// 葉の述語（索引 1 本で評価できる形）。否定は [`Expr::Not`] で表す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Leaf {
    Eq(Val),
    In(Vec<Val>),
    Range { lo: Bound, hi: Bound },
    StartsWith(String),
    Contains(String),
    EndsWith(String),
    Exists,
}

/// 原子式（フィールド × 葉）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Atom {
    pub field: FieldId,
    pub leaf: Leaf,
}

/// 束縛済みの条件式（2 値論理）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Expr {
    True,
    False,
    Atom(Atom),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    /// リンクの半結合（参照先に「閲覧者に見えて条件を満たす行」が 1 件以上ある・data-platform §16.2）。
    Link(Box<LinkExpr>),
}

/// リンクの半結合（`EXISTS`）。参照先の条件は**参照先のフィールド空間**で束縛し、参照先の行述語
/// （`Guard(P)`）は降下で必ず合成する（PIT-20: lookup と同じく閲覧者本人の権限で辿る）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkExpr {
    /// この式が属する表のリンク列（順方向のリンク列か対称列）。
    pub field: FieldId,
    /// 参照先のスコープ（[`BoundQuery::scopes`] の添字）。
    pub scope: usize,
    /// 参照先の条件（参照先のフィールド空間・正規化済み）。
    pub filter: Expr,
}

/// 参照先テーブルのスコープ（束縛済み・JOIN / 半結合の参照先 1 つ）。
#[derive(Debug, Clone)]
pub(crate) struct BoundScope {
    pub table: Uuid,
    pub schema_version: i64,
    /// 参照先のフィールド（束縛器が登録したもの。`filter` 等は使わない）。
    pub q: BoundQuery,
}

impl Expr {
    pub(crate) fn atom(field: FieldId, leaf: Leaf) -> Expr {
        Expr::Atom(Atom { field, leaf })
    }

    /// 原子式を順に訪れる（リンクの半結合の中＝参照先の原子式には降りない）。
    pub(crate) fn atoms<'a>(&'a self, out: &mut Vec<&'a Atom>) {
        match self {
            Expr::True | Expr::False | Expr::Link(_) => {}
            Expr::Atom(a) => out.push(a),
            Expr::Not(e) => e.atoms(out),
            Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| e.atoms(out)),
        }
    }

    /// リンクの半結合を順に訪れる（入れ子の参照先の中には降りない）。
    pub(crate) fn links<'a>(&'a self, out: &mut Vec<&'a LinkExpr>) {
        match self {
            Expr::True | Expr::False | Expr::Atom(_) => {}
            Expr::Link(l) => out.push(l),
            Expr::Not(e) => e.links(out),
            Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| e.links(out)),
        }
    }
}

/// 束縛済みソートキー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BSort {
    pub field: FieldId,
    pub desc: bool,
}

/// 集計・グループ化・射影の列（`src` = 0 は FROM の表・`i + 1` は `joins[i]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ColRef {
    pub src: usize,
    pub field: FieldId,
}

impl ColRef {
    pub(crate) fn base(field: FieldId) -> Self {
        ColRef { src: 0, field }
    }
}

/// 束縛済みの結合（宣言済みリンク列経由の INNER JOIN・カーディナリティは `LinkDef` から）。
#[derive(Debug, Clone)]
pub(crate) struct BoundJoin {
    pub alias: String,
    /// 結合元（0 は FROM の表・`i + 1` は `joins[i]`）。
    pub from: usize,
    /// 結合元のリンク列（順方向のリンク列か対称列）。
    pub field: FieldId,
    /// 結合先のスコープ（[`BoundQuery::scopes`] の添字）。
    pub scope: usize,
    /// 結合先の条件（結合先のフィールド空間・正規化済み）。
    pub filter: Expr,
    /// 結合元 1 行から辿れる結合先が高々 1 件（`LinkDef` の src 側が単値）。
    pub at_most_one: bool,
    /// 射影する結合先の列の参照名（擬似列 `_id` 等を含む）。
    pub select: Vec<String>,
}

/// 集計後の条件（指標の添字・比較・値）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BHaving {
    All(Vec<BHaving>),
    Any(Vec<BHaving>),
    Not(Box<BHaving>),
    Cmp {
        metric: usize,
        op: CondOp,
        value: f64,
    },
}

/// 集計結果の並べ替えのキー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupKey {
    /// `group_by` の添字。
    Group(usize),
    /// `metrics` の添字。
    Metric(usize),
}

/// 束縛済みクエリ。
#[derive(Debug, Clone)]
pub(crate) struct BoundQuery {
    pub fields: BTreeMap<FieldId, BField>,
    pub filter: Expr,
    pub sort: Vec<BSort>,
    pub group_by: Vec<ColRef>,
    pub metrics: Vec<(Metric, Option<ColRef>)>,
    /// 出力に出さない末尾の指標の数（HAVING・並べ替えのためだけに足したもの）。
    pub hidden_metrics: usize,
    pub having: Option<BHaving>,
    pub group_sort: Vec<(GroupKey, bool)>,
    pub group_limit: Option<u32>,
    /// 残した結合（半結合へ書き換えられなかったもの）。
    pub joins: Vec<BoundJoin>,
    pub select: Option<HashSet<String>>,
    pub limit: u32,
    pub with_count: bool,
    pub sort_precision: SortPrecision,
    /// 参照先テーブルのスコープ（リンクの半結合・JOIN。入れ子も平坦に並べる）。
    pub scopes: Vec<BoundScope>,
}

impl BoundQuery {
    pub(crate) fn field(&self, id: FieldId) -> &BField {
        // 束縛時に登録した ID だけが式・ソートに現れる（不在は束縛の不具合）。
        self.fields.get(&id).unwrap_or(&FALLBACK_FIELD)
    }

    /// フィールドだけを持つ束縛（参照先スコープ用）。
    pub(crate) fn fields_only(fields: BTreeMap<FieldId, BField>) -> Self {
        BoundQuery {
            fields,
            filter: Expr::True,
            sort: Vec::new(),
            group_by: Vec::new(),
            metrics: Vec::new(),
            hidden_metrics: 0,
            having: None,
            group_sort: Vec::new(),
            group_limit: None,
            joins: Vec::new(),
            select: None,
            limit: DEFAULT_LIMIT,
            with_count: false,
            sort_precision: SortPrecision::Exact,
            scopes: Vec::new(),
        }
    }

    /// 列（FROM の表か結合先）の束縛済みフィールド。
    pub(crate) fn col_field(&self, c: ColRef) -> &BField {
        if c.src == 0 {
            return self.field(c.field);
        }
        self.joins
            .get(c.src - 1)
            .and_then(|j| self.scopes.get(j.scope))
            .map_or(&FALLBACK_FIELD, |s| s.q.field(c.field))
    }

    /// 列の出力名（FROM の表は参照名・結合先は `別名.参照名`）。
    pub(crate) fn col_key(&self, c: ColRef) -> String {
        let key = &self.col_field(c).key;
        match self.joins.get(c.src.wrapping_sub(1)) {
            Some(j) if c.src > 0 => format!("{}.{key}", j.alias),
            _ => key.clone(),
        }
    }

    pub(crate) fn is_aggregate(&self) -> bool {
        !self.metrics.is_empty()
    }
}

static FALLBACK_FIELD: BField = BField {
    id: SYSTEM_FIELD_OWNER,
    key: String::new(),
    ty: None,
    kind: FieldKind::Text,
    searchable: false,
    ref_table: None,
    derived: false,
};

/// 擬似列の束縛。
pub(crate) fn pseudo(key: &str) -> Option<BField> {
    let id = match key {
        PSEUDO_OWNER => SYSTEM_FIELD_OWNER,
        PSEUDO_CREATED_AT => SYSTEM_FIELD_CREATED_AT,
        PSEUDO_UPDATED_AT => SYSTEM_FIELD_UPDATED_AT,
        _ => return None,
    };
    Some(BField {
        id,
        key: key.to_string(),
        ty: None,
        kind: FieldKind::Text,
        searchable: false,
        ref_table: None,
        derived: false,
    })
}

/// 擬似列 `updated_at` の束縛（既定ソート）。
pub(crate) fn pseudo_updated_at() -> BField {
    pseudo(PSEUDO_UPDATED_AT).unwrap_or_else(|| FALLBACK_FIELD.clone())
}

/// リンクの半結合・結合の束縛の文脈（参照先テーブルと、作ったスコープ）。
pub(crate) struct LinkCtx<'a> {
    pub tables: &'a ScopeTables,
    pub scopes: Vec<BoundScope>,
    /// 半結合の入れ子・結合の段数の上限（`MAX_JOIN_DEPTH`）。
    pub max_depth: usize,
}

/// 束縛器（1 クエリ分）。
pub(crate) struct Binder<'a> {
    schema: &'a TableSchema,
    masked: &'a HashSet<String>,
    fields: BTreeMap<FieldId, BField>,
    leaves: usize,
    /// リンクの半結合を束縛する文脈（無ければリンクの条件は 422）。
    ctx: Option<&'a RefCell<LinkCtx<'a>>>,
    /// この束縛器の表がリンクを何段辿った先か。
    depth: usize,
}

impl<'a> Binder<'a> {
    pub(crate) fn new(schema: &'a TableSchema, masked: &'a HashSet<String>) -> Self {
        Binder {
            schema,
            masked,
            fields: BTreeMap::new(),
            leaves: 0,
            ctx: None,
            depth: 0,
        }
    }

    /// リンクの半結合を束縛できる束縛器（`depth` は辿った段数）。
    pub(crate) fn scoped(
        schema: &'a TableSchema,
        masked: &'a HashSet<String>,
        ctx: &'a RefCell<LinkCtx<'a>>,
        depth: usize,
    ) -> Self {
        Binder {
            ctx: Some(ctx),
            depth,
            ..Binder::new(schema, masked)
        }
    }

    /// 束縛器の表のリンク列とその参照先（マスク 403 → 未宣言 422 → リンク列でない 422 → 参照先が
    /// 読めない 403 の順）。
    pub(crate) fn link_target(
        &self,
        key: &str,
    ) -> Result<(&'a crate::model::FieldDef, &'a super::scopes::ScopeTable), DataError> {
        if self.masked.contains(key) {
            return Err(DataError::Forbidden);
        }
        charge(self.schema.fields.len());
        let f = self.schema.field(key).ok_or_else(|| {
            DataError::Invalid(format!("フィールド '{key}' はスキーマに存在しません"))
        })?;
        let (FieldType::RecordRef, Some(rt)) = (f.field_type, f.ref_table) else {
            return Err(DataError::Invalid(format!(
                "フィールド '{key}' はリンク列（record_ref）ではありません"
            )));
        };
        let ctx = self.ctx.ok_or_else(|| {
            DataError::Invalid("この読み方ではリンクの条件・結合を使えません".into())
        })?;
        let tables = ctx.borrow().tables;
        let st = tables.get(rt).ok_or(DataError::Forbidden)?;
        Ok((f, st))
    }

    /// リンクの半結合を束縛する（参照先の条件は参照先のフィールド空間で束縛・正規化する）。
    fn link_cond(&mut self, lc: &LinkCond) -> Result<Expr, DataError> {
        let (f, st) = self.link_target(&lc.field)?;
        let Some(ctx) = self.ctx else {
            return Err(DataError::Invalid(
                "この読み方ではリンクの条件を使えません".into(),
            ));
        };
        let max = ctx.borrow().max_depth;
        if self.depth + 1 > max {
            return Err(DataError::Rejected(Box::new(
                crate::query::reject::QueryRejection::limit("max_join_depth", max as u64),
            )));
        }
        let mut child = Binder::scoped(&st.table.schema, &st.masked, ctx, self.depth + 1);
        let filter = match &lc.filter {
            Some(c) => child.condition(c, 0)?,
            None => Expr::True,
        };
        let filter = crate::query::planner::normalize::normalize(
            filter,
            &child.fields,
            crate::query::planner::normalize::Rules::ALL,
        );
        let scope = {
            let mut c = ctx.borrow_mut();
            c.scopes.push(BoundScope {
                table: st.table.id,
                schema_version: st.table.schema_version,
                q: BoundQuery::fields_only(child.fields),
            });
            c.scopes.len() - 1
        };
        Ok(Expr::Link(Box::new(LinkExpr {
            field: f.id,
            scope,
            filter,
        })))
    }

    /// フィルタ・ソート・集計で使う列を解決する（マスク 403 → 未宣言 422 → 未索引 422 の順）。
    pub(crate) fn queryable(&mut self, key: &str) -> Result<BField, DataError> {
        if self.masked.contains(key) {
            return Err(DataError::Forbidden);
        }
        if let Some(p) = pseudo(key) {
            self.fields.insert(p.id, p.clone());
            return Ok(p);
        }
        charge(self.schema.fields.len());
        let f = self.schema.field(key).ok_or_else(|| {
            DataError::Invalid(format!("フィールド '{key}' はスキーマに存在しません"))
        })?;
        if f.field_type.is_derived() {
            // 材料化した（invariant かつ indexed の）列だけ。閲覧者ごと・時刻依存の値で並べたり絞ったり
            // すると見えない行の値が順序から漏れる（PIT-71・PIT-19 と同型）。
            if !f.is_materialized() {
                return Err(DataError::Invalid(format!(
                    "派生フィールド '{key}' はフィルタ/ソート/集計に使えません（閲覧者ごと・時刻依存の値、または indexed でない列）"
                )));
            }
            let (ty, kind) = match f.result_type {
                Some(ResultType::Number) => (FieldType::Number, FieldKind::Num),
                Some(ResultType::Date) => (FieldType::Date, FieldKind::Text),
                Some(ResultType::DateTime) => (FieldType::DateTime, FieldKind::Text),
                _ => (FieldType::Text, FieldKind::Text),
            };
            let b = BField {
                id: f.id,
                key: f.key.clone(),
                ty: Some(ty),
                kind,
                searchable: false,
                ref_table: None,
                derived: true,
            };
            self.fields.insert(b.id, b.clone());
            return Ok(b);
        }
        if f.is_reverse_link() {
            return Err(DataError::Invalid(format!(
                "対称列 '{key}' はフィルタ/ソートに使えません（リンク元の列で絞り込んでください）"
            )));
        }
        if !(f.indexed || f.unique) {
            return Err(DataError::Invalid(format!(
                "フィールド '{key}' はフィルタ/ソート対象として宣言されていません（indexed）"
            )));
        }
        let kind = match f.field_type {
            FieldType::Number => FieldKind::Num,
            FieldType::MultiSelect => FieldKind::Multi,
            FieldType::RecordRef => FieldKind::Link,
            _ => FieldKind::Text,
        };
        let b = BField {
            id: f.id,
            key: f.key.clone(),
            ty: Some(f.field_type),
            kind,
            searchable: f.searchable && f.field_type == FieldType::Text,
            ref_table: f.ref_table,
            derived: false,
        };
        self.fields.insert(b.id, b.clone());
        Ok(b)
    }

    /// 条件木を束縛する（2 値論理の式へ）。
    pub(crate) fn condition(&mut self, c: &Condition, depth: usize) -> Result<Expr, DataError> {
        charge(1);
        if depth > MAX_CONDITION_DEPTH {
            return Err(DataError::Invalid(format!(
                "条件のネストが深すぎます（最大 {MAX_CONDITION_DEPTH}）"
            )));
        }
        Ok(match c {
            Condition::All(v) => Expr::And(
                v.iter()
                    .map(|c| self.condition(c, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            Condition::Any(v) => Expr::Or(
                v.iter()
                    .map(|c| self.condition(c, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            Condition::Not(inner) => Expr::Not(Box::new(self.condition(inner, depth + 1)?)),
            Condition::Link(lc) => self.link_cond(lc)?,
            Condition::Cmp(cmp) => {
                self.leaves += 1;
                if self.leaves > MAX_CONDITION_LEAVES {
                    return Err(DataError::Invalid(format!(
                        "条件が多すぎます（最大 {MAX_CONDITION_LEAVES}）"
                    )));
                }
                let f = self.queryable(&cmp.field)?;
                compare(&f, cmp.op, cmp.value.as_ref())?
            }
        })
    }

    /// 横断部分一致（`searchable` 列の `contains` の OR）。
    pub(crate) fn search(
        &mut self,
        text: &str,
        only: Option<&[String]>,
    ) -> Result<Expr, DataError> {
        let mut terms = Vec::new();
        // 明示した列は検証する（マスク列は 403）。省略時は読める searchable 列だけ（PIT-19）。
        let explicit = only.is_some();
        let keys: Vec<String> = only.map_or_else(
            || {
                self.schema
                    .fields
                    .iter()
                    .filter(|f| f.searchable && f.field_type == FieldType::Text)
                    .filter(|f| !self.masked.contains(&f.key))
                    .map(|f| f.key.clone())
                    .collect()
            },
            <[String]>::to_vec,
        );
        for k in keys {
            let f = self.queryable(&k)?;
            if explicit && !f.searchable {
                return Err(DataError::Invalid(format!(
                    "フィールド '{k}' は部分一致検索の対象として宣言されていません（searchable）"
                )));
            }
            terms.push(Expr::atom(f.id, Leaf::Contains(text.to_string())));
        }
        if terms.is_empty() {
            return Err(DataError::Invalid(
                "部分一致検索の対象列（searchable）がありません".into(),
            ));
        }
        Ok(Expr::Or(terms))
    }

    pub(crate) fn into_fields(self) -> BTreeMap<FieldId, BField> {
        self.fields
    }

    pub(crate) fn fields(&self) -> &BTreeMap<FieldId, BField> {
        &self.fields
    }

    /// 登録済みのフィールドを足す（既定ソートの擬似列など）。
    pub(crate) fn register(&mut self, f: BField) {
        self.fields.insert(f.id, f);
    }

    pub(crate) fn schema(&self) -> &'a TableSchema {
        self.schema
    }

    pub(crate) fn masked(&self) -> &'a HashSet<String> {
        self.masked
    }
}

#[cfg(test)]
pub(crate) use super::bound_query::bind_query;
pub(crate) use super::bound_query::{bind_query_scoped, BindOpts};

/// 比較 1 つを束縛する（§3.1 の表: `neq` = `not eq`・`is_null` = `not exists`・比較の否定は呼び出し側）。
fn compare(f: &BField, op: CondOp, value: Option<&Value>) -> Result<Expr, DataError> {
    let e = compare_inner(f, op, value)?;
    if f.derived && f.kind == FieldKind::Text {
        check_derived_text_operands(f, &e)?;
    }
    Ok(e)
}

/// 材料化した派生列（text）の比較値の長さ（data-platform §3.3 の切り詰めキーの契約）。
///
/// 索引には先頭 `MAX_INDEX_KEY_BYTES` だけを持ち、切り詰めた行の等値・前方一致・長い境界の大小比較は
/// **本体の完全値で再確認**する契約だが、派生列は本体に値を持たない（再確認できない）。比較値を
/// [`SAFE_BOUND_BYTES`] 以下に限れば、どの比較もプレフィクスだけで厳密に決まる（切り詰め行は
/// 比較値より長いので等値にならず、大小・前方一致は比較値の長さの内側で決まる）。超える比較値は
/// 偽陰性（と、その否定による偽陽性）を返す代わりに 422 にする。判定は比較値だけで決まり、
/// データに依存しない（行の有無・値の長さのオラクルにならない）。
fn check_derived_text_operands(f: &BField, e: &Expr) -> Result<(), DataError> {
    let too_long = |s: &str| s.len() > SAFE_BOUND_BYTES;
    let long = match e {
        Expr::Not(x) => return check_derived_text_operands(f, x),
        Expr::Atom(a) => match &a.leaf {
            Leaf::Eq(Val::Text(s)) | Leaf::StartsWith(s) => too_long(s),
            Leaf::In(vs) => vs.iter().any(|v| v.as_text().is_some_and(too_long)),
            Leaf::Range { lo, hi } => [lo, hi]
                .into_iter()
                .flatten()
                .any(|(v, _)| v.as_text().is_some_and(too_long)),
            _ => false,
        },
        _ => false,
    };
    if long {
        return Err(DataError::Invalid(format!(
            "派生フィールド '{}' の比較値が長すぎます（最大 {SAFE_BOUND_BYTES} バイト。派生値は索引に先頭部分だけを持つため、長い値では厳密に比較できません）",
            f.key
        )));
    }
    Ok(())
}

fn compare_inner(f: &BField, op: CondOp, value: Option<&Value>) -> Result<Expr, DataError> {
    let key = &f.key;
    let need = || DataError::Invalid(format!("'{key}' の比較には value が必要です"));
    let leaf = |l: Leaf| Ok(Expr::atom(f.id, l));
    match op {
        CondOp::Matches => Err(DataError::Invalid(
            "matches（正規表現）は構造化データのクエリでは使えません".into(),
        )),
        CondOp::Exists => leaf(Leaf::Exists),
        CondOp::IsNull => Ok(Expr::Not(Box::new(Expr::atom(f.id, Leaf::Exists)))),
        CondOp::Eq => leaf(Leaf::Eq(scalar(f, value.ok_or_else(need)?)?)),
        CondOp::Neq => Ok(Expr::Not(Box::new(Expr::atom(
            f.id,
            Leaf::Eq(scalar(f, value.ok_or_else(need)?)?),
        )))),
        CondOp::In => {
            let arr = value.and_then(Value::as_array).ok_or_else(|| {
                DataError::Invalid(format!("'{key}' の in は配列で指定してください"))
            })?;
            if arr.len() > MAX_IN_VALUES {
                return Err(DataError::Invalid(format!(
                    "'{key}' の in は最大 {MAX_IN_VALUES} 件です"
                )));
            }
            let vals = arr
                .iter()
                .map(|v| scalar(f, v))
                .collect::<Result<Vec<_>, _>>()?;
            leaf(Leaf::In(vals))
        }
        CondOp::Lt | CondOp::Lte | CondOp::Gt | CondOp::Gte => {
            if f.multi() {
                return Err(DataError::Invalid(format!(
                    "'{key}'（多値・リンク）に大小比較は使えません"
                )));
            }
            let v = scalar(f, value.ok_or_else(need)?)?;
            let (lo, hi) = match op {
                CondOp::Lt => (None, Some((v, false))),
                CondOp::Lte => (None, Some((v, true))),
                CondOp::Gt => (Some((v, false)), None),
                _ => (Some((v, true)), None),
            };
            leaf(Leaf::Range { lo, hi })
        }
        CondOp::StartsWith | CondOp::Contains | CondOp::EndsWith => {
            if !matches!(f.kind, FieldKind::Text | FieldKind::Multi) {
                return Err(DataError::Invalid(format!(
                    "'{key}' は文字列の部分一致に使えません"
                )));
            }
            let s = value.and_then(Value::as_str).ok_or_else(|| {
                DataError::Invalid(format!("'{key}' の値は文字列で指定してください"))
            })?;
            if s.chars().count() > 1_000 {
                return Err(DataError::Invalid(format!("'{key}' の値が長すぎます")));
            }
            match op {
                CondOp::StartsWith => leaf(Leaf::StartsWith(s.to_string())),
                _ if !f.searchable => Err(DataError::Invalid(format!(
                    "'{key}' は部分一致検索の対象として宣言されていません（searchable）"
                ))),
                CondOp::Contains => leaf(Leaf::Contains(s.to_string())),
                _ => leaf(Leaf::EndsWith(s.to_string())),
            }
        }
    }
}

/// 値 1 つを列の型へ正規化する（書込検証と同じ正準形・不一致は 422）。
fn scalar(f: &BField, v: &Value) -> Result<Val, DataError> {
    charge(1);
    let key = &f.key;
    match f.kind {
        FieldKind::Num => {
            let d = match v {
                Value::Number(n) => Dec::parse(&n.to_string()),
                _ => None,
            };
            d.map(Val::Num).ok_or_else(|| {
                DataError::Invalid(format!("フィルタ '{key}' は数値で指定してください"))
            })
        }
        FieldKind::Link => v
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .map(Val::Ref)
            .ok_or_else(|| {
                DataError::Invalid(format!("フィルタ '{key}' は UUID 文字列で指定してください"))
            }),
        FieldKind::Text | FieldKind::Multi => {
            let s = v.as_str().ok_or_else(|| {
                DataError::Invalid(format!("フィルタ '{key}' は文字列で指定してください"))
            })?;
            Ok(Val::Text(normalize_text(f.ty, s)))
        }
    }
}

/// date / date_time は書込時と同じ正準形へ寄せる（解釈できない形はそのまま＝一致しないだけ）。
fn normalize_text(ty: Option<FieldType>, s: &str) -> String {
    match ty {
        Some(FieldType::Date) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_or_else(|_| s.to_string(), |d| d.format("%Y-%m-%d").to_string()),
        Some(FieldType::DateTime) => DateTime::parse_from_rfc3339(s).map_or_else(
            |_| s.to_string(),
            |d| {
                d.with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::Micros, true)
            },
        ),
        _ => s.to_string(),
    }
}

#[cfg(test)]
#[path = "bound_tests.rs"]
mod tests;
