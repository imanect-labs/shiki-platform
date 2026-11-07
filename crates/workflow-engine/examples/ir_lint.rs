//! 保存時検証の CLI（評価基盤 `eval/app-builder/` の採点の 1 層目・GAP-30）。
//!
//! 製品の規則を評価側へ写さないため、検証 API（`POST /workflows/validate`）と同じ関数を同じ順で呼ぶ
//! （`complete_declared_scopes` → `validate` → `save_warnings`・`node_ports`）。止める理由は、実行の口と
//! 有効化の口（`POST /workflows/{id}/enable` が 409 で返す）と同じ関数で出す（`run_blockers`・`enable_blockers`）。カタログは
//! `Catalog::default()`（モデル・テーブル・フォルダー・ファイル・相手のワークフローは照合しない。secret と skill は
//! 空の集合と照合する）。
//!
//! 標準入力の JSONL を 1 行ずつ読む（1 行 = `{"id": str, "ir": object}`・空行は読み飛ばす）。1 行ごとに標準出力へ
//! `{"id", "errors", "ignored_by_env", "warnings", "run_blockers", "enable_blockers", "output_paths", "node_ports",
//! "declared_scopes"}` を書く。
//!
//! - `errors`: 検証の誤り（環境に依存するコードを除く）。
//! - `ignored_by_env`: 環境（登録した secret・入れた skill）に依存するコードの誤り（[`IGNORED_BY_ENV`]）。
//! - `warnings`: 保存を止めない警告（`save_warnings`）。
//! - `run_blockers`: 対話の実行の前に止める理由（`run_blockers`）。
//! - `enable_blockers`: 自動実行をオンにする前に止める理由（`enable_blockers`・schedule / event のきっかけで、
//!   保存先や一覧のフォルダを省いたなど。採点はきっかけが schedule・event の課題だけで数える）。
//! - `output_paths`: `$from: nodes.<id>.output` の path の先頭が、そのノードの出力の項目に無い参照（[`output_paths`]・
//!   製品の検証には無い採点の検査。出力の項目はカタログの単一定義 `output_fields`・`present_if` で決める・GAP-112）。
//! - `node_ports`: ノードの出口（検証 API と同じ `node_ports`）。
//! - `declared_scopes`: スコープを補った後の宣言。
//!
//! 誤りがあっても読み進め、`validate` が誤りを返しても IR が読めれば警告・出口・止める理由を出す（検証 API と
//! 同じ）。読めない行は `{"id": 読めた id か null, "input_error": 理由}` を書いて続ける。終了コードは 0
//! （標準入出力の誤りだけを 1 にする）。最後の行に `{"ir_lint": {"crate_version", "schema"}}` を書く。
//!
//! `--catalog` を付けると、入力を読まずに 1 行だけ書く:
//! `{"node_catalog": node_catalog() の全件, "save_warning_codes": SAVE_WARNING_CODES のコード,
//!   "ignored_by_env_codes": [...], "ir_lint": {...}}`（採点側が出力の形と警告の表を手書きしないため）。

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufWriter, Write};

use serde_json::{json, Value};
use workflow_engine::ir::expr::FromRef;
use workflow_engine::ir::{Node, OnError};
use workflow_engine::vocab::{node_catalog, NodeCatalogEntry};
use workflow_engine::{
    complete_declared_scopes, enable_blockers, node_ports, run_blockers, save_warnings, validate,
    Catalog, ValidationError, WorkflowIr, SAVE_WARNING_CODES,
};

/// 環境に依存する誤りのコード。評価の版のサーバと ir_lint ではカタログ（登録した secret・入れた skill）が違うので、
/// 採点の「静的に誤りがある」（S2s）に数えず、別の欄に分ける。
///
/// `ir.unknown_version` は入れない（D-164 (f)）。環境に依存するコードではなく、V0 も HEAD も `MAX_IR_VERSION` が 1
/// なので、ir_version が 0 か 2 以上なら必ず出る静的な誤り（`errors` に出して S2s に数える）。
const IGNORED_BY_ENV: &[&str] = &["ir.unknown_secret", "ir.binding_denied", "ir.unknown_skill"];

/// 出力の形の版（形を変えたら上げる）。2: `enable_blockers` を足した。3: `output_paths` を足した（GAP-112）。
const SCHEMA: u32 = 3;

