//! クエリ条件の 2 値論理の共通表（`semantics_table.json`）を**実 SQL の経路**にかける（監査 B1-12）。
//!
//! 単体テスト（`query/semantics_tests.rs`）は束縛 → 参照評価器で表を確かめる。ここでは同じ表の各行を
//! 実 Postgres 上のテーブルに 1 レコードずつ置き、`run_query`（pivot 索引＋自前オプティマイザの既定経路）と
//! 本体経路（`ReadPath::Body`・切戻し用の v1 方式）の両方で「その行が条件に合うか」を問い、表の `expect` と
//! 一致することを固定する（ワークフローの分岐・参照評価器・SQL の 3 者が同じ行を選ぶ）。
//!
//! multi_select の部分一致（`contains` / `ends_with`）も `searchable` を宣言した列で実行する（要素ごとに
//! `data_index_search` へ載る・query-optimizer.md §3.1「要素のいずれかの部分文字列」・監査 B2-9）。
//! 例外は無い（表の全行が両経路で `expect` と一致する）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

mod common;

use std::collections::BTreeSet;

use common::{create_table, ctx, field, schema, setup, store_with, unique_tenant};
use data::{
    CondOp, Condition, DataQuery, FieldType, QueryTuning, ReadPath, CONDITION_SEMANTICS_TABLE,
};
use serde_json::{json, Value};

/// 表の 1 行の型ごとに、値の列 `v` と行の目印 `case` を持つテーブルを作る。
fn schema_for(ty: &str, cases: &[&Value]) -> data::TableSchema {
    let mut tag = field("case", FieldType::Text);
    tag.indexed = true;
    let mut v = match ty {
        "text" => {
            let mut f = field("v", FieldType::Text);
            f.searchable = true;
            f
        }
        "number" => field("v", FieldType::Number),
        "multi_select" => {
            let mut f = field("v", FieldType::MultiSelect);
            // 要素ごとの部分一致（data_index_search に要素ごとに載る）。
            f.searchable = true;
            // 値と比較値に現れる文字列を選択肢にする（選択肢外の値は書けない）。
            let mut opts = BTreeSet::new();
            for c in cases {
                for k in ["value", "operand"] {
                    match c.get(k) {
                        Some(Value::Array(a)) => {
                            opts.extend(a.iter().filter_map(|x| x.as_str().map(str::to_string)));
                        }
                        Some(Value::String(s)) => {
                            opts.insert(s.clone());
                        }
                        _ => {}
                    }
                }
            }
            f.options = opts.into_iter().collect();
            f
        }
        other => panic!("表の未知の型: {other}"),
    };
    v.indexed = true;
    schema(vec![tag, v])
}

#[tokio::test]
async fn sql_paths_match_the_shared_semantics_table() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let c = ctx(&unique_tenant(), "alice");
    let table: Value = serde_json::from_str(CONDITION_SEMANTICS_TABLE).unwrap();
    let cases: Vec<&Value> = table["cases"].as_array().unwrap().iter().collect();
    assert!(cases.len() >= 60, "表が欠けている: {}", cases.len());

    let mut failures = Vec::new();
    let mut checked = 0;
    let mut partial = 0;
    for ty in ["text", "number", "multi_select"] {
        let rows: Vec<(usize, &Value)> = cases
            .iter()
            .enumerate()
            .filter(|(_, x)| x["type"] == ty)
            .map(|(i, x)| (i, *x))
            .collect();
        let only: Vec<&Value> = rows.iter().map(|(_, x)| *x).collect();
        let t = create_table(&store, &c, &format!("sem_{ty}"), schema_for(ty, &only)).await;
        // 1 行 = 1 レコード（値を省いた行は「値なし」）。
        for (i, case) in &rows {
            let mut data = json!({ "case": i.to_string() });
            if let Some(v) = case.get("value") {
                if !v.is_null() {
                    data["v"] = v.clone();
                }
            }
            store.create_record(&c, t.id, data, None).await.unwrap();
        }
        for (i, case) in &rows {
            let op: CondOp = serde_json::from_value(case["op"].clone()).unwrap();
            let q = DataQuery {
                filter: Some(Condition::All(vec![
                    Condition::cmp("case", CondOp::Eq, Some(json!(i.to_string()))),
                    Condition::cmp("v", op, case.get("operand").cloned()),
                ])),
                ..Default::default()
            };
            let expect = case["expect"].as_bool().unwrap();
            if matches!(op, CondOp::Contains | CondOp::EndsWith) {
                partial += 1;
            }
            let hit = |r: data::QueryResult| !r.items.unwrap_or_default().is_empty();
            match store.run_query(&c, t.id, &q, None).await {
                Ok(r) => {
                    let got = hit(r);
                    if got != expect {
                        failures.push(format!("[index] {case} → {got}"));
                    }
                }
                Err(e) => failures.push(format!("[index] {case} → 拒否: {e}")),
            }
            let body = QueryTuning {
                read_path: ReadPath::Body,
                ..Default::default()
            };
            match store.run_query_debug(&c, t.id, &q, &body).await {
                Ok((r, _)) => {
                    let got = hit(r);
                    if got != expect {
                        failures.push(format!("[body] {case} → {got}"));
                    }
                }
                Err(e) => failures.push(format!("[body] {case} → 拒否: {e}")),
            }
            checked += 1;
        }
    }
    eprintln!(
        "意味論表 {checked} 行を索引経路・本体経路で確認（うち部分一致 {partial} 行・例外なし）"
    );
    assert_eq!(checked, cases.len());
    assert!(partial > 0, "部分一致の行が表から消えた");
    assert!(
        failures.is_empty(),
        "実 SQL の経路が共通の意味論表と食い違う:\n{}",
        failures.join("\n")
    );
}
