//! 「データを検索」（`data.query`）の列の保存時検証 V4（BUG-244）。
//!
//! テーブルをリテラルで選んだ `data.query` の絞り込み（`filter` の `all` / `any` / `not` の中の `cmp.field`）・並べ替え
//! （`sort`）・まとめる列（`group_by`）・集計の列（`aggregate`・`having`・`group_sort` の指標の `field`）に、実行で
//! 必ず断られる列があれば保存で止める。判定は実行時の束縛と同じ data の [`data::check_field_use`] で行う（規則を
//! 写さない）。主に、索引の無い列（`indexed` でも `unique` でもない列）で絞る・並べる・まとめる IR を止める
//! （ワークフローの経路では索引を自動で作らない・data-platform §10）。
//!
//! リンクの条件（`filter` の `link`・入れ子も）は、辿る列がリンクの列であることを [`data::check_link_field`] で確かめ、
//! 中の比較の列を、リンク先のテーブルの定義で同じ [`data::check_field_use`] に問う（GAP-129）。
//!
//! 材料は承認と同じ [`Catalog::tables`] の、保存者の見え方の定義（[`super::CatalogTable::schema`]・読めない列は
//! 入らない）。読めない列は無い列と同じ `ir.unknown_field`（同じ文言・data-platform §16.1）。テーブルを参照で決める
//! ノード・材料に無い（見えない）テーブル・定義の無い材料は照合しない。リンク先のテーブルも、API 層が
//! [`query_link_tables`]（照合と同じ歩き方）で保存者の見え方で引き足したものだけを照らす（見えないリンク先の中は
//! 照合しない。実行では、リンク先を開けなければ問い合わせ全体が断られる）。まとめる列・集計の列の結合先の列
//! （`別名.参照名`）は、つないだ先の定義が要るので照合しない（並べ替えと絞り込みは結合先の列を使えないので、
//! 実行と同じく止める）。件数（`count`）の指標の `field` は、実行で読まないので照合しない（実行時の束縛の
//! `bind_metric` と同じ）。比べる値（`$from` を書ける）は実行時に確かめる。
//!
//! 誤りの文言は、列に表示名があれば「表示名」（参照名）で言う（画面は表示名で列を見せ、AI は参照名で IR を直す）。

use std::collections::{BTreeMap, BTreeSet};

use data::{
    CondOp, Condition, DataError, DataQuery, FieldUse, HavingCond, InvalidCode, LinkCond, Metric,
    MetricSpec, TableSchema,
};
use serde_json::Value;
use uuid::Uuid;

use super::data_keys::column_name;
use super::{Catalog, CatalogTable, ValidationError};
use crate::ir::params::DataQueryParams;
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

/// 誤りのコード: 索引の無い列で絞り込み・並べ替え・まとめ・集計をする（部分一致は検索の索引の無い列）。
pub const FIELD_NOT_INDEXED: &str = "ir.field_not_indexed";
/// 誤りのコード: その位置に使えない種類の列（逆向きのリンク・複数の値・数でない列の集計など）。
pub const FIELD_NOT_USABLE: &str = "ir.field_not_usable";
/// 誤りのコード: 無い列か読めない列（承認・同じレコードを探す列と同じ）。
const UNKNOWN_FIELD: &str = "ir.unknown_field";

/// 列を引く表（誤りの文言で表を言い分ける）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Within {
    /// データを検索のテーブル（FROM の表）。
    Base,
    /// リンクの条件で辿った先のテーブル。
    Linked,
}

impl Within {
    /// 「…に無いか、読めない列です」の表の言い方。
    fn table(self) -> &'static str {
        match self {
            Within::Base => "このテーブル",
            Within::Linked => "リンク先のテーブル",
        }
    }

    /// 「…の「列の編集」で」の表の言い方。
    fn edit(self) -> &'static str {
        match self {
            Within::Base => "テーブル",
            Within::Linked => "リンク先のテーブル",
        }
    }
}

/// 照らす列（書いた位置の path・列を引く表の定義・列・位置）。
struct Use<'s> {
    path: String,
    schema: &'s TableSchema,
    within: Within,
    key: String,
    at: FieldUse,
}

/// V4: `data.query` の列（材料が無ければ照合しない）。
pub(super) fn v4(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    let Some(tables) = catalog.tables.as_ref() else {
        return;
    };
    for (node, schema, query) in literal_queries(ir, tables) {
        let mut walk = Walk::new(tables);
        walk.query(schema, &query);
        for (path, e) in walk.refused {
            let path = located(&node.params, &path);
            errors.push(e.at_node(&node.id).at_path(path));
        }
    }
}