/// 出力の path の誤りのコード（ir_lint の検査・製品の検証のコードではない・GAP-112）。
const OUTPUT_PATH: &str = "ir_lint.output_path";

fn meta() -> Value {
    json!({ "crate_version": env!("CARGO_PKG_VERSION"), "schema": SCHEMA })
}

/// `--catalog` の 1 行。
fn catalog_line() -> Value {
    let codes: Vec<&str> = SAVE_WARNING_CODES.iter().map(|(_, code)| *code).collect();
    json!({
        "node_catalog": node_catalog(),
        "save_warning_codes": codes,
        "ignored_by_env_codes": IGNORED_BY_ENV,
        "ir_lint": meta(),
    })
}

/// 誤りを、環境に依存しないもの（前）と依存するもの（後）に分ける。
fn split_by_env(errors: Vec<ValidationError>) -> (Vec<ValidationError>, Vec<ValidationError>) {
    errors
        .into_iter()
        .partition(|e| !IGNORED_BY_ENV.contains(&e.code.as_str()))
}

/// 読めた IR の警告・出口・止める理由・出力の path の誤り。
#[derive(Default)]
struct Derived {
    warnings: Vec<ValidationError>,
    ports: BTreeMap<String, Vec<String>>,
    run_blockers: Vec<ValidationError>,
    enable_blockers: Vec<ValidationError>,
    output_paths: Vec<ValidationError>,
}

fn derived(ir: &WorkflowIr, catalog: &Catalog) -> Derived {
    Derived {
        warnings: save_warnings(ir, catalog),
        ports: node_ports(ir),
        run_blockers: run_blockers(ir),
        enable_blockers: enable_blockers(ir),
        output_paths: output_paths(ir),
    }
}

/// JSON Pointer の 1 段（`~` と `/` をエスケープする）。
fn pointer_child(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// 値の中の参照（`$from`）と、その位置。`$template` の vars・条件の値・入れ子のリテラルの中も見る（製品の
/// `input_refs` と同じ範囲・形の崩れた参照は V5 が誤りにするので飛ばす）。
fn value_refs(value: &Value, path: &str, out: &mut Vec<(String, FromRef)>) {
    match value {
        Value::Object(map) => {
            if map.contains_key("$from") {
                if let Ok(from) = serde_json::from_value::<FromRef>(value.clone()) {
                    out.push((path.to_string(), from));
                }
                return;
            }
            for (k, v) in map {
                value_refs(v, &pointer_child(path, k), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                value_refs(v, &pointer_child(path, &i.to_string()), out);
            }
        }
        _ => {}
    }
}

/// JSON Pointer の先頭の段（`/` で始まらない path はそのまま・V5 が形の誤りにする）。空の path は `None`。
fn first_segment(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let Some(rest) = path.strip_prefix('/') else {
        return Some(path.to_string());
    };
    let seg = rest.split('/').next().unwrap_or_default();
    Some(seg.replace("~1", "/").replace("~0", "~"))
}

/// 設定（params）の `present_if` の場所に値があるか（[`workflow_engine::vocab::OutputField::present_if`] の約束:
/// 無い・`null`・`false`・空の文字・空のリスト・空のオブジェクトは「値が無い」。空の `present_if` は常にある）。
fn present(params: &Value, present_if: &str) -> bool {
    if present_if.is_empty() {
        return true;
    }
    match params.pointer(present_if) {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
        Some(_) => true,
    }
}

/// 出力の項目の名前（設定で決まる項目は設定があるときだけ・`on_error: continue` なら `error` も読める）。
fn output_field_names(entry: &NodeCatalogEntry, target: &Node) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = entry
        .output_fields
        .iter()
        .filter(|f| present(&target.params, f.present_if))
        .map(|f| f.name)
        .collect();
    if target.on_error == OnError::Continue {
        names.push("error");
    }
    names
}

/// `$from: nodes.<id>.output` の path の先頭が、そのノードの出力の項目に無い参照（GAP-112）。
///
/// 出力の項目はカタログの `output_fields`（単一定義）で、設定で決まる項目は `present_if` で決める（右パネルの値の
/// 候補と同じ約束）。出力の形の決まらないノード（項目の無い script.run・control.* など）、無いノード（`ir.bad_ref`
/// が言う）、path の無い参照（出力の全体）は見ない。
fn output_paths(ir: &WorkflowIr) -> Vec<ValidationError> {
    let catalog: HashMap<&'static str, NodeCatalogEntry> = node_catalog()
        .into_iter()
        .map(|e| (e.node_type.as_str(), e))
        .collect();
    let nodes: HashMap<&str, &Node> = ir.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut out = Vec::new();
    for node in &ir.nodes {
        let mut found = Vec::new();
        value_refs(&node.params, "/params", &mut found);
        for (at, from) in found {
            let Some(target) = from
                .from
                .strip_prefix("nodes.")
                .and_then(|s| s.strip_suffix(".output"))
                .and_then(|id| nodes.get(id))
            else {
                continue;
            };
            let Some(first) = from.path.as_deref().and_then(first_segment) else {
                continue;
            };
            let Some(entry) = catalog.get(target.node_type.as_str()) else {
                continue;
            };
            if entry.output_fields.is_empty() {
                continue;
            }
            let names = output_field_names(entry, target);
            if !names.contains(&first.as_str()) {
                out.push(
                    ValidationError::new(
                        OUTPUT_PATH,
                        format!(
                            "{}（{}）の出力に /{first} はありません（{}）",
                            target.id,
                            target.node_type,
                            names.join(", ")
                        ),
                    )
                    .at_node(&node.id)
                    .at_path(at),
                );
            }
        }
    }
    out
}

/// 1 つの IR を検証 API と同じ順で検査する。
fn lint(id: &str, raw: &Value) -> Value {
    let catalog = Catalog::default();
    let (ir, _) = complete_declared_scopes(raw);
    let (errors, d) = match validate(&ir, &catalog) {
        Ok(parsed) => (Vec::new(), derived(&parsed, &catalog)),
        // スキーマを通る IR なら、ほかの誤りがあっても警告と出口は出す（検証 API と同じ）。
        Err(errors) => {
            let rest = WorkflowIr::from_json(&ir)
                .map(|parsed| derived(&parsed, &catalog))
                .unwrap_or_default();
            (errors, rest)
        }
    };
    let (errors, ignored) = split_by_env(errors);
    let declared_scopes: Vec<String> = ir
        .get("declared_scopes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": id,
        "errors": errors,
        "ignored_by_env": ignored,
        "warnings": d.warnings,
        "run_blockers": d.run_blockers,
        "enable_blockers": d.enable_blockers,
        "output_paths": d.output_paths,
        "node_ports": d.ports,
        "declared_scopes": declared_scopes,
    })
}

