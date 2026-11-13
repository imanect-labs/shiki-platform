//! V1: スキーマ（deny-unknown・型・必須）の誤りを、まとめて位置つきで返す（ir.md §8・GAP-35）。
//!
//! IR が型どおりに読めないとき、全体・ブロック・線・きっかけを分けて読み、それぞれの誤りを位置つきで集める
//! （[`crate::ir::schema_faults`]）。ブロックの設定（V1b）と入力項目（V1c）も、読める範囲で同じ回に返す
//! （1 往復で 1 件しか直せないと、AI の作成も人の手直しも往復が増える）。
//!
//! 位置の書き方は後段と同じ: ブロックの誤りは `node_id` とブロックの中の位置（`/params/...`・`/retry/...`）、
//! 線の誤りは `edge`（`from -> to`）と `/edges/<i>/...`、きっかけは `/triggers/<i>/...`、全体は `/<項目>`。
//! 項目が無い誤りは、その項目を入れる入れ物の位置で、項目の名前は文言が言う（ブロックの設定と同じ）。
//!
//! 読めない IR は V7 の上限（ブロック・線の数・大きさ）を通らないので、ここで先に生の値で確かめ、超えたら上限の
//! 誤りだけを返す（何十万個の空のブロックを 1 つずつ読み直さない）。集めるのも、誤りが上限の数に達するか、serde を
//! 通す回数の合計（[`MAX_PARSES`]）が尽きたら止める。

use serde_json::Value;

use super::{params_message, ValidationError};
use crate::ir::params::{expected_values, humanize, params_issues_with};
use crate::ir::patterns::PatternBudget;
use crate::ir::schema_faults::{faults_within, pointer, Budget, Fault};
use crate::ir::{Edge, Node, Trigger, WorkflowIr};
use crate::vocab::NodeType;

/// 誤りのコード（V1）。
pub(super) const SCHEMA_VIOLATION: &str = "ir.schema_violation";
/// 1 つの部分（全体・1 つのブロック・1 本の線・1 つのきっかけ）あたりの上限。
const MAX_PER_PART: usize = 10;
/// V1 の誤りの全体の上限（応答を膨らませない。直せば次の保存で続きが出る）。
const MAX_TOTAL: usize = 50;
/// 全体・ブロック・線・きっかけを合わせて serde を通す回数の上限（1 回の検証で CPU を使い切らない）。
const MAX_PARSES: usize = 2_000;

