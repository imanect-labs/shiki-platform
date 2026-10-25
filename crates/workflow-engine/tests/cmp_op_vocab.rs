//! workflow IR の比較演算子（`ir::expr::CmpOp`）と data のクエリ条件の演算子（`data::CondOp`）の
//! 語彙一致（監査 B1-7）。
//!
//! 2 つは別々に定義されている（workflow は ts-rs、data は utoipa で型を生成する）が、LLM・利用者が
//! ワークフローの条件と data ノード・ツールの条件で同じ語彙を書けることが契約（data-platform §16・
//! `semantics_table.json` の共通の意味論表もこの語彙で書く）。生成される型の列挙値（ts-rs の宣言と
//! OpenAPI の enum）を全列挙して突き合わせ、片方だけに演算子を足す・綴りを変えると落ちるようにする。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use ts_rs::TS;
use utoipa::PartialSchema;
use workflow_engine::ir::expr::CmpOp;

/// ts-rs の宣言（`type CmpOp = "eq" | "neq" | …;`）から文字列リテラルを取り出す。
fn ts_literals(decl: &str) -> BTreeSet<String> {
    decl.split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect()
}

/// OpenAPI のスキーマ（`{"type": "string", "enum": [...]}`）の列挙値。
fn openapi_enum() -> BTreeSet<String> {
    let v = serde_json::to_value(data::CondOp::schema()).unwrap();
    v["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("CondOp のスキーマに enum が無い: {v}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn workflow_cmp_op_and_data_cond_op_share_the_vocabulary() {
    let wf = ts_literals(&CmpOp::decl());
    let dt = openapi_enum();
    assert!(wf.len() >= 13, "ts-rs の宣言を読めていない: {wf:?}");
    assert_eq!(
        wf, dt,
        "workflow の CmpOp と data の CondOp の語彙がずれている（片方だけに足した・綴りを変えた）"
    );
    // シリアライズ表現でも往復する（生成型だけでなく実際の serde の綴りも一致）。
    for s in &wf {
        let w: CmpOp = serde_json::from_value(serde_json::json!(s)).unwrap();
        let d: data::CondOp = serde_json::from_value(serde_json::json!(s)).unwrap();
        assert_eq!(
            serde_json::to_value(w).unwrap(),
            serde_json::to_value(d).unwrap()
        );
    }
}
