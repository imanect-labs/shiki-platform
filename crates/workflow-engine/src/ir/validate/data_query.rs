//! 「データを検索」（`data.query`）の列の保存時検証 V4（BUG-244）。
//!
//! テーブルをリテラルで選んだ `data.query` の絞り込み（`filter` の `all` / `any` / `not` の中の `cmp.field`）・並べ替え
//! （`sort`）・まとめる列（`group_by`）・集計の列（`aggregate`・`having`・`group_sort` の指標の `field`）に、実行で
//! 必ず断られる列があれば保存で止める。判定は実行時の束縛と同じ data の [`data::check_field_use`] で行う（規則を
//! 写さない）。主に、索引の無い列（`indexed` でも `unique` でもない列）で絞る・並べる・まとめる IR を止める
//! （ワークフローの経路では索引を自動で作らない・data-platform §10）。
//!
//! リンクの条件（`filter` の `link`・入れ子も）と結合（`joins`）は、辿る列（`link.field`・`joins[].field`）がリンクの列で
//! あることを [`data::check_link_field`] で確かめ、その先の列（リンクの条件の中・結合の条件 `joins[].filter`・
//! まとめる列と集計の列の `別名.参照名`）を、辿った先のテーブルの定義で同じ [`data::check_field_use`] に問う
//! （GAP-129）。`別名.参照名` の別名が結合に無い IR も止める（実行では「別名は結合にありません」で断られる）。
//!
//! 材料は承認と同じ [`Catalog::tables`] の、保存者の見え方の定義（[`super::CatalogTable::schema`]・読めない列は
//! 入らない）。読めない列は無い列と同じ `ir.unknown_field`（同じ文言・data-platform §16.1）。テーブルを参照で決める
//! ノード・材料に無い（見えない）テーブル・定義の無い材料は照合しない。リンク先・結合先のテーブルも、API 層が
//! [`query_link_tables`]（照合と同じ歩き方）で保存者の見え方で引き足したものだけを照らす（見えない表の中は
//! 照合しない。実行では、辿る先を開けなければ問い合わせ全体が断られる）。並べ替えと絞り込みは結合先の列を
//! 使えないので、`別名.参照名` を書けば実行と同じく止める。件数（`count`）の指標の `field` は、実行で読まないので
//! 照合しない（実行時の束縛の `bind_metric` と同じ）。比べる値（`$from` を書ける）は実行時に確かめる。
//!
//! 誤りの文言は、列に表示名があれば「表示名」（参照名）で言う（画面は表示名で列を見せ、AI は参照名で IR を直す）。

use std::collections::{BTreeMap, BTreeSet};

use data::{
    CondOp, Condition, DataError, DataQuery, FieldUse, HavingCond, InvalidCode, JoinSpec, Metric,
    MetricSpec, TableSchema,
};
use serde_json::Value;
use uuid::Uuid;

use super::data_keys::{column_name, column_name_as};
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
enum Within<'q> {
    /// データを検索のテーブル（FROM の表）。
    Base,
    /// リンクの条件で辿った先のテーブル。
    Linked,
    /// 結合した表（別名）。
    Joined(&'q str),
}

impl Within<'_> {
    /// 「…に無いか、読めない列です」の表の言い方。
    fn table(self) -> String {
        match self {
            Within::Base => "このテーブル".to_string(),
            Within::Linked => "リンク先のテーブル".to_string(),
            Within::Joined(alias) => format!("つないだテーブル（{alias}）"),
        }
    }

    /// 「…の「列の編集」で」の表の言い方。
    fn edit(self) -> String {
        match self {
            Within::Base => "テーブル".to_string(),
            other => other.table(),
        }
    }
}

/// 照らす列（書いた位置の path・列を引く表の定義・列・IR に書いた名前・位置）。
struct Use<'s, 'q> {
    path: String,
    schema: &'s TableSchema,
    within: Within<'q>,
    key: String,
    /// IR に書いた名前（結合先の列は `別名.参照名`・ほかは `key` と同じ）。
    written: String,
    at: FieldUse,
}

impl<'s, 'q> Use<'s, 'q> {
    fn new(
        path: String,
        schema: &'s TableSchema,
        within: Within<'q>,
        key: &str,
        at: FieldUse,
    ) -> Self {
        Use {
            path,
            schema,
            within,
            key: key.to_string(),
            written: key.to_string(),
            at,
        }
    }
}

/// 辿る列（リンクの条件の `link.field` と結合の `joins[].field`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Through {
    Link,
    Join,
}

/// 結合（別名と、辿った先の定義。辿れない・材料に無い結合先は `None`）。
type Joined<'c, 'q> = Vec<(&'q str, Option<&'c TableSchema>)>;

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

