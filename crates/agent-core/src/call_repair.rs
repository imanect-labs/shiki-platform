//! ツール呼び出しの回収と修復（小さいモデル向け・docs/agent/small-model-harness.md）。
//!
//! 小さい・オープンなモデルは、構造化されたツール呼び出しの代わりに本文へ呼び出しを書く
//! （Qwen の `<tool_call>`、Gemma 4 の `call:name{…}`）、ツール名を言い換える、数値を文字列で
//! 渡す、といった崩れを起こす。ここでは**解釈が一意に決まる崩れだけ**を直し、何を読み替えたかを
//! 結果に添える（黙って直すと、モデルは同じ崩れを繰り返す）。決まらないものは直さずに、
//! 直し方の分かるエラーとして返す。
//!
//! - 本文の呼び出しは、明示のタグ（`<tool_call>` / `<|tool_call>`）か、**本文全体が**呼び出しの
//!   JSON のときだけ回収する（説明文中の例を実行しない）。名前は提示済みのツールに限る。
//! - ツール名は、接頭辞（`functions.` 等）・大文字小文字・`-`/`_` の違いだけなら読み替える。
//!   1 文字違いは候補が 1 つに決まるときだけ。
//! - 引数は、スキーマの型と食い違うものだけを読み替える（`"3"`→3・`"true"`→true・JSON 文字列の
//!   配列/オブジェクト）。必須の引数が無く、スキーマに無い引数が 1 つだけあるときは、名前の
//!   表記揺れ（`filePath`↔`file_path`）か定番の別名（`file_path`→`path`）に限って読み替える。

use serde_json::{Map, Value};

/// 本文から回収した呼び出し（名前・引数）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextCall {
    pub name: String,
    pub input: Value,
}

/// 本文に書かれたツール呼び出しを回収する。回収できたら（呼び出しを取り除いた本文, 呼び出し）。
pub(crate) fn extract_text_calls(text: &str, known: &[&str]) -> Option<(String, Vec<TextCall>)> {
    let mut calls = Vec::new();
    let mut rest = text.to_string();
    // 1) 明示のタグ（本文のどこにあってもよい）。
    for (open, close) in [
        ("<tool_call>", "</tool_call>"),
        ("<|tool_call>", "<tool_call|>"),
    ] {
        while let Some(start) = rest.find(open) {
            let after = start + open.len();
            let end = rest[after..].find(close).map_or(rest.len(), |e| after + e);
            let inner = rest[after..end].trim().to_string();
            let cut_end = (end + close.len()).min(rest.len());
            rest.replace_range(start..cut_end, "");
            if let Some(c) = parse_call_body(&inner, known) {
                calls.push(c);
            }
        }
    }
    // 1') Gemma 4 が書くことのある `<call:name>{…}`（閉じタグは `</call:name>` / `</call>` / 無し）。
    while let Some(start) = rest.find("<call:") {
        let after = start + "<call:".len();
        let Some(gt) = rest[after..].find('>').map(|i| after + i) else {
            break;
        };
        let name = rest[after..gt].trim().to_string();
        let body_start = rest[gt + 1..].find('{').map(|i| gt + 1 + i);
        let parsed = body_start.and_then(|b| {
            let end = json_object_end(&rest[b..])? + b;
            Some((b, end))
        });
        let Some((b, end)) = parsed else {
            break;
        };
        let mut cut_end = end;
        let tail = &rest[end..];
        let trimmed = tail.trim_start();
        for close in [format!("</call:{name}>"), "</call>".to_string()] {
            if trimmed.starts_with(&close) {
                cut_end = end + (tail.len() - trimmed.len()) + close.len();
                break;
            }
        }
        let input = parse_loose_object(&rest[b..end]);
        let name = normalize_tool_name(&name, known);
        rest.replace_range(start..cut_end, "");
        if let (Some(name), Some(input)) = (name, input) {
            calls.push(TextCall { name, input });
        }
    }
    // 2) 本文全体が呼び出し（JSON・フェンス付き JSON・Gemma の `call:name{…}`）。
    if calls.is_empty() {
        let body = strip_fence(text.trim());
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            let items = match v {
                Value::Array(a) => a,
                other => vec![other],
            };
            for item in items {
                match call_from_json(&item, known) {
                    Some(c) => calls.push(c),
                    None => return None,
                }
            }
            rest = String::new();
        } else if body.starts_with("call:") && !body.contains('\n') {
            if let Some(c) = parse_call_body(body, known) {
                calls.push(c);
                rest = String::new();
            }
        }
    }
    (!calls.is_empty()).then(|| (rest.trim().to_string(), calls))
}