/// データを検索（リテラルのテーブル・定義のある材料）のリンクの条件が辿る先のうち、`tables` に無いテーブル。API 層が
/// 保存者の見え方で引き足し、その中の列を照らす（GAP-129）。照合（[`v4`]）と同じ歩き方で決めるので、照らす列のある
/// リンク先だけが返る。引き足すと、その先のリンク先が分かる（入れ子のリンクは、深さの上限
/// 〔[`data::MAX_JOIN_DEPTH`]〕まで引き足しを繰り返す）。
#[must_use]
pub fn query_link_tables(
    ir: &WorkflowIr,
    tables: &BTreeMap<String, CatalogTable>,
) -> BTreeSet<Uuid> {
    let mut out = BTreeSet::new();
    for (_, schema, query) in literal_queries(ir, tables) {
        let mut walk = Walk::new(tables);
        walk.query(schema, &query);
        out.append(&mut walk.missing);
    }
    out
}

/// テーブルをリテラルで選び、材料に定義のある `data.query` と、その定義・クエリ（契約に合わない params は V1b が言う）。
fn literal_queries<'a>(
    ir: &'a WorkflowIr,
    tables: &'a BTreeMap<String, CatalogTable>,
) -> impl Iterator<Item = (&'a crate::ir::Node, &'a TableSchema, DataQuery)> {
    ir.nodes
        .iter()
        .filter(|n| n.node_type == NodeType::DataQuery.as_str())
        .filter_map(move |node| {
            let raw = node.params.get("table")?.as_str()?;
            let key = Uuid::parse_str(raw).ok()?.hyphenated().to_string();
            let schema = tables.get(&key)?.schema.as_ref()?;
            let params = serde_json::from_value::<DataQueryParams>(node.params.clone()).ok()?;
            Some((node, schema, params.query?))
        })
}

/// 1 つのクエリを歩いて、実行で断られる列を集める（リンク先は材料の定義で歩く）。
struct Walk<'c> {
    tables: &'c BTreeMap<String, CatalogTable>,
    /// 断る列（書いた位置の path・誤り）。
    refused: Vec<(String, ValidationError)>,
    /// 定義が要るのに材料に無いリンク先（[`query_link_tables`]）。
    missing: BTreeSet<Uuid>,
}

impl<'c> Walk<'c> {
    fn new(tables: &'c BTreeMap<String, CatalogTable>) -> Self {
        Walk {
            tables,
            refused: Vec::new(),
            missing: BTreeSet::new(),
        }
    }

    /// リンク先のテーブルの定義（材料に無ければ覚えて `None`。定義の無い材料も `None`）。
    fn target(&mut self, id: Uuid) -> Option<&'c TableSchema> {
        let Some(t) = self.tables.get(&id.hyphenated().to_string()) else {
            self.missing.insert(id);
            return None;
        };
        t.schema.as_ref()
    }

    /// 列を 1 つ照らす（実行時の束縛と同じ data の判定）。
    fn check(&mut self, u: &Use<'_>) {
        if let Err(e) = data::check_field_use(u.schema, &u.key, u.at) {
            self.refused.push((u.path.clone(), refusal(&e, u)));
        }
    }

    /// クエリの全体（書いた順）。
    fn query(&mut self, base: &'c TableSchema, q: &DataQuery) {
        if let Some(c) = &q.filter {
            self.cond(base, Within::Base, c, "/params/query/filter");
        }
        for u in uses(base, q) {
            self.check(&u);
        }
    }

    /// 条件の木の比較の列と、リンクの条件（辿る列と、リンク先の定義で中の条件）。
    fn cond(&mut self, schema: &'c TableSchema, within: Within, c: &Condition, at: &str) {
        match c {
            Condition::All(v) | Condition::Any(v) => {
                let kind = if matches!(c, Condition::All(_)) {
                    "all"
                } else {
                    "any"
                };
                for (i, x) in v.iter().enumerate() {
                    self.cond(schema, within, x, &format!("{at}/{kind}/{i}"));
                }
            }
            Condition::Not(x) => self.cond(schema, within, x, &format!("{at}/not")),
            Condition::Cmp(cmp) => self.check(&Use {
                path: format!("{at}/cmp/field"),
                schema,
                within,
                key: cmp.field.clone(),
                at: FieldUse::Filter(cmp.op),
            }),
            Condition::Link(lc) => self.link(schema, within, lc, at),
        }
    }

    /// リンクの条件: 辿る列がリンクの列であること（[`data::check_link_field`]）と、リンク先の定義で中の条件。
    fn link(&mut self, schema: &'c TableSchema, within: Within, lc: &LinkCond, at: &str) {
        let target = match data::check_link_field(schema, &lc.field) {
            Ok(id) => id,
            Err(e) => {
                let e = link_refusal(&e, schema, within, &lc.field);
                self.refused.push((format!("{at}/link/field"), e));
                return;
            }
        };
        // 中の条件が無ければリンク先の定義は要らない（引き足さない）。
        let Some(inner) = lc.filter.as_deref() else {
            return;
        };
        if let Some(target) = self.target(target) {
            self.cond(target, Within::Linked, inner, &format!("{at}/link/filter"));
        }
    }
}

