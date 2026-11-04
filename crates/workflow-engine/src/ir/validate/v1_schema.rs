//! V1: スキーマ（deny-unknown・型・必須）の誤りを、まとめて位置つきで返す（ir.md §8・GAP-35）。
//!
//! IR が型どおりに読めないとき、全体・ブロック・線・きっかけを分けて読み、それぞれの誤りを位置つきで集める
//! （[`crate::ir::schema_faults`]）。ブロックの設定（V1b）と入力項目（V1c）も、読める範囲で同じ回に返す
//! （1 往復で 1 件しか直せないと、AI の作成も人の手直しも往復が増える）。
//!
//! 位置の書き方は後段と同じ: ブロックの誤りは `node_id` とブロックの中の位置（`/params/...`・`/retry/...`）、
//! 線の誤りは `edge`（`from -> to`）と `/edges/<i>/...`、きっかけは `/triggers/<i>/...`、全体は `/<項目>`。
//! 項目が無い誤りは、その項目を入れる入れ物の位置で、項目の名前は文言が言う（ブロックの設定と同じ）。

use serde_json::Value;

use super::{params_message, ValidationError};
use crate::ir::params::{humanize, params_issues};
use crate::ir::schema_faults::{faults, pointer, Fault};
use crate::ir::{Edge, Node, Trigger, WorkflowIr};
use crate::vocab::NodeType;

/// 誤りのコード（V1）。
pub(super) const SCHEMA_VIOLATION: &str = "ir.schema_violation";
/// 1 つの部分（全体・1 つのブロック・1 本の線・1 つのきっかけ）あたりの上限。
const MAX_PER_PART: usize = 10;
/// V1 の誤りの全体の上限（応答を膨らませない。直せば次の保存で続きが出る）。
const MAX_TOTAL: usize = 50;

/// 型どおりに読めない IR の誤り（空にはならない）。
pub(super) fn errors(value: &Value) -> Vec<ValidationError> {
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
    // 全体（ブロック・線・きっかけの配列は要素ごとに下で読むので、ここでは空にして読む）。
    let mut top = obj.clone();
    for key in ["nodes", "edges", "triggers"] {
        if top.get(key).is_some_and(Value::is_array) {
            top.insert(key.into(), Value::Array(Vec::new()));
        }
    }
    for f in faults::<WorkflowIr>(&Value::Object(top), MAX_PER_PART) {
        out.push(at_absolute(&f, ""));
    }
    let items = |key: &str| obj.get(key).and_then(Value::as_array).into_iter().flatten();
    for (i, n) in items("nodes").enumerate() {
        node_errors(i, n, &mut out);
    }
    for (i, e) in items("edges").enumerate() {
        let label = match (
            e.get("from").and_then(Value::as_str),
            e.get("to").and_then(Value::as_str),
        ) {
            (Some(from), Some(to)) => Some(format!("{from} -> {to}")),
            _ => None,
        };
        for f in faults::<Edge>(e, MAX_PER_PART) {
            let err = at_absolute(&f, &format!("/edges/{i}"));
            out.push(match &label {
                Some(l) => err.at_edge(l),
                None => err,
            });
        }
    }
    for (i, t) in items("triggers").enumerate() {
        for f in faults::<Trigger>(t, MAX_PER_PART) {
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
fn node_errors(i: usize, n: &Value, out: &mut Vec<ValidationError>) {
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
    for f in faults::<Node>(n, MAX_PER_PART) {
        out.push(place(
            ValidationError::new(SCHEMA_VIOLATION, humanize(&f.raw)),
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
    for issue in params_issues(nt, &params) {
        out.push(place(
            ValidationError::new(SCHEMA_VIOLATION, params_message(&issue.message)),
            &issue.path,
        ));
    }
}

/// 全体・線・きっかけの誤り（位置は IR の根から）。
fn at_absolute(f: &Fault, prefix: &str) -> ValidationError {
    let path = format!("{prefix}{}", pointer(&f.at));
    let err = ValidationError::new(SCHEMA_VIOLATION, humanize(&f.raw));
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

    #[test]
    fn a_non_object_is_one_error() {
        let errs = validate(&json!([1]), &Catalog::default()).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, SCHEMA_VIOLATION);
        assert_eq!(errs[0].path, None);
    }
}