/// データを検索（リテラルのテーブル・定義のある材料）のリンクの条件と結合が辿る先のうち、`tables` に無いテーブル。
/// API 層が保存者の見え方で引き足し、その中の列を照らす（GAP-129）。照合（[`v4`]）と同じ歩き方で決めるので、中の条件の
/// あるリンク先と、結合先（次の結合の結合元にもなる）だけが返る。引き足すと、その先が分かる（入れ子のリンクと結合の
/// 連なりは、深さの上限〔[`data::MAX_JOIN_DEPTH`]〕まで引き足しを繰り返す）。
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
    fn check(&mut self, u: &Use<'_, '_>) {
        if let Err(e) = data::check_field_use(u.schema, &u.key, u.at) {
            self.refused.push((u.path.clone(), refusal(&e, u)));
        }
    }

    /// クエリの全体（実行時の束縛と同じ順: 絞り込み → 並べ替え → 結合 → まとめる列・集計）。
    fn query<'q>(&mut self, base: &'c TableSchema, q: &'q DataQuery) {
        if let Some(c) = &q.filter {
            self.cond(base, Within::Base, c, "/params/query/filter");
        }
        let keys = q.sort.len();
        for (i, k) in q.sort.iter().enumerate() {
            let path = format!("/params/query/sort/{i}/field");
            self.check(&Use::new(
                path,
                base,
                Within::Base,
                &k.field,
                FieldUse::Sort { keys },
            ));
        }
        let joined = self.joins(base, &q.joins);
        let mut col =
            |path: String, name: &str, at: FieldUse| self.col(base, &joined, path, name, at);
        for (i, g) in q.group_by.iter().enumerate() {
            col(format!("/params/query/group_by/{i}"), g, FieldUse::GroupBy);
        }
        for (i, m) in q.aggregate.iter().enumerate() {
            if let Some(f) = metric_field(m) {
                col(
                    format!("/params/query/aggregate/{i}/field"),
                    f,
                    FieldUse::Metric,
                );
            }
        }
        if let Some(h) = &q.having {
            having_uses(h, "/params/query/having", &mut col);
        }
        for (i, k) in q.group_sort.iter().enumerate() {
            if let Some(f) = k.metric.as_ref().and_then(metric_field) {
                col(
                    format!("/params/query/group_sort/{i}/metric/field"),
                    f,
                    FieldUse::Metric,
                );
            }
        }
    }

    /// 結合: 結合元の表で辿る列（`joins[].field`）と、結合先の定義で結合の条件（`joins[].filter`）。結合元の別名
    /// （`from`）が先の結合に無い結合と、その先は照らさない（実行では結合の誤りで断られる）。
    fn joins<'q>(&mut self, base: &'c TableSchema, joins: &'q [JoinSpec]) -> Joined<'c, 'q> {
        let mut out: Joined<'c, 'q> = Vec::with_capacity(joins.len());
        for (i, j) in joins.iter().enumerate() {
            let from = match &j.from {
                None => Some((base, Within::Base)),
                Some(a) => out
                    .iter()
                    .find(|(x, _)| x == a)
                    .and_then(|(x, s)| s.map(|s| (s, Within::Joined(x)))),
            };
            let target = from.and_then(|(schema, within)| {
                let at = format!("/params/query/joins/{i}/field");
                self.through(schema, within, &j.field, Through::Join, at)
            });
            let target = target.and_then(|id| self.target(id));
            if let (Some(t), Some(c)) = (target, &j.filter) {
                let at = format!("/params/query/joins/{i}/filter");
                self.cond(t, Within::Joined(&j.alias), c, &at);
            }
            out.push((j.alias.as_str(), target));
        }
        out
    }

    /// まとめる列・集計の列（`参照名` は FROM の表、`別名.参照名` は結合先の定義で照らす）。
    fn col(
        &mut self,
        base: &'c TableSchema,
        joined: &Joined<'c, '_>,
        path: String,
        name: &str,
        at: FieldUse,
    ) {
        let Some((alias, key)) = name.split_once('.') else {
            return self.check(&Use::new(path, base, Within::Base, name, at));
        };
        match joined.iter().find(|(a, _)| *a == alias) {
            None => {
                let message = format!(
                    "{}の「{name}」の「{alias}」は、つないだ表の別名（joins の alias）にありません。joins に書いた別名を使ってください。",
                    place(at, Within::Base)
                );
                self.refused
                    .push((path, ValidationError::new(FIELD_NOT_USABLE, message)));
            }
            // 結合先を辿れない・見えない（材料に無い）ときは照らさない。
            Some((_, None)) => {}
            Some((a, Some(schema))) => self.check(&Use {
                written: name.to_string(),
                ..Use::new(path, schema, Within::Joined(a), key, at)
            }),
        }
    }

    /// 条件の木の比較の列と、リンクの条件（辿る列と、リンク先の定義で中の条件）。
    fn cond(&mut self, schema: &'c TableSchema, within: Within<'_>, c: &Condition, at: &str) {
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
            Condition::Cmp(cmp) => {
                let path = format!("{at}/cmp/field");
                self.check(&Use::new(
                    path,
                    schema,
                    within,
                    &cmp.field,
                    FieldUse::Filter(cmp.op),
                ));
            }
            Condition::Link(lc) => {
                let path = format!("{at}/link/field");
                let target = self.through(schema, within, &lc.field, Through::Link, path);
                // 中の条件が無ければリンク先の定義は要らない（引き足さない）。
                let (Some(target), Some(inner)) = (target, lc.filter.as_deref()) else {
                    return;
                };
                if let Some(t) = self.target(target) {
                    self.cond(t, Within::Linked, inner, &format!("{at}/link/filter"));
                }
            }
        }
    }

    /// 辿る列がリンクの列であること（[`data::check_link_field`]・実行時の束縛と同じ関数）と、その参照先のテーブル。
    fn through(
        &mut self,
        schema: &TableSchema,
        within: Within<'_>,
        key: &str,
        by: Through,
        path: String,
    ) -> Option<Uuid> {
        match data::check_link_field(schema, key) {
            Ok(id) => Some(id),
            Err(e) => {
                self.refused
                    .push((path, link_refusal(&e, schema, within, key, by)));
                None
            }
        }
    }
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
fn having_uses(h: &HavingCond, at: &str, push: &mut impl FnMut(String, &str, FieldUse)) {
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
fn place(at: FieldUse, within: Within<'_>) -> &'static str {
    match (at, within) {
        (FieldUse::Filter(_), Within::Base) => "絞り込みの条件",
        (FieldUse::Filter(_), Within::Linked) => "リンク先の絞り込みの条件",
        (FieldUse::Filter(_), Within::Joined(_)) => "表をつなぐ条件",
        (FieldUse::Sort { .. }, _) => "並べ替え",
        (FieldUse::GroupBy, _) => "まとめる列",
        (FieldUse::Metric, _) => "集計",
    }
}

/// 辿る列を、実行時の束縛が断る理由（無い・読めない列と、リンクの列でない列）。
fn link_refusal(
    e: &DataError,
    schema: &TableSchema,
    within: Within<'_>,
    key: &str,
    by: Through,
) -> ValidationError {
    let table = within.table();
    let col = column_name(schema, key);
    let message = match (e, by) {
        (DataError::FieldUnavailable, Through::Link) => {
            return ValidationError::new(
                UNKNOWN_FIELD,
                format!("リンクの条件の「{key}」は、{table}に無いか、読めない列です。"),
            )
        }
        (DataError::FieldUnavailable, Through::Join) => {
            return ValidationError::new(
                UNKNOWN_FIELD,
                format!("表をつなぐ列の「{key}」は、{table}に無いか、読めない列です。"),
            )
        }
        (_, Through::Link) => format!(
            "{col}はリンクの列ではないので、リンクの条件に使えません。ほかのテーブルを指すリンクの列を選ぶか、この列を比べる条件（cmp）にしてください。"
        ),
        (_, Through::Join) => format!(
            "{col}はリンクの列ではないので、表をつなぐのに使えません。ほかのテーブルを指すリンクの列を選んでください。"
        ),
    };
    ValidationError::new(FIELD_NOT_USABLE, message)
}

/// 実行時の束縛が断る理由を、保存時の誤りにする（直し方まで言う）。
fn refusal(e: &DataError, u: &Use<'_, '_>) -> ValidationError {
    let key = &u.key;
    let written = &u.written;
    let schema = u.schema;
    let col = column_name_as(schema, key, written);
    let place = place(u.at, u.within);
    let (table, edit) = (u.within.table(), u.within.edit());
    let partial = matches!(u.at, FieldUse::Filter(CondOp::Contains | CondOp::EndsWith));
    let text_op = matches!(
        u.at,
        FieldUse::Filter(CondOp::StartsWith | CondOp::Contains | CondOp::EndsWith)
    );
    let (code, message) = match (e, e.invalid_code()) {
        // `別名.参照名` は利用者の列の参照名になれない（列の手がかりにならない）ので、書き方の誤りとして言う。
        (DataError::FieldUnavailable, _) if key.contains('.') && matches!(u.at, FieldUse::Filter(_)) => (
            UNKNOWN_FIELD,
            format!("{place}には「{key}」のような、つないだ表の列は書けません。つないだ表の列で絞るときは、その表をつなぐ設定（joins）の filter に、その表の参照名で書いてください。"),
        ),
        (DataError::FieldUnavailable, _) => (
            UNKNOWN_FIELD,
            format!("{place}の「{written}」は、{table}に無いか、読めない列です。"),
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
fn not_usable(u: &Use<'_, '_>, col: &str) -> String {
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