/// `s` の先頭の `{` に対応する `}` の直後の位置（文字列の中の括弧は数えない）。
fn json_object_end(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_fence(s: &str) -> &str {
    let Some(inner) = s.strip_prefix("```") else {
        return s;
    };
    let inner = inner.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
    inner.strip_suffix("```").unwrap_or(inner).trim()
}

/// タグの中身を読む: JSON（`{"name", "arguments"}`）・Qwen3-Coder の XML・Gemma の `call:name{…}`。
fn parse_call_body(inner: &str, known: &[&str]) -> Option<TextCall> {
    let inner = inner.trim();
    if let Ok(v) = serde_json::from_str::<Value>(strip_fence(inner)) {
        return call_from_json(&v, known);
    }
    if let Some(after) = inner.strip_prefix("<function=") {
        // <function=name><parameter=key>value</parameter>…</function>
        let (name, mut body) = after.split_once('>')?;
        let name = normalize_tool_name(name.trim(), known)?;
        let mut args = Map::new();
        while let Some(p) = body.find("<parameter=") {
            let rest = &body[p + "<parameter=".len()..];
            let (key, rest) = rest.split_once('>')?;
            let end = rest.find("</parameter>")?;
            let raw = rest[..end].trim();
            let value = serde_json::from_str::<Value>(raw)
                .ok()
                .filter(|v| !v.is_string())
                .unwrap_or_else(|| Value::String(raw.to_string()));
            args.insert(key.trim().to_string(), value);
            body = &rest[end..];
        }
        return Some(TextCall {
            name,
            input: Value::Object(args),
        });
    }
    if let Some(after) = inner.strip_prefix("call:") {
        let brace = after.find('{')?;
        let name = normalize_tool_name(after[..brace].trim(), known)?;
        let args = after[brace..].trim_end();
        let input = parse_loose_object(args)?;
        return Some(TextCall { name, input });
    }
    None
}

fn call_from_json(v: &Value, known: &[&str]) -> Option<TextCall> {
    let obj = v.as_object()?;
    let name = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .or_else(|| obj.get("function").and_then(|f| f.get("name")))?
        .as_str()?;
    let name = normalize_tool_name(name, known)?;
    let args = obj
        .get("arguments")
        .or_else(|| obj.get("parameters"))
        .or_else(|| obj.get("input"))
        .or_else(|| obj.get("args"))
        .or_else(|| obj.get("function").and_then(|f| f.get("arguments")))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let input = match args {
        Value::String(s) => serde_json::from_str(&s).ok().filter(Value::is_object)?,
        Value::Object(_) => args,
        _ => return None,
    };
    Some(TextCall { name, input })
}

/// Gemma 4 の引数表記（`{path:<|"|>a.py<|"|>,n:3}`）を JSON として読む。
fn parse_loose_object(s: &str) -> Option<Value> {
    let s = s.replace("<|\"|>", "\"");
    if let Ok(v) = serde_json::from_str::<Value>(&s) {
        return v.is_object().then_some(v);
    }
    // 引用符の無いキーに引用符を付ける（文字列の中は触らない）。
    let mut out = String::with_capacity(s.len() + 16);
    let mut in_str = false;
    let mut escaped = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if (c == '{' || c == ',') && i + 1 < chars.len() {
            out.push(c);
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let start = j;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j > start && j < chars.len() && chars[j] == ':' {
                out.push('"');
                out.extend(&chars[start..j]);
                out.push('"');
                i = j;
                continue;
            }
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    serde_json::from_str::<Value>(&out)
        .ok()
        .filter(Value::is_object)
}

/// ツール名を提示済みの名前へ寄せる（決まらなければ `None`）。
pub(crate) fn normalize_tool_name(name: &str, known: &[&str]) -> Option<String> {
    if known.contains(&name) {
        return Some(name.to_string());
    }
    let mut n = name.trim().trim_matches('`').trim_matches('"');
    for prefix in [
        "functions.",
        "functions:",
        "function.",
        "default_api.",
        "tool.",
        "tools.",
    ] {
        if let Some(r) = n.strip_prefix(prefix) {
            n = r;
        }
    }
    let canon = |s: &str| s.to_ascii_lowercase().replace('-', "_");
    let target = canon(n);
    if let Some(k) = known.iter().find(|k| canon(k) == target) {
        return Some((*k).to_string());
    }
    // 1 文字違い（候補が 1 つに決まるときだけ）。短い名前は取り違えやすいので対象外。
    if target.chars().count() < 6 {
        return None;
    }
    let close: Vec<&&str> = known
        .iter()
        .filter(|k| edit_distance(&canon(k), &target) <= 1)
        .collect();
    (close.len() == 1).then(|| (*close[0]).to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// 引数の修復の結果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ArgRepair {
    /// そのまま（または読み替えた）引数で実行する。`notes` は読み替えの説明（空なら読み替え無し）。
    Ok { input: Value, notes: Vec<String> },
    /// 引数がオブジェクトとして読めない（実行しない）。モデルへ返すエラー文。
    Invalid(String),
}

/// 定番の別名（モデルが付けがちな名前 → スキーマの名前）。
const ALIASES: &[(&str, &str)] = &[
    ("file_path", "path"),
    ("filepath", "path"),
    ("file", "path"),
    ("filename", "path"),
    ("file_name", "path"),
    ("q", "query"),
    ("search", "query"),
    ("text", "content"),
    ("body", "content"),
    ("cmd", "command"),
];

/// スキーマに合わせて引数を読み替える。
pub(crate) fn repair_args(input: Value, schema: &Value) -> ArgRepair {
    let mut notes = Vec::new();
    let mut input = match input {
        Value::String(raw) => {
            if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&repair_json_text(&raw))
            {
                notes.push("引数が JSON の文字列で届いたため、オブジェクトとして読みました".into());
                v
            } else {
                let head: String = raw.chars().take(300).collect();
                return ArgRepair::Invalid(format!(
                    "引数を JSON オブジェクトとして読めなかったため、実行していません。受け取った引数の\
                     先頭: {head}\n引数は JSON オブジェクトで、スキーマに従って渡してください: {schema}"
                ));
            }
        }
        other => other,
    };
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return ArgRepair::Ok { input, notes };
    };
    let Some(obj) = input.as_object_mut() else {
        return ArgRepair::Ok { input, notes };
    };
    // 名前の読み替え（必須が欠け、スキーマに無い引数が 1 つだけのとき）。
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| !obj.contains_key(*k))
        .collect();
    let unknown: Vec<String> = obj
        .keys()
        .filter(|k| !props.contains_key(k.as_str()))
        .cloned()
        .collect();
    if missing.len() == 1 && unknown.len() == 1 {
        let (from, to) = (unknown[0].as_str(), missing[0]);
        let canon = |s: &str| s.to_ascii_lowercase().replace(['_', '-'], "");
        let alias = ALIASES
            .iter()
            .any(|(a, b)| canon(a) == canon(from) && *b == to);
        if canon(from) == canon(to) || alias {
            if let Some(v) = obj.remove(from) {
                obj.insert(to.to_string(), v);
                notes.push(format!("引数 `{from}` を `{to}` として読みました"));
            }
        }
    }
    for (key, spec) in props {
        let Some(v) = obj.get_mut(key) else { continue };
        let Some(ty) = spec.get("type").and_then(Value::as_str) else {
            continue;
        };
        if let Some(fixed) = coerce(v, ty) {
            notes.push(format!("引数 `{key}` を {ty} として読みました"));
            *v = fixed;
        }
    }
    ArgRepair::Ok { input, notes }
}

