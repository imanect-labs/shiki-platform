//! 「データを検索」（`data.query`）の列の保存時検証 V4（BUG-244）。
//!
//! テーブルをリテラルで選んだ `data.query` の絞り込み（`filter` の `all` / `any` / `not` の中の `cmp.field`）・並べ替え
//! （`sort`）・まとめる列（`group_by`）・集計の列（`aggregate`・`having`・`group_sort` の指標の `field`）に、実行で
//! 必ず断られる列があれば保存で止める。判定は実行時の束縛と同じ data の [`data::check_field_use`] で行う（規則を
//! 写さない）。主に、索引の無い列（`indexed` でも `unique` でもない列）で絞る・並べる・まとめる IR を止める
//! （ワークフローの経路では索引を自動で作らない・data-platform §10）。
//!
//! 材料は承認と同じ [`Catalog::tables`] の、保存者の見え方の定義（[`super::CatalogTable::schema`]・読めない列は
//! 入らない）。読めない列は無い列と同じ `ir.unknown_field`（同じ文言・data-platform §16.1）。テーブルを参照で決める
//! ノード・材料に無い（見えない）テーブル・定義の無い材料は照合しない。リンクの条件（`link`）と、まとめる列・集計の列の
//! 結合先の列（`別名.参照名`）は、つないだ先の定義が要るので照合しない（並べ替えと絞り込みは結合先の列を使えないので、
//! 実行と同じく止める）。件数（`count`）の指標の `field` は、実行で読まないので照合しない（実行時の束縛の
//! `bind_metric` と同じ）。比べる値（`$from` を書ける）は実行時に確かめる。
//!
//! 誤りの文言は、列に表示名があれば「表示名」（参照名）で言う（画面は表示名で列を見せ、AI は参照名で IR を直す）。

use data::{
    CondOp, Condition, DataError, DataQuery, FieldUse, HavingCond, InvalidCode, Metric, MetricSpec,
    TableSchema,
};
use serde_json::Value;

use super::data_keys::column_name;
use super::{Catalog, ValidationError};
use crate::ir::params::DataQueryParams;
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

/// 誤りのコード: 索引の無い列で絞り込み・並べ替え・まとめ・集計をする（部分一致は検索の索引の無い列）。
pub const FIELD_NOT_INDEXED: &str = "ir.field_not_indexed";
/// 誤りのコード: その位置に使えない種類の列（逆向きのリンク・複数の値・数でない列の集計など）。
pub const FIELD_NOT_USABLE: &str = "ir.field_not_usable";
/// 誤りのコード: 無い列か読めない列（承認・同じレコードを探す列と同じ）。
const UNKNOWN_FIELD: &str = "ir.unknown_field";

/// 照らす列（書いた位置の path・列・位置）。
struct Use {
    path: String,
    key: String,
    at: FieldUse,
}

/// V4: `data.query` の列（材料が無ければ照合しない）。
pub(super) fn v4(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    if catalog.tables.is_none() {
        return;
    }
    for node in ir
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::DataQuery.as_str())
    {
        let Some(schema) =
            super::data_keys::table_of(catalog, &node.params).and_then(|t| t.schema.as_ref())
        else {
            continue;
        };
        // 契約に合わない params は V1b が言う。
        let Ok(params) = serde_json::from_value::<DataQueryParams>(node.params.clone()) else {
            continue;
        };
        let Some(query) = params.query else { continue };
        for u in uses(&query) {
            if let Err(e) = data::check_field_use(schema, &u.key, u.at) {
                let path = located(&node.params, &u.path);
                errors.push(refusal(&e, &u, schema).at_node(&node.id).at_path(path));
            }
        }
    }
}

