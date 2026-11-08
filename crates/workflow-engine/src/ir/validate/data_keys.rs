//! 同じレコードを探す列と足す数の保存時検証 V4（GAP-16）。
//!
//! - キーで追加か更新（`data.record.upsert`）とまとめて登録（`data.bulk_upsert`）の同じレコードを探す列（`match_on`）は、
//!   テーブルで重複を許さない列（unique）であること。同じ値で並行に追加しても 2 件にならないのは列の一意に頼るため
//!   （重複を許す列だと、同時に動いた 2 つの実行が両方とも追加する）。
//! - 足す数（`data.record.update` と `data.record.upsert` の `add`）の列は、数の列であること。
//!
//! 材料は承認と同じ [`Catalog::tables`]（API 層が保存者の見え方で引く・読めない列は材料に入らない）。読めない列は
//! 無い列と同じ `ir.unknown_field`（同じ文言・data-platform §16.1）。テーブルを参照で決めるノードと、材料に無い
//! （見えない）テーブルは照合しない（見えないテーブルは [`super::exists`] が `ir.unknown_table` で止める）。

use serde_json::Value;

use super::{Catalog, CatalogTable, ValidationError};
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

/// 誤りのコード: 同じレコードを探す列が、重複を許さない列ではない。
pub const MATCH_NOT_UNIQUE: &str = "ir.match_not_unique";
/// 誤りのコード: 足す数の列が、数の列ではない。
pub const ADD_NOT_NUMBER: &str = "ir.add_not_number";
/// 誤りのコード: 無い列か読めない列（承認と同じ）。
const UNKNOWN_FIELD: &str = "ir.unknown_field";

/// 材料のテーブル（リテラルの UUID で選び、保存者に見えるものだけ）。
pub(super) fn table_of<'a>(catalog: &'a Catalog, params: &Value) -> Option<&'a CatalogTable> {
    let raw = params.get("table")?.as_str()?;
    let key = uuid::Uuid::parse_str(raw).ok()?.hyphenated().to_string();
    catalog.tables.as_ref()?.get(&key)
}

/// 誤りの文言での列の言い方。表示名があれば「表示名」（参照名）、無ければ「参照名」（画面は表示名で列を見せ、AI は
/// 参照名で IR を直すので両方を出す）。`schema` は保存者の見え方の定義で、読めない列・無い列・システムの列は参照名だけ。
pub(super) fn column_name(schema: &data::TableSchema, key: &str) -> String {
    match schema
        .field(key)
        .map(|f| f.display_name.trim())
        .filter(|d| !d.is_empty() && *d != key)
    {
        Some(d) => format!("「{d}」（{key}）"),
        None => format!("「{key}」"),
    }
}

/// V4: 同じレコードを探す列と足す数の列（材料が無ければ照合しない）。
pub(super) fn v4(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    if catalog.tables.is_none() {
        return;
    }
    for node in &ir.nodes {
        let Some(nt) = NodeType::parse(&node.node_type) else {
            continue;
        };
        if !matches!(
            nt,
            NodeType::DataRecordUpsert | NodeType::DataRecordUpdate | NodeType::DataBulkUpsert
        ) {
            continue;
        }
        let Some(table) = table_of(catalog, &node.params) else {
            continue;
        };
        if nt != NodeType::DataRecordUpdate {
            let key = node
                .params
                .get("match_on")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|k| !k.is_empty());
            if let Some(key) = key {
                if let Some(e) = check_match(table, key) {
                    errors.push(e.at_node(&node.id).at_path("/params/match_on"));
                }
            }
        }
        if nt == NodeType::DataBulkUpsert {
            continue;
        }
        let Some(add) = node.params.get("add").and_then(Value::as_object) else {
            continue;
        };
        for k in add.keys() {
            // 状態の列は、状態を進めるでだけ変えられる誤りとして status_writes が言う（同じ欄に 2 つ出さない・BUG-251）。
            if table.schema.is_some() && table.status_field.as_deref() == Some(k.as_str()) {
                continue;
            }
            if let Some(e) = check_add(table, k) {
                errors.push(e.at_node(&node.id).at_path(format!("/params/add/{k}")));
            }
        }
    }
}