/// 型の食い違いを直した値（直せない・直す必要が無ければ `None`）。
fn coerce(v: &Value, ty: &str) -> Option<Value> {
    match (ty, v) {
        ("integer", Value::String(s)) => s.trim().parse::<i64>().ok().map(Value::from),
        ("integer", Value::Number(n)) if n.as_i64().is_none() => {
            // 3.0 → 3（端数のあるものは直さない）。
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15)
                .map(|f| Value::from(f as i64))
        }
        ("number", Value::String(s)) => s.trim().parse::<f64>().ok().map(Value::from),
        ("boolean", Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        ("array", Value::String(s)) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_array),
        ("object", Value::String(s)) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object),
        ("string", Value::Number(n)) => Some(Value::String(n.to_string())),
        ("string", Value::Bool(b)) => Some(Value::String(b.to_string())),
        _ => None,
    }
}

/// 崩れた JSON 文字列の、解釈が一意な直し（フェンス・前後の地の文・末尾のカンマ）。
pub(crate) fn repair_json_text(raw: &str) -> String {
    let s = strip_fence(raw.trim());
    let (Some(a), Some(b)) = (s.find('{'), s.rfind('}')) else {
        return s.to_string();
    };
    if b < a {
        return s.to_string();
    }
    let body = &s[a..=b];
    // 文字列の外の `,}` / `,]` を落とす。
    let mut out = String::with_capacity(body.len());
    let mut in_str = false;
    let mut escaped = false;
    let chars: Vec<char> = body.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KNOWN: &[&str] = &["read_file", "edit_file", "run_tests", "list"];

    #[test]
    fn hermes_tag_calls_are_recovered_and_removed_from_text() {
        let text = "ファイルを読みます。\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.py\"}}\n</tool_call>";
        let (rest, calls) = extract_text_calls(text, KNOWN).unwrap();
        assert_eq!(rest, "ファイルを読みます。");
        assert_eq!(
            calls,
            vec![TextCall {
                name: "read_file".into(),
                input: json!({"path": "a.py"})
            }]
        );
    }

    #[test]
    fn qwen_xml_and_gemma_calls_are_recovered() {
        let xml = "<tool_call><function=read_file><parameter=path>src/a.py</parameter><parameter=start_line>10</parameter></function></tool_call>";
        let (_, c) = extract_text_calls(xml, KNOWN).unwrap();
        assert_eq!(c[0].input, json!({"path": "src/a.py", "start_line": 10}));
        let gemma = "<|tool_call>call:edit_file{path:<|\"|>a.py<|\"|>,old_string:<|\"|>x = 1<|\"|>,new_string:<|\"|>x = 2<|\"|>}<tool_call|>";
        let (_, c) = extract_text_calls(gemma, KNOWN).unwrap();
        assert_eq!(c[0].name, "edit_file");
        assert_eq!(
            c[0].input,
            json!({"path": "a.py", "old_string": "x = 1", "new_string": "x = 2"})
        );
    }

    #[test]
    fn gemma_call_tag_with_json_body_is_recovered() {
        // 実測（Gemma 4 31B・OpenRouter）: 計画の本文に続けて `<call:plan>{…}` を書いて終わった。
        let text = "集計します。\n\n<call:plan>{\n  \"subtasks\": [{\"status\": \"todo\", \"title\": \"読む {全部}\"}]\n}</call:plan>";
        let (rest, calls) = extract_text_calls(text, &["plan", "read_report"]).unwrap();
        assert_eq!(rest, "集計します。");
        assert_eq!(calls[0].name, "plan");
        assert_eq!(calls[0].input["subtasks"][0]["title"], "読む {全部}");
        // 閉じタグが無くても読む。
        let (_, calls) =
            extract_text_calls("<call:read_report>{\"id\": \"2025-04\"}", &["read_report"])
                .unwrap();
        assert_eq!(calls[0].input, json!({"id": "2025-04"}));
    }

    #[test]
    fn whole_message_json_is_a_call_but_examples_in_prose_are_not() {
        let fenced = "```json\n{\"name\": \"run_tests\", \"arguments\": {}}\n```";
        assert_eq!(
            extract_text_calls(fenced, KNOWN).unwrap().1[0].name,
            "run_tests"
        );
        let prose = "次のように呼べます: {\"name\": \"run_tests\", \"arguments\": {}}";
        assert!(extract_text_calls(prose, KNOWN).is_none());
        // 提示していないツールは回収しない。
        assert!(extract_text_calls("{\"name\": \"rm_rf\", \"arguments\": {}}", KNOWN).is_none());
    }

    #[test]
    fn tool_names_are_normalized_only_when_unambiguous() {
        assert_eq!(
            normalize_tool_name("functions.read_file", KNOWN).as_deref(),
            Some("read_file")
        );
        assert_eq!(
            normalize_tool_name("Read-File", KNOWN).as_deref(),
            Some("read_file")
        );
        assert_eq!(
            normalize_tool_name("read_files", KNOWN).as_deref(),
            Some("read_file")
        );
        assert_eq!(normalize_tool_name("delete_all", KNOWN), None);
        // 短い名前の 1 文字違いは寄せない。
        assert_eq!(normalize_tool_name("lost", KNOWN), None);
    }

    #[test]
    fn args_are_coerced_to_schema_types_with_notes() {
        let schema = json!({"type": "object", "properties": {
            "path": {"type": "string"}, "start_line": {"type": "integer"},
            "all": {"type": "boolean"}, "ids": {"type": "array"}
        }, "required": ["path"]});
        let ArgRepair::Ok { input, notes } = repair_args(
            json!({"file_path": "a.py", "start_line": "10", "all": "true", "ids": "[1,2]"}),
            &schema,
        ) else {
            panic!("ok")
        };
        assert_eq!(
            input,
            json!({"path": "a.py", "start_line": 10, "all": true, "ids": [1, 2]})
        );
        assert_eq!(notes.len(), 4);
        // 直す必要が無ければ注記も無い。
        let ArgRepair::Ok { notes, .. } = repair_args(json!({"path": "a"}), &schema) else {
            panic!("ok")
        };
        assert!(notes.is_empty());
    }

    #[test]
    fn unreadable_arguments_are_reported_not_replaced_with_empty_object() {
        let schema = json!({"type": "object", "properties": {"path": {"type": "string"}}});
        match repair_args(Value::String("{\"path\": \"a.py\"".into()), &schema) {
            ArgRepair::Invalid(msg) => {
                assert!(msg.contains("実行していません") && msg.contains("a.py"))
            }
            ArgRepair::Ok { .. } => panic!("must not run"),
        }
        // 末尾のカンマ・フェンスだけの崩れは直す。
        let ArgRepair::Ok { input, .. } = repair_args(
            Value::String("```json\n{\"path\": \"a.py\",}\n```".into()),
            &schema,
        ) else {
            panic!("ok")
        };
        assert_eq!(input, json!({"path": "a.py"}));
    }
}
