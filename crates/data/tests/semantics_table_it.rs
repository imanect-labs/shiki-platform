//! クエリ条件の 2 値論理の共通表（`semantics_table.json`）を**実 SQL の経路**にかける（監査 B1-12）。
//!
//! 単体テスト（`query/semantics_tests.rs`）は束縛 → 参照評価器で表を確かめる。ここでは同じ表の各行を
//! 実 Postgres 上のテーブルに 1 レコードずつ置き、`run_query`（pivot 索引＋自前オプティマイザの既定経路）と
//! 本体経路（`ReadPath::Body`・切戻し用の v1 方式）の両方で「その行が条件に合うか」を問い、表の `expect` と
//! 一致することを固定する（ワークフローの分岐・参照評価器・SQL の 3 者が同じ行を選ぶ）。
//!
//! 例外（既知の乖離・結果を誤るのではなく入口で 400 にする）: multi_select の部分一致（`contains` /
//! `ends_with`）。部分一致は `searchable` 宣言の列だけに許し、`searchable` は text 型にしか付けられない
//! （`field_key::validate_identity`）ので、実テーブルの multi_select では必ず拒否になる。参照評価器の
//! 単体テストは束縛の検査を通る形（searchable）で列を作るので表の `expect` を返す。拒否であることと、
//! 例外がこの組に限られることを固定する（表の意味論の側を変えるかは設計判断）。

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
    let mut rejected = 0;
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
            if ty == "multi_select" && matches!(op, CondOp::Contains | CondOp::EndsWith) {
                // 既知の例外: searchable を宣言できない型の部分一致は入口で拒否（誤った結果は返さない）。
                for r in [
                    store.run_query(&c, t.id, &q, None).await.map(|_| ()),
                    store
                        .run_query_debug(
                            &c,
                            t.id,
                            &q,
                            &QueryTuning {
                                read_path: ReadPath::Body,
                                ..Default::default()
                            },
                        )
                        .await
                        .map(|_| ()),
                ] {
                    match r {
                        Err(data::DataError::Invalid(m)) if m.contains("searchable") => {}
                        other => failures.push(format!("[例外] {case} → 拒否されない: {other:?}")),
                    }
                }
                rejected += 1;
                checked += 1;
                continue;
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
        "意味論表 {checked} 行を索引経路・本体経路で確認（うち既知の例外 {rejected} 行は拒否）"
    );
    assert_eq!(checked, cases.len());
    assert_eq!(
        rejected, 6,
        "例外は multi_select の contains / ends_with の 6 行だけ"
    );
    assert!(
        failures.is_empty(),
        "実 SQL の経路が共通の意味論表と食い違う:\n{}",
        failures.join("\n")
    );
}