/// 入力の 1 行を処理する（読めない行は `input_error`）。
fn process_line(line: &str) -> Value {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return json!({ "id": null, "input_error": format!("JSON として読めません: {e}") })
        }
    };
    let id = value.get("id").and_then(Value::as_str);
    match (id, value.get("ir")) {
        (Some(id), Some(ir)) => lint(id, ir),
        (None, _) => json!({ "id": null, "input_error": "id（文字列）がありません" }),
        (Some(id), None) => json!({ "id": id, "input_error": "ir がありません" }),
    }
}

fn write_line<W: Write>(out: &mut W, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    out.write_all(b"\n")
}

/// JSONL を読み、1 行ごとに結果を書き、最後に版の行を書く。
fn run<R: BufRead, W: Write>(input: R, mut out: W) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        write_line(&mut out, &process_line(&line))?;
    }
    write_line(&mut out, &json!({ "ir_lint": meta() }))?;
    out.flush()
}

fn main() -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    if std::env::args().skip(1).any(|a| a == "--catalog") {
        write_line(&mut out, &catalog_line())?;
        return out.flush();
    }
    run(io::stdin().lock(), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lint_lines(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        run(input.as_bytes(), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn lint_one(ir: &Value) -> Value {
        let line = json!({ "id": "t", "ir": ir }).to_string();
        let mut lines = lint_lines(&line);
        assert_eq!(lines.len(), 2, "結果 1 行と版の行: {lines:#?}");
        lines.remove(0)
    }

    fn codes(v: &Value, key: &str) -> Vec<String> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["code"].as_str().unwrap().to_string())
            .collect()
    }

    /// BUG-3 の再現（validate/mod.rs の ai_style_mistakes_are_all_reported と同じ IR）。
    fn bug3_mistakes() -> Value {
        json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["storage.write"],
            "nodes": [
                { "id": "ask", "type": "llm.invoke", "params": { "prompt": "要約して" } },
                { "id": "save", "type": "storage.write", "params": {
                    "name": { "$template": "summary-{d}.md", "vars": { "date": "2026-10-25" } },
                    "content": { "$template": "{t}", "vars": { "t": { "$from": "ask" } } } } }
            ],
            "edges": [{ "from": "ask", "from_port": "next", "to": "save" }]
        })
    }

    /// BUG-1 の再現（validate/mod.rs の mustache_template_saves_with_a_warning と同じ IR）。
    fn bug1_mustache() -> Value {
        json!({
            "ir_version": 1, "name": "bug1-mustache", "declared_scopes": [],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "echo", "type": "script.run", "params": {
                "source": { "inline": "function main(i){ return { got: i }; }" },
                "input": { "$template": "件名: {{title}} / 金額: {{amount}} 円",
                    "vars": { "title": { "$from": "input", "path": "/title" },
                              "amount": { "$from": "input", "path": "/amount" } } } } }],
            "edges": []
        })
    }

    #[test]
    fn bug3_mistakes_are_errors() {
        let out = lint_one(&bug3_mistakes());
        let errs = codes(&out, "errors");
        for code in ["ir.unknown_port", "ir.bad_template", "ir.bad_ref"] {
            assert!(errs.iter().any(|c| c == code), "{code}: {out:#}");
        }
        assert!(codes(&out, "ignored_by_env").is_empty(), "{out:#}");
        assert_eq!(out["id"], "t");
        // 誤りがあっても IR が読めれば出口を出す（検証 API と同じ）。
        assert!(out["node_ports"].get("ask").is_some(), "{out:#}");
    }

    #[test]
    fn mustache_is_a_warning() {
        let out = lint_one(&bug1_mustache());
        assert!(codes(&out, "errors").is_empty(), "{out:#}");
        let warns = codes(&out, "warnings");
        assert!(
            warns.iter().any(|c| c == "ir.template_escaped_var"),
            "{out:#}"
        );
        // スコープは補ってから検証する（script.run は宣言が無くても補われる）。
        assert!(out["declared_scopes"].is_array(), "{out:#}");
    }

    #[test]
    fn env_dependent_codes_are_set_aside() {
        let secret = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["http.egress"],
            "nodes": [{ "id": "h", "type": "http.request",
                "params": { "url": "https://api.example.com/x", "secret": { "name": "unknown" } } }],
            "edges": []
        });
        let skill = json!({
            "ir_version": 1, "name": "wf",
            "nodes": [{ "id": "s", "type": "skill.invoke", "params": { "skill": "skill:expense@1" } }],
            "edges": []
        });
        for (ir, code) in [(&secret, "ir.unknown_secret"), (&skill, "ir.unknown_skill")] {
            let out = lint_one(ir);
            assert!(
                codes(&out, "ignored_by_env").iter().any(|c| c == code),
                "{code} は ignored_by_env に入る: {out:#}"
            );
            assert!(
                !codes(&out, "errors").iter().any(|c| c == code),
                "{code} は errors に入らない: {out:#}"
            );
        }
        // ir_version の誤りは環境に依存しない静的な誤り（errors に出る・D-164 (f)）。
        let version = json!({
            "ir_version": 99, "name": "wf", "declared_scopes": [],
            "nodes": [{ "id": "s", "type": "script.run", "params": {
                "source": { "inline": "function main(i){ return i; }" }, "input": {} } }],
            "edges": []
        });
        let out = lint_one(&version);
        assert!(
            codes(&out, "errors")
                .iter()
                .any(|c| c == "ir.unknown_version"),
            "ir.unknown_version は errors に入る: {out:#}"
        );
        assert!(codes(&out, "ignored_by_env").is_empty(), "{out:#}");
        // binding_denied は登録した secret が要るので Catalog::default() では出ない。振り分けだけを確かめる。
        let (errors, ignored) = split_by_env(vec![
            ValidationError::new("ir.binding_denied", "x"),
            ValidationError::new("ir.bad_ref", "y"),
        ]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "ir.bad_ref");
        assert_eq!(ignored.len(), 1);
        assert_eq!(ignored[0].code, "ir.binding_denied");
    }

    #[test]
    fn broken_lines_do_not_stop() {
        let good = json!({ "id": "ok", "ir": bug1_mustache() }).to_string();
        let input = format!("{{not json\n\n{{\"ir\":{{}}}}\n{{\"id\":\"no-ir\"}}\n{good}\n");
        let lines = lint_lines(&input);
        assert_eq!(lines.len(), 5, "{lines:#?}");
        assert!(lines[0]["id"].is_null() && lines[0]["input_error"].is_string());
        assert!(lines[1]["id"].is_null() && lines[1]["input_error"].is_string());
        assert_eq!(lines[2]["id"], "no-ir");
        assert!(lines[2]["input_error"].is_string());
        assert_eq!(lines[3]["id"], "ok");
        assert!(lines[3]["errors"].is_array());
        assert_eq!(lines[4]["ir_lint"]["schema"], SCHEMA);
        assert_eq!(
            lines[4]["ir_lint"]["crate_version"],
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn unreadable_ir_gives_schema_errors_without_ports() {
        let out = lint_one(&json!("not an object"));
        assert!(!codes(&out, "errors").is_empty(), "{out:#}");
        assert_eq!(out["node_ports"], json!({}));
        assert_eq!(out["warnings"], json!([]));
    }

    #[test]
    fn catalog_lists_codes_and_output_fields() {
        let cat = catalog_line();
        let codes: Vec<&str> = cat["save_warning_codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let expected: Vec<&str> = SAVE_WARNING_CODES.iter().map(|(_, c)| *c).collect();
        assert_eq!(codes, expected);
        assert_eq!(cat["ignored_by_env_codes"], json!(IGNORED_BY_ENV));
        let entries = cat["node_catalog"].as_array().unwrap();
        assert_eq!(entries.len(), node_catalog().len());
        let llm = entries
            .iter()
            .find(|e| e["type"] == "llm.invoke")
            .expect("llm.invoke がカタログにある");
        let fields = llm["output_fields"].as_array().unwrap();
        assert!(!fields.is_empty(), "{llm:#}");
        assert!(fields.iter().all(|f| f["name"].is_string()));
    }

    #[test]
    fn output_paths_follow_the_catalog_fields_and_present_if() {
        // llm.invoke の `data` は出力の形（output_schema）を書いたときだけある。`text` はいつもある。
        let ir = |schema: Value, path: &str| {
            json!({
                "ir_version": 1, "name": "wf", "declared_scopes": [],
                "triggers": [{ "kind": "interactive" }],
                "nodes": [
                    { "id": "ask", "type": "llm.invoke", "params": { "prompt": "分類して", "output_schema": schema } },
                    { "id": "s", "type": "script.run", "params": {
                        "source": { "inline": "function main(i){ return i; }" },
                        "input": { "v": { "$template": "{x}", "vars": {
                            "x": { "$from": "nodes.ask.output", "path": path } } } } } }
                ],
                "edges": [{ "from": "ask", "to": "s" }]
            })
        };
        let schema =
            json!({ "type": "object", "properties": { "category": { "type": "string" } } });
        assert!(codes(
            &lint_one(&ir(schema.clone(), "/data/category")),
            "output_paths"
        )
        .is_empty());
        assert!(codes(&lint_one(&ir(schema.clone(), "/text")), "output_paths").is_empty());
        for absent in [json!(null), json!(false), json!(""), json!([]), json!({})] {
            let out = lint_one(&ir(absent.clone(), "/data/category"));
            assert_eq!(
                codes(&out, "output_paths"),
                [OUTPUT_PATH],
                "{absent}: {out:#}"
            );
            assert_eq!(out["output_paths"][0]["node_id"], "s", "{out:#}");
            assert_eq!(
                out["output_paths"][0]["path"], "/params/input/v/vars/x",
                "{out:#}"
            );
        }
        // 出力に無い項目（形の無い llm.invoke の /category）。
        let out = lint_one(&ir(json!(null), "/category"));
        assert_eq!(codes(&out, "output_paths"), [OUTPUT_PATH], "{out:#}");
        // 項目の無いノード（script.run の戻り値）と path の無い参照は見ない。
        let mut whole = ir(schema, "/x");
        whole["nodes"][1]["params"]["input"]["v"]["vars"]["x"] =
            json!({ "$from": "nodes.ask.output" });
        assert!(codes(&lint_one(&whole), "output_paths").is_empty());
    }

    #[test]
    fn output_paths_allow_error_only_with_on_error_continue() {
        let ir = |on_error: &str| {
            json!({
                "ir_version": 1, "name": "wf", "declared_scopes": [],
                "triggers": [{ "kind": "interactive" }],
                "nodes": [
                    { "id": "ask", "type": "llm.invoke", "on_error": on_error, "params": { "prompt": "x" } },
                    { "id": "s", "type": "script.run", "params": {
                        "source": { "inline": "function main(i){ return i; }" },
                        "input": { "e": { "$from": "nodes.ask.output", "path": "/error" } } } }
                ],
                "edges": [{ "from": "ask", "from_port": "error", "to": "s" }]
            })
        };
        assert!(codes(&lint_one(&ir("continue")), "output_paths").is_empty());
        let out = lint_one(&ir("fail_run"));
        assert_eq!(codes(&out, "output_paths"), [OUTPUT_PATH], "{out:#}");
    }

    #[test]
    fn present_follows_the_output_field_contract() {
        let params = json!({ "a": 1, "z": 0, "t": "x", "e": "", "f": false, "n": null,
            "l": [1], "el": [], "o": { "k": 1 }, "eo": {}, "nested": { "x": [0] } });
        for at in ["", "/a", "/z", "/t", "/l", "/o", "/nested/x", "/nested/x/0"] {
            assert!(present(&params, at), "{at}");
        }
        for at in ["/e", "/f", "/n", "/el", "/eo", "/missing", "/nested/y"] {
            assert!(!present(&params, at), "{at}");
        }
    }

    #[test]
    fn node_ports_match_the_validate_api_function() {
        let ir = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": [],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "sw", "type": "control.switch", "params": {
                    "value": { "$from": "input", "path": "/kind" },
                    "cases": [{ "equals": "a", "port": "a" }, { "equals": "b", "port": "b" }] } },
                { "id": "s", "type": "script.run", "on_error": "continue", "params": {
                    "source": { "inline": "function main(i){ return i; }" }, "input": {} } }
            ],
            "edges": [{ "from": "sw", "from_port": "a", "to": "s" }]
        });
        let out = lint_one(&ir);
        let (completed, _) = complete_declared_scopes(&ir);
        let parsed = WorkflowIr::from_json(&completed).unwrap();
        assert_eq!(out["node_ports"], json!(node_ports(&parsed)), "{out:#}");
        assert_eq!(out["node_ports"]["s"], json!(["out", "error"]), "{out:#}");
        assert!(
            out["node_ports"]["sw"].as_array().unwrap().len() >= 2,
            "{out:#}"
        );
    }

    #[test]
    fn run_blockers_come_from_the_launcher_function() {
        // 必須の文字の欄（llm.invoke の prompt）が空の IR は保存できるが、実行の前に止まる（D-43）。
        let ir = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": [],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "ask", "type": "llm.invoke", "params": { "prompt": "" } }],
            "edges": []
        });
        let out = lint_one(&ir);
        let (completed, _) = complete_declared_scopes(&ir);
        let parsed = WorkflowIr::from_json(&completed).unwrap();
        assert!(!run_blockers(&parsed).is_empty());
        assert_eq!(out["run_blockers"], json!(run_blockers(&parsed)), "{out:#}");
        assert!(codes(&out, "errors").is_empty(), "{out:#}");
    }

    #[test]
    fn enable_blockers_come_from_the_enable_function() {
        // スケジュールで動く IR の保存先のフォルダを省くと、保存はできるが自動実行をオンにできない
        // （POST /workflows/{id}/enable が 409・WRITE_FOLDER_REQUIRED）。対話の IR では出ない。
        let scheduled = json!({
            "ir_version": 1, "name": "wf", "declared_scopes": [],
            "triggers": [{ "kind": "schedule", "cron": "0 9 * * 1", "tz": "Asia/Tokyo" }],
            "nodes": [{ "id": "save", "type": "storage.write", "params": {
                "name": "a.md", "content": "x" } }],
            "edges": []
        });
        let out = lint_one(&scheduled);
        let (completed, _) = complete_declared_scopes(&scheduled);
        let parsed = WorkflowIr::from_json(&completed).unwrap();
        assert_eq!(
            out["enable_blockers"],
            json!(enable_blockers(&parsed)),
            "{out:#}"
        );
        assert!(
            codes(&out, "enable_blockers")
                .iter()
                .any(|c| c == "ir.write_folder_required"),
            "{out:#}"
        );
        let mut interactive = scheduled.clone();
        interactive["triggers"] = json!([{ "kind": "interactive" }]);
        let out = lint_one(&interactive);
        assert!(
            !codes(&out, "enable_blockers")
                .iter()
                .any(|c| c == "ir.write_folder_required"),
            "{out:#}"
        );
    }
}
