//! V5 の一部: `$from` の path を、出力の形から決まる範囲で照らす（GAP-137）。
//!
//! 照らすのは、どの実行でも形の変わらない出力だけ（[`NodeType::closed_output_keys`]）。
//!
//! - data.query の結果の最上位（集計を書いたかで items と groups のどちらかになる・件数は with_count のとき）と、
//!   items・groups の行の項目（行の列の値は `/items/<n>/data/<列>`）。
//! - data.record.create・update・upsert・delete と data.transition の結果の最上位。
//! - 繰り返しの中の 1 件（`$from: each`）の最上位（item・index）と、繰り返す並びが data.query の items か
//!   groups のときの、1 件の行の項目。
//!
//! `on_error: continue` のブロックの失敗の出力 `{ error }` は、どの種類でも読める。ほかの種類（項目の決まらない
//! script.run・項目の表に漏れのある llm.invoke など）と `$template` の文の中の差し込みは照らさない。解決しない
//! path は、条件の中では値が無いものとして比べられ（黙って偽になる）、値の位置ではブロックの失敗になるので、
//! どちらも保存の誤りにする（map の外の each を誤りにするのと同じ扱い）。

use std::collections::HashMap;

use crate::ir::expr::FromRef;
use crate::ir::{Node, OnError, WorkflowIr};
use crate::vocab::NodeType;

use super::super::block_name;

/// 出力に無い所を指す `$from` の誤りのコード（保存を止める）。
pub(crate) const UNKNOWN_OUTPUT_PATH: &str = "ir.unknown_output_path";

/// 参照の置き場所（解決しなかったときの実行の振る舞いが違う）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    /// 分岐の条件の比較の値そのもの。解決しなければ値が無いものとして比べる（`control::eval` の
    /// `resolve_operand`）。
    Condition,
    /// それ以外の値の位置。解決しなければブロックが失敗する（`control::eval::resolve_deep`）。
    Value,
}

/// 照らす材料（IR のブロックを id で引く）。
pub(super) struct OutputShapes<'a> {
    nodes: HashMap<&'a str, &'a Node>,
}

impl<'a> OutputShapes<'a> {
    pub(super) fn new(ir: &'a WorkflowIr) -> Self {
        OutputShapes {
            nodes: ir.nodes.iter().map(|n| (n.id.as_str(), n)).collect(),
        }
    }

    /// `user` の中の `$from` 1 つを照らす。出力に無い所を指していれば、その文を返す。
    ///
    /// 照らせない参照（path が無い・/ で始まらない・指すブロックが無い・形の決まらない種類）は `None`。
    pub(super) fn check(&self, user: &Node, from: &FromRef, place: Place) -> Option<String> {
        let segs = segments(from.path.as_deref()?)?;
        let found = if from.from == "each" {
            self.each_problem(user, &segs)?
        } else {
            let id = from.from.strip_prefix("nodes.")?.strip_suffix(".output")?;
            output_problem(self.nodes.get(id)?, &segs)?
        };
        Some(format!("{found}{}", consequence(from, place)))
    }

    /// 繰り返しの中の 1 件（`$from: each`）の `segs` が無い所を指すときの文。
    fn each_problem(&self, user: &Node, segs: &[String]) -> Option<String> {
        let map = self.nodes.get(user.parent.as_deref()?)?;
        let keys = NodeType::parse(&map.node_type)?.element_keys()?;
        let first = segs[0].as_str();
        if !keys.contains(&first) {
            return Some(format!(
                "繰り返し中の 1 件に {first} はありません（項目: {}）。要素の中は item の下にあります（/item/{first}）。",
                keys.join(", ")
            ));
        }
        if first != "item" {
            return None;
        }
        let k = segs.get(1)?;
        let (q, list) = self.map_rows(map)?;
        let row = NodeType::DataQuery.closed_row_keys(list)?;
        if row.contains(&k.as_str()) {
            return None;
        }
        let name = named(q);
        Some(if list == "items" {
            format!(
                "繰り返し中の 1 件（{name}の結果の行）に {k} はありません（行の項目: {}）。行の列の値は data の下にあります（/item/data/{k}）。",
                record_fields()
            )
        } else {
            format!(
                "繰り返し中の 1 件（{name}の集計の行）に {k} はありません（集計の行の項目: {}）。まとめた列の値は key の下（/item/key/<列>）、集計の値は value か values（/item/values/<aggregate の位置>）にあります。",
                row.join(", ")
            )
        })
    }

    /// 繰り返す並びが data.query の items か groups なら、その data.query と並びの名前。並びの指し方そのものが
    /// 誤り（集計を書いたのに items など）なら、繰り返しの側で言うので `None`。
    fn map_rows(&self, map: &Node) -> Option<(&'a Node, &'static str)> {
        let items: FromRef = serde_json::from_value(map.params.get("items")?.clone()).ok()?;
        let id = items.from.strip_prefix("nodes.")?.strip_suffix(".output")?;
        let q = *self.nodes.get(id)?;
        if NodeType::parse(&q.node_type) != Some(NodeType::DataQuery) {
            return None;
        }
        let list = match items.path.as_deref() {
            Some("/items") => "items",
            Some("/groups") => "groups",
            _ => return None,
        };
        output_problem(q, &[list.to_string()])
            .is_none()
            .then_some((q, list))
    }
}