/// FROM の表の、絞り込み以外の位置の列（書いた順）。
fn uses<'s>(schema: &'s TableSchema, q: &DataQuery) -> Vec<Use<'s>> {
    let mut out = Vec::new();
    let mut push = |path: String, key: &str, at: FieldUse| {
        out.push(Use {
            path,
            schema,
            within: Within::Base,
            key: key.to_string(),
            at,
        });
    };
    let keys = q.sort.len();
    for (i, k) in q.sort.iter().enumerate() {
        push(
            format!("/params/query/sort/{i}/field"),
            &k.field,
            FieldUse::Sort { keys },
        );
    }
    // FROM の表の列だけ（結合先の `別名.参照名` は照合しない）。
    let mut base = |path: String, key: &str, at: FieldUse| {
        if !key.contains('.') {
            push(path, key, at);
        }
    };
    for (i, g) in q.group_by.iter().enumerate() {
        base(format!("/params/query/group_by/{i}"), g, FieldUse::GroupBy);
    }
    for (i, m) in q.aggregate.iter().enumerate() {
        if let Some(f) = metric_field(m) {
            base(
                format!("/params/query/aggregate/{i}/field"),
                f,
                FieldUse::Metric,
            );
        }
    }
    if let Some(h) = &q.having {
        having_uses(h, "/params/query/having", &mut base);
    }
    for (i, k) in q.group_sort.iter().enumerate() {
        if let Some(f) = k.metric.as_ref().and_then(metric_field) {
            base(
                format!("/params/query/group_sort/{i}/metric/field"),
                f,
                FieldUse::Metric,
            );
        }
    }
    out
}

/// 指標が読む列。件数（`count`）は `field` を書いても読まない（実行時の束縛 `bind_metric` が `field` を見ずに返す。
/// SQL の `COUNT(列)` もこの形になる）ので照らさない。
fn metric_field(m: &MetricSpec) -> Option<&String> {
    if m.metric == Metric::Count {
        return None;
    }
    m.field.as_ref()
}

/// 集計後の条件の指標の列。
fn having_uses(h: &HavingCond, at: &str, push: &mut dyn FnMut(String, &str, FieldUse)) {
    match h {
        HavingCond::All(v) | HavingCond::Any(v) => {
            let kind = if matches!(h, HavingCond::All(_)) {
                "all"
            } else {
                "any"
            };
            for (i, x) in v.iter().enumerate() {
                having_uses(x, &format!("{at}/{kind}/{i}"), push);
            }
        }
        HavingCond::Not(x) => having_uses(x, &format!("{at}/not"), push),
        HavingCond::Cmp(cmp) => {
            if let Some(f) = metric_field(&cmp.metric) {
                push(format!("{at}/cmp/metric/field"), f, FieldUse::Metric);
            }
        }
    }
}

/// 書いた位置の path（前の版の書き方〔`sort: {field}` など〕で書いた IR は、その項目の位置まで戻す）。
fn located(params: &Value, path: &str) -> String {
    let rel = |p: &str| p.strip_prefix("/params").unwrap_or(p).to_string();
    let mut p = path.to_string();
    while params.pointer(&rel(&p)).is_none() {
        match p.rfind('/') {
            Some(i) if i > "/params/query".len() => p.truncate(i),
            _ => return "/params/query".to_string(),
        }
    }
    p
}

/// 位置の言い方。
fn place(at: FieldUse, within: Within) -> &'static str {
    match (at, within) {
        (FieldUse::Filter(_), Within::Base) => "絞り込みの条件",
        (FieldUse::Filter(_), Within::Linked) => "リンク先の絞り込みの条件",
        (FieldUse::Sort { .. }, _) => "並べ替え",
        (FieldUse::GroupBy, _) => "まとめる列",
        (FieldUse::Metric, _) => "集計",
    }
}

