//! 状態の列（状態の進め方〔FSM〕のあるテーブル）への書き込みの保存時検証 V4（BUG-251）。
//!
//! 状態の列は「状態を進める」（`data.transition`）でだけ変えられる。data は、更新の値に状態の列があると断り、作成の値の
//! 状態が最初の状態と違うと断る（どちらも `fsm_invalid`）。テーブルをリテラルで選んだ次の IR は実行で必ず断られるので、
//! 保存で止める。判定は実行時と同じ data の関数（[`data::reject_status_write`]・[`data::FsmInitial::apply`]）で行う。
//!
//! - `data.record.update` の変更する値（`patch`）と足す数（`add`）に状態の列がある。
//! - `data.record.upsert` の入れる値（`data`・同じレコードを探す列を除く）と足す数に状態の列がある。書き換えのときは
//!   必ず断られ、追加のときは書かなくても最初の状態になる（外しても追加の結果は変わらない）。
//! - `data.record.create`（と、探す列が状態の列の `data.record.upsert`）の入れる値の状態の列に、最初の状態でない値を
//!   リテラルで書いた。
//! - `data.bulk_upsert` の行（`rows` にリテラルで書いた行の `data`）で、上と同じ決まり（GAP-129）。`id` のある行は
//!   書き換える行なので、状態の列を書くと必ず断られる（取込の更新の行も 1 件の更新と同じ入口を通る）。`id` の無い行は、
//!   同じレコードを探す列（`match_on`）があれば `data.record.upsert` と同じく状態の列（探す列を除く）を止め、無ければ
//!   追加の行なので最初の状態でない値を止める（取込の検証が作成と同じ [`data::FsmInitial::apply`] で断る）。
//!
//! 値の欄の全体や状態の値を参照（`$from` など）で決めるときは確かめない（実行時に data が断る）。材料は保存者の見え方
//! （[`super::CatalogTable::schema`]）で、状態の列が読めなければ状態の列として扱わない（承認と同じ・読めない列の手がかりを
//! 出さない）。テーブルを参照で決めるノードと、材料に無い（見えない）テーブルは照合しない。

use serde_json::{Map, Value};

use super::{Catalog, CatalogTable, ValidationError};
use crate::ir::expr::ValueExpr;
use crate::ir::{Node, WorkflowIr};
use crate::vocab::{label_ja, NodeType};

/// 誤りのコード: 状態の列を、状態を進める以外のブロックで書く（更新の値・足す数・最初の状態でない追加の値）。
pub const STATUS_NOT_WRITABLE: &str = "ir.status_not_writable";