/// 同じレコードを探す列の照合（無い・読めない列と、重複を許す列）。
fn check_match(table: &CatalogTable, key: &str) -> Option<ValidationError> {
    if !table.fields.contains_key(key) {
        return Some(ValidationError::new(
            UNKNOWN_FIELD,
            format!(
                "「同じレコードを探す列」の「{key}」は、このテーブルに無いか、読めない列です。"
            ),
        ));
    }
    (!table.unique.contains(key)).then(|| {
        ValidationError::new(
            MATCH_NOT_UNIQUE,
            format!(
                "「同じレコードを探す列」には、重複を許さない列を選んでください。「{key}」は同じ値のレコードを複数持てるので、同時に動くと 2 件追加されることがあります。"
            ),
        )
    })
}

/// 足す数の列の照合（無い・読めない列と、数でない列）。
fn check_add(table: &CatalogTable, key: &str) -> Option<ValidationError> {
    match table.fields.get(key).map(String::as_str) {
        None => Some(ValidationError::new(
            UNKNOWN_FIELD,
            format!("「足す数」の「{key}」は、このテーブルに無いか、読めない列です。"),
        )),
        Some("number") => None,
        Some(_) => Some(ValidationError::new(
            ADD_NOT_NUMBER,
            format!("「足す数」の「{key}」は数の列ではありません。数の列を選んでください。"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::json;

    use super::*;

    const ITEMS: &str = "00000000-0000-0000-0000-0000000000c4";
    const HIDDEN: &str = "00000000-0000-0000-0000-0000000000b3";

    fn catalog() -> Catalog {
        let fields = [
            ("sku", "text"),
            ("name", "text"),
            ("stock", "number"),
            ("secret_cost", "number"),
        ];
        let mut table = CatalogTable {
            fields: fields
                .iter()
                .map(|(k, t)| ((*k).to_string(), (*t).to_string()))
                .collect(),
            unique: BTreeSet::from(["sku".to_string()]),
            ..CatalogTable::default()
        };
        // 読めない列は材料に入らない（無い列と同じ）。
        table.fields.remove("secret_cost");
        Catalog {
            tables: Some(BTreeMap::from([(ITEMS.to_string(), table)])),
            ..Catalog::default()
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn ir(nodes: serde_json::Value) -> WorkflowIr {
        WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": nodes, "edges": []
        }))
        .unwrap_or_else(|e| unreachable!("{e}"))
    }

    type Located<'a> = (&'a str, Option<&'a str>, Option<&'a str>);

    fn located(errs: &[ValidationError]) -> Vec<Located<'_>> {
        errs.iter()
            .map(|e| (e.code.as_str(), e.node_id.as_deref(), e.path.as_deref()))
            .collect()
    }

    /// 探す列は重複を許さない列だけ、足す数は数の列だけ。読めない列は無い列と同じ誤り。参照で決めるテーブル・
    /// 見えないテーブル・置いたばかりの空の探す列は照合しない。
    #[test]
    fn match_on_must_be_unique_and_add_must_be_numeric() {
        let ir = ir(json!([
            { "id": "ok", "type": "data.record.upsert",
              "params": { "table": ITEMS, "match_on": "sku", "data": { "sku": "A" }, "add": { "stock": 1 } } },
            { "id": "not_unique", "type": "data.record.upsert",
              "params": { "table": ITEMS, "match_on": "name", "data": { "name": "A" } } },
            { "id": "missing", "type": "data.bulk_upsert",
              "params": { "table": ITEMS.to_uppercase(), "rows": [], "match_on": "nope" } },
            { "id": "add_text", "type": "data.record.update",
              "params": { "table": ITEMS, "record": "r", "add": { "name": 1, "secret_cost": 1 } } },
            { "id": "fresh", "type": "data.record.upsert",
              "params": { "table": ITEMS, "match_on": "", "data": "" } },
            { "id": "hidden", "type": "data.record.upsert",
              "params": { "table": HIDDEN, "match_on": "name", "data": { "name": "A" } } },
            { "id": "dynamic", "type": "data.bulk_upsert",
              "params": { "table": { "$from": "input", "path": "/t" }, "rows": [], "match_on": "name" } },
            { "id": "bulk_ok", "type": "data.bulk_upsert",
              "params": { "table": ITEMS, "rows": [], "match_on": "sku" } }
        ]));
        let mut errors = Vec::new();
        v4(&ir, &catalog(), &mut errors);
        assert_eq!(
            located(&errors),
            vec![
                (
                    MATCH_NOT_UNIQUE,
                    Some("not_unique"),
                    Some("/params/match_on")
                ),
                (UNKNOWN_FIELD, Some("missing"), Some("/params/match_on")),
                (ADD_NOT_NUMBER, Some("add_text"), Some("/params/add/name")),
                (
                    UNKNOWN_FIELD,
                    Some("add_text"),
                    Some("/params/add/secret_cost")
                ),
            ]
        );
        assert!(
            errors[0].message.contains("重複を許さない列"),
            "{}",
            errors[0].message
        );
        let mut none = Vec::new();
        v4(&ir, &Catalog::default(), &mut none);
        assert!(none.is_empty(), "材料が無い構成は照合しない");
    }

    /// BUG-245: 入力の数を引く書き方（入力で負の数を受け取る・script.run で負の数にしてから参照する）は保存の検証を
    /// 通る。文章の組み立てで負の数を作る書き方（`"-{q}"`）は文字になるので保存で止める（emit_workflow の説明文が
    /// 教える書き方と同じ）。
    #[test]
    fn subtracting_an_input_number_saves_without_a_template() {
        let wf = |input_schema: serde_json::Value,
                  nodes: serde_json::Value,
                  edges: serde_json::Value| {
            let ir = json!({
                "ir_version": 1, "name": "stock-out", "triggers": [{ "kind": "interactive" }],
                "input_schema": input_schema, "nodes": nodes, "edges": edges
            });
            let (ir, _) = crate::ir::scopes::complete_declared_scopes(&ir);
            super::super::validate(&ir, &catalog())
        };
        let number =
            |k: &str| json!({ "type": "object", "properties": { k: { "type": "number" } } });
        // 入力で負の数を受け取る（出庫なら -3 を入れる）。
        let by_input = wf(
            number("delta"),
            json!([{ "id": "take", "type": "data.record.update", "params": {
                "table": ITEMS, "record": { "$from": "input", "path": "/id" },
                "add": { "stock": { "$from": "input", "path": "/delta" } } } }]),
            json!([]),
        );
        assert!(by_input.is_ok(), "{by_input:?}");
        // script.run で負の数にしてから参照する（出庫数 3 を -3 にする）。
        let neg = json!({ "id": "neg", "type": "script.run", "params": {
            "source": { "inline": "function main(i) { return { delta: -i.qty }; }" },
            "input": { "qty": { "$from": "input", "path": "/qty" } } } });
        let by_script = wf(
            number("qty"),
            json!([neg, { "id": "take", "type": "data.record.upsert", "params": {
                "table": ITEMS, "match_on": "sku", "data": { "sku": "A" },
                "add": { "stock": { "$from": "nodes.neg.output", "path": "/delta" } } } }]),
            json!([{ "from": "neg", "to": "take" }]),
        );
        assert!(by_script.is_ok(), "{by_script:?}");
        // 文章の組み立ては文字になるので止める。
        let errs = wf(
            number("qty"),
            json!([{ "id": "take", "type": "data.record.update", "params": {
                "table": ITEMS, "record": { "$from": "input", "path": "/id" },
                "add": { "stock": { "$template": "-{q}",
                                    "vars": { "q": { "$from": "input", "path": "/qty" } } } } } }]),
            json!([]),
        )
        .unwrap_err();
        assert_eq!(
            located(&errs),
            vec![(
                "ir.schema_violation",
                Some("take"),
                Some("/params/add/stock")
            )]
        );
    }
}
