//! TS 版（`@imanect/yui`）と同じ結果になるかを、y-ui の `spec-tests/app.json` を写した課題集で確かめる。
//! 課題集は y-ui で `pnpm spec:app` が生成する。TS 版の規則を変えたら、ここに写し直す。

use serde_json::Value;

fn fixtures() -> Value {
    serde_json::from_str(include_str!("fixtures/app.json")).unwrap_or(Value::Null)
}

/// 数値は 1 と 1.0 を同じに扱う（TS は数値を区別しない）。
fn norm(v: &Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map_or(Value::Null, |f| serde_json::json!(f)),
        Value::Array(a) => Value::Array(a.iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), norm(v))).collect()),
        other => other.clone(),
    }
}

#[test]
fn parses_like_typescript() {
    let data = fixtures();
    let cases = data["cases"].as_array().cloned().unwrap_or_default();
    assert!(cases.len() >= 10, "課題集が読めません");
    for c in cases {
        let name = c["name"].as_str().unwrap_or("?");
        let parsed = yui::parse_block(c["source"].as_str().unwrap_or(""), hint(&c));
        let figure = parsed
            .figure
            .map(|f| serde_json::to_value(f).unwrap_or(Value::Null));
        // 操作を持たない種類（bar など）はサーバーでは読まない
        if !matches!(c["figure"]["kind"].as_str(), Some("app" | "ask" | "plan")) {
            assert!(figure.is_none(), "{name}: 操作を持たない種類は読まない");
            continue;
        }
        assert_eq!(
            figure.as_ref().map(norm),
            Some(norm(&c["figure"])),
            "{name}: IR"
        );
        let diags: Vec<Value> = parsed
            .diagnostics
            .iter()
            .map(|d| serde_json::json!({ "line": d.line, "code": d.code }))
            .collect();
        assert_eq!(Value::Array(diags), c["diagnostics"], "{name}: 診断");
        let f = yui::parse_block(c["source"].as_str().unwrap_or(""), hint(&c)).figure;
        let actions =
            f.map(|f| serde_json::to_value(yui::figure_actions(&f)).unwrap_or(Value::Null));
        assert_eq!(actions, Some(c["actions"].clone()), "{name}: ボタン");
    }
}

#[test]
fn validates_like_typescript() {
    let data = fixtures();
    let cases = data["cases"].as_array().cloned().unwrap_or_default();
    for v in data["values"].as_array().cloned().unwrap_or_default() {
        let src = cases.iter().find(|c| c["name"] == v["source"]);
        let fig = src
            .and_then(|c| c["source"].as_str().map(|s| yui::parse_block(s, hint(c))))
            .and_then(|p| p.figure);
        assert!(fig.is_some(), "{}: 図が読めません", v["source"]);
        let Some(fig) = fig else { continue };
        let input = v["input"].as_object().cloned().unwrap_or_default();
        let check =
            yui::validate_action_values(&fig, v["action"].as_str().unwrap_or(""), &input, "ja-JP");
        let got = serde_json::to_value(check).unwrap_or(Value::Null);
        assert_eq!(
            norm(&got),
            norm(&v["expect"]),
            "{} / {}",
            v["source"],
            v["action"]
        );
    }
}

/// フェンスの情報文字列で分かっている種類（` ```ui:app ` など）。
fn hint(c: &serde_json::Value) -> Option<yui::Kind> {
    c["kind"].as_str().and_then(yui::resolve_kind)
}