/// 値の欄のリテラルのオブジェクト（全体を参照で決める欄は `None`）。
fn literal_object(v: Option<&Value>) -> Option<Map<String, Value>> {
    match serde_json::from_value::<ValueExpr>(v?.clone()).ok()? {
        ValueExpr::Literal(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// 値が参照・組み立て（形の崩れたものを含む）か（実行まで値が決まらない）。
fn is_expr(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| o.contains_key("$from") || o.contains_key("$template"))
}

/// V4: 状態の列への書き込み（材料が無ければ照合しない）。
pub(super) fn v4(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    if catalog.tables.is_none() {
        return;
    }
    let advance = label_ja(NodeType::DataTransition);
    for node in &ir.nodes {
        let Some(nt) = NodeType::parse(&node.node_type) else {
            continue;
        };
        if !matches!(
            nt,
            NodeType::DataRecordUpdate
                | NodeType::DataRecordUpsert
                | NodeType::DataRecordCreate
                | NodeType::DataBulkUpsert
        ) {
            continue;
        }
        let Some(table) = super::data_keys::table_of(catalog, &node.params) else {
            continue;
        };
        let Some(schema) = table.schema.as_ref() else {
            continue;
        };
        if nt == NodeType::DataBulkUpsert {
            bulk_rows(node, table, schema, errors);
            continue;
        }
        let block = label_ja(nt);
        let mut refuse = |path: String, message: String| {
            errors.push(
                ValidationError::new(STATUS_NOT_WRITABLE, message)
                    .at_node(&node.id)
                    .at_path(path),
            );
        };
        let initial = table.fsm_initial.as_ref();
        let first = first_state(table);
        // 書き換えの値（更新の変更する値・追加か更新の入れる値）と足す数の状態の列。
        let values = match nt {
            NodeType::DataRecordUpdate => Some(("patch", "変更する値")),
            NodeType::DataRecordUpsert => Some(("data", "入れる値")),
            _ => None,
        };
        let match_on = node
            .params
            .get("match_on")
            .and_then(Value::as_str)
            .map(str::trim);
        let mut refused = false;
        for (field, label) in values
            .into_iter()
            .chain((nt != NodeType::DataRecordCreate).then_some(("add", "足す数")))
        {
            let Some(written) = literal_object(node.params.get(field)) else {
                continue;
            };
            for key in written.keys() {
                // 探す列は書き換えでは書かない（`data_upsert`）。追加の値としては下で照らす。
                if Some(key.as_str()) == match_on {
                    continue;
                }
                if data::reject_status_write(schema, std::iter::once(key)).is_ok() {
                    continue;
                }
                refused = true;
                let col = super::data_keys::column_name(schema, key);
                let message = if nt == NodeType::DataRecordUpsert && field == "data" {
                    format!(
                        "{col}は状態の列なので、「{block}」の「{label}」に書けません（同じレコードがあって書き換えるときに必ず断られます）。「{label}」から外してください。追加したレコードは{first}になり、状態を変えるときは「{advance}」のブロックを使います。"
                    )
                } else {
                    format!(
                        "{col}は状態の列なので、「{block}」の「{label}」に書けません。「{label}」から外し、状態を変えるときは「{advance}」のブロックを使ってください。"
                    )
                };
                refuse(format!("/params/{field}/{key}"), message);
            }
        }
        if refused || nt == NodeType::DataRecordUpdate {
            continue;
        }
        // 追加の値に書いた状態（最初の状態でなければ data の作成が断る）。
        let (Some(fi), Some(data)) = (initial, literal_object(node.params.get("data"))) else {
            continue;
        };
        let Some(v) = data.get(&fi.status_field).filter(|v| !is_expr(v)) else {
            continue;
        };
        let mut only = Map::from_iter([(fi.status_field.clone(), v.clone())]);
        if fi.apply(&mut only).is_err() {
            let key = &fi.status_field;
            let col = super::data_keys::column_name(schema, key);
            refuse(
                format!("/params/data/{key}"),
                format!(
                    "追加したレコードの状態は{first}に決まります。{col}は書かないか「{}」にし、ほかの状態にするときは、追加した後に「{advance}」のブロックを使ってください。",
                    fi.initial
                ),
            );
        }
    }
}

/// 最初の状態の言い方（作成時の状態が分かれば名前つき）。
fn first_state(table: &CatalogTable) -> String {
    table.fsm_initial.as_ref().map_or_else(
        || "最初の状態".to_string(),
        |fi| format!("最初の状態「{}」", fi.initial),
    )
}

/// `data.bulk_upsert` の、リテラルで書いた行の状態の列（GAP-129）。行の配列の全体・行・行の `data` を参照で決めるもの
/// と、状態の値を参照で決めるものは照らさない（実行時に data が断る）。
fn bulk_rows(
    node: &Node,
    table: &CatalogTable,
    schema: &data::TableSchema,
    errors: &mut Vec<ValidationError>,
) {
    let rows = node
        .params
        .get("rows")
        .and_then(|v| serde_json::from_value::<ValueExpr>(v.clone()).ok());
    let Some(ValueExpr::Literal(Value::Array(rows))) = rows else {
        return;
    };
    let match_on = node
        .params
        .get("match_on")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|k| !k.is_empty());
    let block = label_ja(NodeType::DataBulkUpsert);
    let advance = label_ja(NodeType::DataTransition);
    let first = first_state(table);
    let mut refuse = |path: String, message: String| {
        errors.push(
            ValidationError::new(STATUS_NOT_WRITABLE, message)
                .at_node(&node.id)
                .at_path(path),
        );
    };
    for (i, row) in rows.iter().enumerate() {
        let n = i + 1;
        let Some(row) = row.as_object().filter(|_| !is_expr(row)) else {
            continue;
        };
        let Some(data) = literal_object(row.get("data")) else {
            continue;
        };
        // id のある行は書き換える行（null は書いていないのと同じ・`DataBulkRow`）。探す列で当たった行も書き換える。
        let by_id = row.get("id").is_some_and(|v| !v.is_null());
        let mut refused = false;
        if by_id || match_on.is_some() {
            for key in data.keys() {
                // 探す列は、id の無い行では追加の値として下で照らす（`data.record.upsert` と同じ）。
                if !by_id && Some(key.as_str()) == match_on {
                    continue;
                }
                if data::reject_status_write(schema, std::iter::once(key)).is_ok() {
                    continue;
                }
                refused = true;
                let col = super::data_keys::column_name(schema, key);
                let message = if by_id {
                    format!(
                        "{col}は状態の列なので、「{block}」の {n} 行目（id のある行）の data に書けません（すでにあるレコードを書き換える行は、状態の列を書くと必ず断られます）。data から外し、状態を変えるときは「{advance}」のブロックを使ってください。"
                    )
                } else {
                    format!(
                        "{col}は状態の列なので、「{block}」の {n} 行目の data に書けません（「同じレコードを探す列」で見つかったレコードを書き換えるときに必ず断られます）。data から外してください。追加したレコードは{first}になり、状態を変えるときは「{advance}」のブロックを使います。"
                    )
                };
                refuse(format!("/params/rows/{i}/data/{key}"), message);
            }
        }
        if refused || by_id {
            continue;
        }
        // 追加の行の状態（最初の状態でなければ data の取込の検証が断る）。
        let Some(fi) = table.fsm_initial.as_ref() else {
            continue;
        };
        let Some(v) = data.get(&fi.status_field).filter(|v| !is_expr(v)) else {
            continue;
        };
        let mut only = Map::from_iter([(fi.status_field.clone(), v.clone())]);
        if fi.apply(&mut only).is_err() {
            let key = &fi.status_field;
            let col = super::data_keys::column_name(schema, key);
            refuse(
                format!("/params/rows/{i}/data/{key}"),
                format!(
                    "「{block}」の {n} 行目で追加するレコードの状態は{first}に決まります。{col}は書かないか「{}」にし、ほかの状態にするときは、追加した後に「{advance}」のブロックを使ってください。",
                    fi.initial
                ),
            );
        }
    }
}

#[cfg(test)]
#[path = "status_writes_tests.rs"]
mod tests;