/// クエリが列を書く位置（書いた順）。
fn uses(q: &DataQuery) -> Vec<Use> {
    let mut out = Vec::new();
    if let Some(c) = &q.filter {
        filter_uses(c, "/params/query/filter", &mut out);
    }
    let keys = q.sort.len();
    for (i, k) in q.sort.iter().enumerate() {
        out.push(Use {
            path: format!("/params/query/sort/{i}/field"),
            key: k.field.clone(),
            at: FieldUse::Sort { keys },
        });
    }
    for (i, g) in q.group_by.iter().enumerate() {
        push_base(
            &mut out,
            format!("/params/query/group_by/{i}"),
            g,
            FieldUse::GroupBy,
        );
    }
    for (i, m) in q.aggregate.iter().enumerate() {
        if let Some(f) = metric_field(m) {
            let path = format!("/params/query/aggregate/{i}/field");
            push_base(&mut out, path, f, FieldUse::Metric);
        }
    }
    if let Some(h) = &q.having {
        having_uses(h, "/params/query/having", &mut out);
    }
    for (i, k) in q.group_sort.iter().enumerate() {
        if let Some(f) = k.metric.as_ref().and_then(metric_field) {
            let path = format!("/params/query/group_sort/{i}/metric/field");
            push_base(&mut out, path, f, FieldUse::Metric);
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

/// FROM の表の列だけを足す（結合先の `別名.参照名` は照合しない）。
fn push_base(out: &mut Vec<Use>, path: String, key: &str, at: FieldUse) {
    if !key.contains('.') {
        out.push(Use {
            path,
            key: key.to_string(),
            at,
        });
    }
}

/// 条件の木の比較の列（リンクの条件は参照先の列なので照合しない）。
fn filter_uses(c: &Condition, at: &str, out: &mut Vec<Use>) {
    match c {
        Condition::All(v) | Condition::Any(v) => {
            let kind = if matches!(c, Condition::All(_)) {
                "all"
            } else {
                "any"
            };
            for (i, x) in v.iter().enumerate() {
                filter_uses(x, &format!("{at}/{kind}/{i}"), out);
            }
        }
        Condition::Not(x) => filter_uses(x, &format!("{at}/not"), out),
        Condition::Cmp(cmp) => out.push(Use {
            path: format!("{at}/cmp/field"),
            key: cmp.field.clone(),
            at: FieldUse::Filter(cmp.op),
        }),
        Condition::Link(_) => {}
    }
}

/// 集計後の条件の指標の列。
fn having_uses(h: &HavingCond, at: &str, out: &mut Vec<Use>) {
    match h {
        HavingCond::All(v) | HavingCond::Any(v) => {
            let kind = if matches!(h, HavingCond::All(_)) {
                "all"
            } else {
                "any"
            };
            for (i, x) in v.iter().enumerate() {
                having_uses(x, &format!("{at}/{kind}/{i}"), out);
            }
        }
        HavingCond::Not(x) => having_uses(x, &format!("{at}/not"), out),
        HavingCond::Cmp(cmp) => {
            if let Some(f) = metric_field(&cmp.metric) {
                push_base(out, format!("{at}/cmp/metric/field"), f, FieldUse::Metric);
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
fn place(at: FieldUse) -> &'static str {
    match at {
        FieldUse::Filter(_) => "絞り込みの条件",
        FieldUse::Sort { .. } => "並べ替え",
        FieldUse::GroupBy => "まとめる列",
        FieldUse::Metric => "集計",
    }
}

/// 実行時の束縛が断る理由を、保存時の誤りにする（直し方まで言う）。
fn refusal(e: &DataError, u: &Use, schema: &TableSchema) -> ValidationError {
    let key = &u.key;
    let col = column_name(schema, key);
    let place = place(u.at);
    let partial = matches!(u.at, FieldUse::Filter(CondOp::Contains | CondOp::EndsWith));
    let text_op = matches!(
        u.at,
        FieldUse::Filter(CondOp::StartsWith | CondOp::Contains | CondOp::EndsWith)
    );
    let (code, message) = match (e, e.invalid_code()) {
        (DataError::FieldUnavailable, _) => (
            UNKNOWN_FIELD,
            format!("{place}の「{key}」は、このテーブルに無いか、読めない列です。"),
        ),
        (_, Some(InvalidCode::FieldNotIndexed)) if partial => (
            FIELD_NOT_INDEXED,
            format!(
                "{col}は部分一致で検索できる列ではないので、「含む」「で終わる」の条件に使えません。「等しい」「で始まる」の条件にするか、テーブルの「列の編集」でこの列の「部分一致で検索できる」をオンにしてください。"
            ),
        ),
        (_, Some(InvalidCode::FieldNotIndexed)) => (
            FIELD_NOT_INDEXED,
            format!(
                "{col}は索引の無い列なので、{place}に使えません。索引のある列か重複を許さない列を使うか、テーブルの「列の編集」でこの列の索引をオンにしてください。"
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
        _ => (FIELD_NOT_USABLE, not_usable(key, &col, u.at, schema)),
    };
    ValidationError::new(code, message)
}

/// その位置に使えない種類の列の文言（逆向きのリンク・計算の列・複数の値やリンクを持つ列・システムの列）。
/// `col` は誤りの文言での列の言い方（[`column_name`]）。
fn not_usable(key: &str, col: &str, at: FieldUse, schema: &TableSchema) -> String {
    let reverse = schema
        .field(key)
        .is_some_and(data::FieldDef::is_reverse_link);
    if reverse {
        return format!(
            "{col}は逆向きのリンクの列なので、{}に使えません。リンク元のテーブルの列を使ってください。",
            place(at)
        );
    }
    match at {
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