/// 型どおりに読めない IR の誤り（空にはならない）。
pub(super) fn errors(value: &Value) -> Vec<ValidationError> {
    // 上限（V7 と同じ数）を超える IR は、部分ごとに読まずに上限の誤りだけを返す。
    let limits = super::size_limit_errors(
        value
            .get("nodes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        value
            .get("edges")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        value,
    );
    if !limits.is_empty() {
        return limits;
    }
    let mut out = collect(value);
    if out.is_empty() {
        // 部分ごとに読めて全体で読めないことは無いはずだが、黙って通さない。
        let message = WorkflowIr::from_json(value).err().map_or_else(
            || "値が正しくありません".to_string(),
            |e| humanize(&e.to_string()),
        );
        out.push(ValidationError::new(SCHEMA_VIOLATION, message));
    }
    out.truncate(MAX_TOTAL);
    out
}

fn collect(value: &Value) -> Vec<ValidationError> {
    let Some(obj) = value.as_object() else {
        return vec![ValidationError::new(
            SCHEMA_VIOLATION,
            "ワークフローの定義は JSON のオブジェクトで書いてください",
        )];
    };
    let mut out = Vec::new();
    let mut budget = Budget::new(MAX_PARSES);
    // 全体（ブロック・線・きっかけの配列は要素ごとに下で読むので、ここでは空にして読む）。
    let mut top = obj.clone();
    for key in ["nodes", "edges", "triggers"] {
        if top.get(key).is_some_and(Value::is_array) {
            top.insert(key.into(), Value::Array(Vec::new()));
        }
    }
    for f in faults_within::<WorkflowIr>(&Value::Object(top), MAX_PER_PART, &mut budget) {
        out.push(at_absolute(&f, ""));
    }
    let full =
        |out: &Vec<ValidationError>, budget: &Budget| out.len() >= MAX_TOTAL || budget.exhausted();
    let items = |key: &str| obj.get(key).and_then(Value::as_array).into_iter().flatten();
    // 出力の形の pattern は、読めた IR と同じくワークフローの全体で数える（BUG-226）。
    let mut patterns = PatternBudget::for_save();
    for (i, n) in items("nodes").enumerate() {
        if full(&out, &budget) {
            return out;
        }
        node_errors(i, n, &mut out, &mut budget, &mut patterns);
    }
    for (i, e) in items("edges").enumerate() {
        if full(&out, &budget) {
            return out;
        }
        let label = match (
            e.get("from").and_then(Value::as_str),
            e.get("to").and_then(Value::as_str),
        ) {
            (Some(from), Some(to)) => Some(format!("{from} -> {to}")),
            _ => None,
        };
        for f in faults_within::<Edge>(e, MAX_PER_PART, &mut budget) {
            let err = at_absolute(&f, &format!("/edges/{i}"));
            out.push(match &label {
                Some(l) => err.at_edge(l),
                None => err,
            });
        }
    }
    for (i, t) in items("triggers").enumerate() {
        if full(&out, &budget) {
            return out;
        }
        for f in faults_within::<Trigger>(t, MAX_PER_PART, &mut budget) {
            out.push(at_absolute(&f, &format!("/triggers/{i}")));
        }
    }
    // 入力項目の形（V1c・読めた IR と同じ検査）。
    if let Some(schema) = obj.get("input_schema") {
        out.extend(super::input_schema_errors(schema));
    }
    out
}

/// 1 つのブロックの誤り（ブロックの項目と、読める種類なら設定 V1b）。id が読めればブロックに紐づけ、位置は
/// ブロックの中で言う（後段の誤りと同じ）。読めなければ `/nodes/<i>/...` で言う。
fn node_errors(
    i: usize,
    n: &Value,
    out: &mut Vec<ValidationError>,
    budget: &mut Budget,
    patterns: &mut PatternBudget,
) {
    let id = n.get("id").and_then(Value::as_str);
    let place = |err: ValidationError, inner: &str| match id {
        Some(id) => {
            let err = err.at_node(id);
            if inner.is_empty() {
                err
            } else {
                err.at_path(inner)
            }
        }
        None => err.at_path(format!("/nodes/{i}{inner}")),
    };
    for f in faults_within::<Node>(n, MAX_PER_PART, budget) {
        out.push(place(
            ValidationError::new(SCHEMA_VIOLATION, humanize(&f.raw))
                .with_candidates(expected_values(&f.raw)),
            &pointer(&f.at),
        ));
    }
    let Some(nt) = n
        .get("type")
        .and_then(Value::as_str)
        .and_then(NodeType::parse)
        .filter(|nt| nt.available_stage_a())
    else {
        return;
    };
    let params = n.get("params").cloned().unwrap_or(Value::Null);
    for issue in params_issues_with(nt, &params, patterns) {
        out.push(place(
            ValidationError::new(SCHEMA_VIOLATION, params_message(&issue.message))
                .with_candidates(issue.expected),
            &issue.path,
        ));
    }
}

/// 全体・線・きっかけの誤り（位置は IR の根から）。
fn at_absolute(f: &Fault, prefix: &str) -> ValidationError {
    let path = format!("{prefix}{}", pointer(&f.at));
    let err = ValidationError::new(SCHEMA_VIOLATION, humanize(&f.raw))
        .with_candidates(expected_values(&f.raw));
    if path.is_empty() {
        err
    } else {
        err.at_path(path)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{validate, Catalog};
    use super::*;

    /// 誤りの（ブロック, 線, 位置, 文言）。
    type Found = (Option<String>, Option<String>, Option<String>, String);

    fn found(v: &Value) -> Vec<Found> {
        let mut got: Vec<_> = validate(v, &Catalog::default())
            .unwrap_err()
            .into_iter()
            .map(|e| {
                assert_eq!(e.code, SCHEMA_VIOLATION, "{e:?}");
                (e.node_id, e.edge, e.path, e.message)
            })
            .collect();
        got.sort();
        got
    }

    /// 全体・ブロック・線・きっかけ・ブロックの設定・入力項目の誤りを、1 回でまとめて位置つきで返す（GAP-35）。
    #[test]
    fn schema_errors_are_all_reported_with_their_positions() {
        let ir = json!({
            "ir_version": 1, "name": "wf", "surprise": true,
            "policies": { "run_timeout_sec": "3d" },
            "triggers": [{ "kind": "interactive", "extra": 1 }],
            "input_schema": { "type": "object", "x-order": ["nope"], "properties": {} },
            "nodes": [
                { "id": "read", "type": "storage.read", "params": { "file": "x", "bogus": 1 }, "colour": "red" },
                { "type": "storage.read", "params": {} }
            ],
            "edges": [{ "from": "read", "port": "next", "to": "b" }]
        });
        let s = |x: &str| Some(x.to_string());
        let mut want = vec![
            (
                None,
                None,
                s("/input_schema/x-order/0"),
                "x-order の「nope」は properties にありません".to_string(),
            ),
            (None, None, s("/nodes/1"), "項目 'id' がありません".into()),
            (
                None,
                None,
                s("/policies/run_timeout_sec"),
                "値の型が正しくありません".into(),
            ),
            (
                None,
                None,
                s("/surprise"),
                "項目 'surprise' は使えません".into(),
            ),
            (
                None,
                None,
                s("/triggers/0/extra"),
                "項目 'extra' は使えません".into(),
            ),
            (
                None,
                s("read -> b"),
                s("/edges/0/port"),
                "項目 'port' は使えません".into(),
            ),
            (
                s("read"),
                None,
                s("/colour"),
                "項目 'colour' は使えません".into(),
            ),
            (
                s("read"),
                None,
                s("/params/bogus"),
                "params が不正です: 項目 'bogus' は使えません".into(),
            ),
        ];
        want.sort();
        assert_eq!(found(&ir), want);
    }

    /// 読める IR でも、ブロックの設定の誤りは 1 件で止めず、ブロックごとにまとめて返す（V1b）。
    #[test]
    fn params_errors_of_a_readable_ir_are_all_reported() {
        let ir = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["http.egress"],
            "nodes": [{ "id": "h", "type": "http.request", "params": {
                "url": "https://api.example.com", "method": "FETCH", "bogus": 1 } }],
            "edges": []
        });
        let s = |x: &str| Some(x.to_string());
        assert_eq!(
            found(&ir),
            vec![
                (
                    s("h"),
                    None,
                    s("/params/bogus"),
                    "params が不正です: 項目 'bogus' は使えません".into()
                ),
                (
                    s("h"),
                    None,
                    s("/params/method"),
                    "params が不正です: 選択肢にない値です".into()
                ),
            ]
        );
    }

    /// 入力項目の起動の検査が断る形（`$ref`・`oneOf`）は、値の入らない項目の下でも保存で止める（GAP-35）。
    #[test]
    fn input_schema_shapes_that_cannot_be_checked_are_refused_on_save() {
        let ir = |extra: Value| {
            let mut ir = json!({
                "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
                "input_schema": { "type": "object", "properties": {
                    "title": { "type": "string" },
                    "memo": { "oneOf": [{ "type": "string" }, { "type": "null" }] },
                    "who": { "$ref": "#/defs/user" }
                } },
                "nodes": [], "edges": []
            });
            ir.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            ir
        };
        let s = |x: &str| Some(x.to_string());
        let message = |k: &str| {
            format!("入力項目の形に「{k}」は書けません。使えるのは type・properties・required・items・additionalProperties・enum・const と、範囲・文字数・個数・pattern です")
        };
        let want = vec![
            (
                None,
                None,
                s("/input_schema/properties/memo/oneOf"),
                message("oneOf"),
            ),
            (
                None,
                None,
                s("/input_schema/properties/who/$ref"),
                message("$ref"),
            ),
        ];
        assert_eq!(found(&ir(json!({}))), want);
        // 型どおりに読めない IR でも、同じ回に返す。
        let mut with_bogus = want.clone();
        with_bogus.push((None, None, s("/bogus"), "項目 'bogus' は使えません".into()));
        with_bogus.sort();
        assert_eq!(found(&ir(json!({ "bogus": 1 }))), with_bogus);
    }

    /// 誤りの無い IR の読み方は変わらない（V1 を通った値はそのまま後段へ）。
    #[test]
    fn a_valid_ir_is_read_as_before() {
        let ir = json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "declared_scopes": ["storage.read"],
            "nodes": [{ "id": "a", "type": "storage.read", "params": { "file": "x" } }], "edges": []
        });
        let parsed = validate(&ir, &Catalog::default()).unwrap();
        assert_eq!(parsed, WorkflowIr::from_json(&ir).unwrap());
    }

    /// 上限を超える読めない IR は、ブロックを 1 つずつ読み直さずに上限の誤りだけを返す（何十万個の空のブロック）。
    #[test]
    fn unreadable_irs_over_the_limits_are_not_read_part_by_part() {
        let nodes: Vec<Value> = (0..300_000).map(|_| json!({})).collect();
        let errs = validate(
            &json!({ "ir_version": 1, "name": "wf", "nodes": nodes, "edges": [] }),
            &Catalog::default(),
        )
        .unwrap_err();
        assert_eq!(errs.len(), 1, "{:?}", &errs[..errs.len().min(3)]);
        assert_eq!(errs[0].code, "ir.limit_exceeded");
        assert!(errs[0].message.contains("200"), "{}", errs[0].message);

        let edges: Vec<Value> = (0..600).map(|_| json!({ "from": 1 })).collect();
        let errs = validate(
            &json!({ "ir_version": 1, "name": "wf", "nodes": [], "edges": edges }),
            &Catalog::default(),
        )
        .unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("500"), "{}", errs[0].message);

        // 上限の内でも、誤りは上限の数で止める（読めないきっかけを何万個も並べる）。
        let triggers: Vec<Value> = (0..50_000).map(|_| json!({})).collect();
        let errs = validate(
            &json!({ "ir_version": 1, "name": "wf", "nodes": [], "edges": [], "triggers": triggers }),
            &Catalog::default(),
        )
        .unwrap_err();
        assert!(
            !errs.is_empty() && errs.len() <= MAX_TOTAL,
            "{}",
            errs.len()
        );
        assert!(errs.iter().all(|e| e.code == SCHEMA_VIOLATION));
    }

    #[test]
    fn a_non_object_is_one_error() {
        let errs = validate(&json!([1]), &Catalog::default()).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, SCHEMA_VIOLATION);
        assert_eq!(errs[0].path, None);
    }

    /// 使えない項目・選択肢にない値の誤りには、serde が挙げる候補を AI 向けの詳細として持つ（人向けの文言には
    /// 出さない・T12・HD-2）。型の誤りは候補を持たない。
    #[test]
    fn schema_errors_carry_the_expected_candidates() {
        let ir = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "a", "type": "storage.read", "params": { "file": "x", "bogus": 1 } },
                { "id": "b", "type": "storage.read", "params": { "file": "y" }, "on_error": "explode" }
            ],
            "edges": [{ "from": "a", "port": "out", "to": "b" }]
        });
        let errs = validate(&ir, &Catalog::default()).unwrap_err();
        let at = |path: &str| {
            errs.iter()
                .find(|e| e.path.as_deref() == Some(path))
                .expect("その位置の誤りがある")
        };
        let edge = at("/edges/0/port");
        assert!(edge.candidates.iter().any(|c| c == "from_port"), "{edge:?}");
        assert!(!edge.message.contains("from_port"), "{edge:?}");
        let param = at("/params/bogus");
        assert_eq!(param.node_id.as_deref(), Some("a"));
        assert!(param.candidates.iter().any(|c| c == "file"), "{param:?}");
        let variant = at("/on_error");
        assert!(
            variant.candidates.iter().any(|c| c == "continue"),
            "{variant:?}"
        );
        // 候補は API の応答（JSON）に出さない。
        assert!(serde_json::to_value(edge)
            .unwrap()
            .get("candidates")
            .is_none());
    }
}