/// `target` の出力の `segs` が無い所を指すときの文。
fn output_problem(target: &Node, segs: &[String]) -> Option<String> {
    let nt = NodeType::parse(&target.node_type)?;
    let keys = nt.closed_output_keys()?;
    let first = segs[0].as_str();
    if first == "error" && target.on_error == OnError::Continue {
        return None;
    }
    let name = named(target);
    if !keys.contains(&first) {
        return Some(top_unknown(nt, &name, first));
    }
    if let Some(text) = absent_by_params(nt, target, &name, first) {
        return Some(text);
    }
    let row = nt.closed_row_keys(first)?;
    let n = segs.get(1)?;
    if !is_index(n) {
        return Some(format!(
            "{name}の結果の {first} は行の並びなので、/{first} の次は 0 から数えた番号です（{}）。",
            row_example(first, "0")
        ));
    }
    let k = segs.get(2)?;
    (!row.contains(&k.as_str())).then(|| row_unknown(&name, first, n, k))
}

/// 文の中でブロックを名指しする（「名前」（id））。
fn named(node: &Node) -> String {
    format!("「{}」（{}）", block_name(node), node.id)
}

/// レコードの項目（説明文が教えるもの・data.record.create の出力の項目）。
fn record_fields() -> String {
    field_names(NodeType::DataRecordCreate)
}

/// 種類の出力の項目の名前（説明文が教えるもの）。
fn field_names(nt: NodeType) -> String {
    nt.output_fields()
        .iter()
        .map(|f| f.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// 出力の最上位に無い項目を指すときの文。
fn top_unknown(nt: NodeType, name: &str, k: &str) -> String {
    if k == "error" {
        return format!(
            "{name}の結果に error はありません。error は、on_error を continue にしたブロックが失敗したときだけ入ります。"
        );
    }
    match nt {
        NodeType::DataQuery => format!(
            "{name}の結果に {k} はありません（結果の項目: {}）。行の列の値は、行の data の下にあります（/items/0/data/{k}）。",
            field_names(nt)
        ),
        NodeType::DataRecordDelete => format!(
            "{name}の結果に {k} はありません（結果の項目: {}）。",
            field_names(nt)
        ),
        _ => format!(
            "{name}の結果のレコードに {k} はありません（レコードの項目: {}）。列の値は data の下にあります（/data/{k}）。",
            field_names(nt)
        ),
    }
}

/// 設定で決まる項目（data.query の groups・count と、集計を書いたときの items）が、書いた設定では入らないときの文。
/// 設定が `$from` などで実行の値で決まるときは照らさない。
fn absent_by_params(nt: NodeType, target: &Node, name: &str, k: &str) -> Option<String> {
    let field = |n: &str| nt.output_fields().iter().find(|f| f.name == n);
    if nt == NodeType::DataQuery
        && k == "items"
        && field("groups")?.present_in(&target.params) == Some(true)
    {
        return Some(format!(
            "{name}は集計（aggregate）を書いているので、結果は groups に入り、items はありません（{}・/groups/0/value）。",
            row_example("groups", "0")
        ));
    }
    let f = field(k)?;
    if f.present_in(&target.params) != Some(false) {
        return None;
    }
    Some(match k {
        "groups" => format!(
            "{name}は集計（aggregate）を書いていないので、結果に groups はありません。行は items に入ります（{}）。",
            row_example("items", "0")
        ),
        "count" => format!(
            "{name}は件数（with_count）を求めていないので、結果に count はありません。件数が要るなら query に \"with_count\": true を書きます。"
        ),
        _ => format!(
            "{name}の設定に {} が無いので、結果に {k} はありません。",
            f.present_if
        ),
    })
}

/// 行の並びの読み方の例。
fn row_example(list: &str, n: &str) -> String {
    if list == "groups" {
        format!("/groups/{n}/key/<列>")
    } else {
        format!("/{list}/{n}/data/<列>")
    }
}

/// 行に無い項目を指すときの文。
fn row_unknown(name: &str, list: &str, n: &str, k: &str) -> String {
    if list == "groups" {
        format!(
            "{name}の集計の行に {k} はありません（集計の行の項目: {}）。まとめた列の値は key の下（/groups/{n}/key/<列>）、集計の値は value か values（/groups/{n}/values/<aggregate の位置>）にあります。",
            NodeType::DataQuery
                .closed_row_keys(list)
                .unwrap_or_default()
                .join(", ")
        )
    } else {
        format!(
            "{name}の結果の行に {k} はありません（行の項目: {}）。行の列の値は data の下にあります（/{list}/{n}/data/{k}）。",
            record_fields()
        )
    }
}

/// 解決しないときの実行の振る舞い（置き場所と既定値で決まる・`control::eval`）。
fn consequence(from: &FromRef, place: Place) -> &'static str {
    if from.default.is_some() {
        "この参照は値を解決できないので、いつも既定値（default）が使われます。"
    } else if place == Place::Condition {
        "条件の中では値が無いものとして扱われ、「等しい」はいつも偽、「等しくない」はいつも真になります。"
    } else {
        "実行すると、このブロックは値を解決できずに失敗します。"
    }
}

/// JSON Pointer の段（`~1` は `/`・`~0` は `~` に戻す）。空の path（全体）と / で始まらない path（V5 の
/// `ir.bad_ref` が言う）は `None`。
fn segments(path: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix('/')?;
    Some(
        rest.split('/')
            .map(|s| s.replace("~1", "/").replace("~0", "~"))
            .collect(),
    )
}

/// serde_json の JSON Pointer が配列の番号として読む段か（`0` か、0 で始まらない数字の並び。`+`・`-`・先頭の 0 は
/// 読まない・`Value::pointer`）。
fn is_index(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s == "0" || !s.starts_with('0'))
        && s.parse::<usize>().is_ok()
}

#[cfg(test)]
#[path = "output_refs_tests.rs"]
mod tests;