/// リンクの条件の辿る列を、実行時の束縛が断る理由（無い・読めない列と、リンクの列でない列）。
fn link_refusal(e: &DataError, schema: &TableSchema, within: Within, key: &str) -> ValidationError {
    if matches!(e, DataError::FieldUnavailable) {
        return ValidationError::new(
            UNKNOWN_FIELD,
            format!(
                "リンクの条件の「{key}」は、{}に無いか、読めない列です。",
                within.table()
            ),
        );
    }
    ValidationError::new(
        FIELD_NOT_USABLE,
        format!(
            "{}はリンクの列ではないので、リンクの条件に使えません。ほかのテーブルを指すリンクの列を選ぶか、この列を比べる条件（cmp）にしてください。",
            column_name(schema, key)
        ),
    )
}

/// 実行時の束縛が断る理由を、保存時の誤りにする（直し方まで言う）。
fn refusal(e: &DataError, u: &Use<'_>) -> ValidationError {
    let key = &u.key;
    let schema = u.schema;
    let col = column_name(schema, key);
    let place = place(u.at, u.within);
    let (table, edit) = (u.within.table(), u.within.edit());
    let partial = matches!(u.at, FieldUse::Filter(CondOp::Contains | CondOp::EndsWith));
    let text_op = matches!(
        u.at,
        FieldUse::Filter(CondOp::StartsWith | CondOp::Contains | CondOp::EndsWith)
    );
    let (code, message) = match (e, e.invalid_code()) {
        (DataError::FieldUnavailable, _) => (
            UNKNOWN_FIELD,
            format!("{place}の「{key}」は、{table}に無いか、読めない列です。"),
        ),
        (_, Some(InvalidCode::FieldNotIndexed)) if partial => (
            FIELD_NOT_INDEXED,
            format!(
                "{col}は部分一致で検索できる列ではないので、「含む」「で終わる」の条件に使えません。「等しい」「で始まる」の条件にするか、{edit}の「列の編集」でこの列の「部分一致で検索できる」をオンにしてください。"
            ),
        ),
        (_, Some(InvalidCode::FieldNotIndexed)) => (
            FIELD_NOT_INDEXED,
            format!(
                "{col}は索引の無い列なので、{place}に使えません。索引のある列か重複を許さない列を使うか、{edit}の「列の編集」でこの列の索引をオンにしてください。"
            ),
        ),
        (_, Some(InvalidCode::ValueTypeMismatch)) if text_op => (
            FIELD_NOT_USABLE,
            format!("{col}は文字の列ではないので、「含む」「で始まる」「で終わる」の条件に使えません。「等しい」などの条件にしてください。"),
        ),
        (_, Some(InvalidCode::ValueTypeMismatch)) if u.at == FieldUse::Metric => (
            FIELD_NOT_USABLE,
            format!("{col}は数の列ではないので、合計・平均・最小・最大に使えません。数の列を選ぶか、件数（count）にしてください。"),
        ),
        (_, Some(InvalidCode::InvalidInput)) if matches!(u.at, FieldUse::Sort { .. }) => (
            FIELD_NOT_USABLE,
            format!("並べ替えには、このテーブルの列を使ってください（つないだ表の列「{key}」では並べ替えられません）。"),
        ),
        _ => (FIELD_NOT_USABLE, not_usable(u, &col)),
    };
    ValidationError::new(code, message)
}

/// その位置に使えない種類の列の文言（逆向きのリンク・計算の列・複数の値やリンクを持つ列・システムの列）。
/// `col` は誤りの文言での列の言い方（[`column_name`]）。
fn not_usable(u: &Use<'_>, col: &str) -> String {
    let reverse = u
        .schema
        .field(&u.key)
        .is_some_and(data::FieldDef::is_reverse_link);
    if reverse {
        return format!(
            "{col}は逆向きのリンクの列なので、{}に使えません。リンク元のテーブルの列を使ってください。",
            place(u.at, u.within)
        );
    }
    match u.at {
        FieldUse::Filter(_) => format!(
            "{col}は、この条件に使えない列です（複数の値やリンクを持つ列は大小を比べられず、計算で決まる列の一部は条件に使えません）。ほかの列か条件にしてください。"
        ),
        FieldUse::Sort { .. } => format!(
            "{col}では並べ替えできません（複数の値やリンクを持つ列は使えず、計算で決まる列は並べ替えの列が 1 つのときだけ使えます）。ほかの列で並べ替えてください。"
        ),
        FieldUse::GroupBy => format!(
            "{col}はまとめる列に使えません（複数の値やリンクを持つ列、作成者・作成日時などのシステムの列は使えません）。ほかの列でまとめてください。"
        ),
        FieldUse::Metric => format!(
            "{col}は集計に使えない列です（計算で決まる列の一部は使えません）。ほかの数の列を選んでください。"
        ),
    }
}

#[cfg(test)]
#[path = "data_query_tests.rs"]
mod tests;
