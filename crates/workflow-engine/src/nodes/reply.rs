//! workflow.reply の実行（結果を画面に返す・GAP-09・D-28）。
//!
//! 値式を解決し、形（`format`）に合わせて画面に出す 1 節（[`ReplySection`]）に整形して step の出力にする。
//! 能力を呼ばない pure ノードで、secret も解決しない。整形の規則:
//!
//! - text・markdown: 文字列はそのまま、数値・真偽は文字にする（null は空）。オブジェクト・配列は誤り
//!   （「黙って JSON を見せる」をしない）。
//! - table: 行は配列で、各行はオブジェクトか配列。列を指定すれば各セルを位置で引き、無ければ先頭 20 行の
//!   キーを最大 20 列。セルは 500 字・行は 100 行まで（`total_rows` に元の行数）。null は空欄、
//!   オブジェクト・配列は短い JSON。列を省略したときのキーは行の順に集め、1 行の中は名前順になる
//!   （serde_json のオブジェクトの順・見せたい順があれば列を指定する）。
//! - file_link: オブジェクトか配列で 20 件まで。ID が UUID でなければ誤り。同じ ID は 1 件にまとめる
//!   （社内検索の同じファイルの複数の抜粋）。名前は 200 字まで（無ければ「ファイル」）。
//!
//! 1 節は 16KB（UTF-8）まで。text・markdown は文字の境界で切り、table・file_link は末尾から落とし、
//! どちらも `truncated` を立てる。

use std::collections::BTreeSet;

use serde_json::Value;

use crate::control::eval::resolve_deep;
use crate::ir::params::{self, ReplyColumn, ReplyFormat, WorkflowReplyParams};
use crate::run::outcome::{ReplyBody, ReplyFile, ReplySection};
use crate::run::{NodeContext, NodeResult};

use super::resolver::ParamResolver;

/// 1 節の上限（UTF-8 のバイト数）。
pub const MAX_SECTION_BYTES: usize = 16 * 1024;
/// 表の行の上限。
pub const MAX_TABLE_ROWS: usize = 100;
/// 表のセルの上限（文字数）。
pub const MAX_CELL_CHARS: usize = 500;
/// 列を省略したときにキーを集める行数。
const COLUMN_SAMPLE_ROWS: usize = 20;
/// ファイルへのリンクの上限。
pub const MAX_FILES: usize = 20;
/// ファイル名の上限（文字数）。
const MAX_FILE_NAME_CHARS: usize = 200;

/// 整形の誤り（値の形が format と合わない・step の permanent 失敗）。
fn invalid(message: impl Into<String>) -> NodeResult {
    NodeResult::fail("invalid", message, false)
}

/// workflow.reply を実行する（上のモジュールの説明）。
pub(super) fn execute(raw: &Value, ctx: &NodeContext) -> NodeResult {
    let p: WorkflowReplyParams = match params::parse(raw) {
        Ok(p) => p,
        Err(e) => return NodeResult::fail("bad_params", format!("結果の設定が不正: {e}"), false),
    };
    let r = ParamResolver::new(ctx);
    let value = match resolve_deep(&p.value, &r) {
        Ok(v) => v,
        Err(e) => {
            return NodeResult::fail(e.code, format!("結果の値: {}", e.message), false);
        }
    };
    let built = match p.format {
        ReplyFormat::Text => text_of(&value).map(|text| (ReplyBody::Text { text }, false)),
        ReplyFormat::Markdown => text_of(&value).map(|text| (ReplyBody::Markdown { text }, false)),
        ReplyFormat::Table => table_of(&value, p.columns.as_deref()),
        ReplyFormat::FileLink => files_of(&value, &p),
    };
    let (body, truncated) = match built {
        Ok(b) => b,
        Err(message) => return invalid(message),
    };
    let section = fit(ReplySection {
        label: p.label.clone(),
        tone: p.tone.unwrap_or_default(),
        truncated,
        body,
    });
    match serde_json::to_value(&section) {
        Ok(v) => NodeResult::ok(v),
        Err(e) => NodeResult::fail("internal", format!("結果を組めません: {e}"), false),
    }
}

/// text・markdown の文字（オブジェクト・配列は誤り）。
fn text_of(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        Value::Bool(_) | Value::Number(_) => Ok(v.to_string()),
        Value::Array(_) | Value::Object(_) => Err(
            "文章として返す値が、文字ではなくまとまったデータです。項目を path で指すか、形を「表」にしてください"
                .to_string(),
        ),
    }
}

/// セルの表示（null は空欄・オブジェクトは短い JSON・500 字まで）。
fn cell(v: Option<&Value>) -> String {
    let s = match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    clamp_chars(&s, MAX_CELL_CHARS)
}

/// 文字数で切る（文字の境界を守る）。
fn clamp_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// 表（配列の各要素が 1 行）。戻り値の bool は行を切り詰めたか。
fn table_of(v: &Value, columns: Option<&[ReplyColumn]>) -> Result<(ReplyBody, bool), String> {
    let Some(rows) = v.as_array() else {
        return Err(
            "表として返す値が配列ではありません（例: データを調べるの /items）".to_string(),
        );
    };
    if let Some(i) = rows.iter().position(|r| !r.is_object() && !r.is_array()) {
        return Err(format!(
            "表の {} 行目がオブジェクトでも配列でもありません",
            i + 1
        ));
    }
    let total_rows = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let (labels, paths): (Vec<String>, Vec<String>) = match columns {
        Some(cols) => cols
            .iter()
            .map(|c| (c.label.clone(), c.path.clone()))
            .unzip(),
        None => implicit_columns(rows),
    };
    let out: Vec<Vec<String>> = rows
        .iter()
        .take(MAX_TABLE_ROWS)
        .map(|row| paths.iter().map(|p| cell(row.pointer(p))).collect())
        .collect();
    Ok((
        ReplyBody::Table {
            columns: labels,
            rows: out,
            total_rows,
        },
        rows.len() > MAX_TABLE_ROWS,
    ))
}

/// 列を省略したときの列（オブジェクトの行は先頭 20 行のキーを行の順に・配列の行は位置）。
fn implicit_columns(rows: &[Value]) -> (Vec<String>, Vec<String>) {
    let max = crate::ir::params::MAX_REPLY_COLUMNS;
    let mut keys: Vec<String> = Vec::new();
    let mut width = 0;
    for row in rows.iter().take(COLUMN_SAMPLE_ROWS) {
        match row {
            Value::Object(m) => {
                for k in m.keys() {
                    if keys.len() < max && !keys.contains(k) {
                        keys.push(k.clone());
                    }
                }
            }
            Value::Array(a) => width = width.max(a.len().min(max)),
            _ => {}
        }
    }
    if keys.is_empty() {
        return (0..width)
            .map(|i| (format!("{}", i + 1), format!("/{i}")))
            .unzip();
    }
    keys.into_iter()
        .map(|k| {
            let pointer = format!("/{}", k.replace('~', "~0").replace('/', "~1"));
            (k, pointer)
        })
        .unzip()
}

/// ファイルへのリンクの一覧。戻り値の bool は件数を切り詰めたか。
fn files_of(v: &Value, p: &WorkflowReplyParams) -> Result<(ReplyBody, bool), String> {
    let spec = p.file.clone().unwrap_or_default();
    let id_path = spec.id_path.as_deref().unwrap_or("/id");
    let name_path = spec.name_path.as_deref().unwrap_or("/name");
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        _ => {
            return Err(
                "ファイルへのリンクとして返す値が、オブジェクトでも配列でもありません".to_string(),
            )
        }
    };
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    let mut truncated = false;
    for (i, item) in items.iter().enumerate() {
        let id = item
            .pointer(id_path)
            .and_then(Value::as_str)
            .and_then(|s| uuid::Uuid::parse_str(s).ok());
        let Some(id) = id else {
            return Err(format!(
                "{} 件目のファイルの ID（{id_path}）がファイルの ID になっていません",
                i + 1
            ));
        };
        if !seen.insert(id) {
            continue;
        }
        if files.len() == MAX_FILES {
            truncated = true;
            break;
        }
        let name = item
            .pointer(name_path)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map_or_else(
                || "ファイル".to_string(),
                |s| clamp_chars(s, MAX_FILE_NAME_CHARS),
            );
        files.push(ReplyFile { id, name });
    }
    Ok((ReplyBody::FileLink { files }, truncated))
}

/// 直列化した大きさ（UTF-8 のバイト数）。
fn size_of(section: &ReplySection) -> usize {
    serde_json::to_vec(section).map_or(usize::MAX, |v| v.len())
}

/// JSON にしたときの大きさ（UTF-8 のバイト数）。
fn json_len<T: serde::Serialize + ?Sized>(v: &T) -> usize {
    serde_json::to_vec(v).map_or(usize::MAX, |v| v.len())
}

/// 1 節を 16KB に収める（文字は境界で切り・行と件数は末尾から落とす）。
fn fit(mut section: ReplySection) -> ReplySection {
    if size_of(&section) <= MAX_SECTION_BYTES {
        return section;
    }
    section.truncated = true;
    // 中身を空にした節の大きさ（見出し・形の名前・列の見出し）を除いた分が中身に使える。
    let overhead = {
        let mut probe = section.clone();
        match &mut probe.body {
            ReplyBody::Text { text } | ReplyBody::Markdown { text } => text.clear(),
            ReplyBody::Table { rows, .. } => rows.clear(),
            ReplyBody::FileLink { files } => files.clear(),
        }
        size_of(&probe)
    };
    let budget = MAX_SECTION_BYTES.saturating_sub(overhead);
    match &mut section.body {
        ReplyBody::Text { text } | ReplyBody::Markdown { text } => {
            // エスケープで伸びる分（改行・引用符）を見て、収まるまで縮める。
            let mut cut = floor_char_boundary(text, budget);
            loop {
                let size = json_len(&text[..cut]).saturating_sub(2);
                if size <= budget || cut == 0 {
                    break;
                }
                cut = floor_char_boundary(text, cut.saturating_sub(size - budget));
            }
            text.truncate(cut);
        }
        ReplyBody::Table { rows, .. } => {
            while !rows.is_empty() && json_len(rows).saturating_sub(2) > budget {
                rows.pop();
            }
        }
        ReplyBody::FileLink { files } => {
            while !files.is_empty() && json_len(files).saturating_sub(2) > budget {
                files.pop();
            }
        }
    }
    section
}

/// `limit` バイト以下で最も後ろの文字の境界。
fn floor_char_boundary(s: &str, limit: usize) -> usize {
    if limit >= s.len() {
        return s.len();
    }
    let mut i = limit;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::needless_pass_by_value
    )]
    use super::*;
    use crate::ir::params::ReplyTone;
    use serde_json::json;
    use uuid::Uuid;

    fn ctx(outputs: Value) -> NodeContext {
        NodeContext {
            tenant_id: "t1".into(),
            org: "acme".into(),
            run_id: Uuid::nil(),
            step_path: "reply".into(),
            idempotency_key: "k".into(),
            attempt: 1,
            principal: "alice".into(),
            principal_kind: "user".into(),
            input: json!({ "name": "ノート PC", "n": 3 }),
            trigger: Value::Null,
            node_outputs: outputs,
            each: None,
            trace_id: None,
            scope_ceiling: Vec::new(),
            cancel: crate::run::CancelSignal::default(),
        }
    }

    fn run(params: Value, outputs: Value) -> NodeResult {
        execute(&params, &ctx(outputs))
    }

    fn section(res: &NodeResult) -> ReplySection {
        assert!(res.ok, "{:?}", res.error);
        serde_json::from_value(res.output.clone()).unwrap()
    }

    #[test]
    fn text_and_markdown_take_strings_numbers_and_templates() {
        let s = section(&run(
            json!({ "format": "text", "label": "受付", "tone": "success",
                    "value": { "$template": "{name} を受け付けました（{n} 件）",
                               "vars": { "name": { "$from": "input", "path": "/name" },
                                         "n": { "$from": "input", "path": "/n" } } } }),
            Value::Null,
        ));
        assert_eq!(s.label.as_deref(), Some("受付"));
        assert_eq!(s.tone, ReplyTone::Success);
        assert_eq!(
            s.body,
            ReplyBody::Text {
                text: "ノート PC を受け付けました（3 件）".into()
            }
        );
        let s = section(&run(
            json!({ "format": "markdown", "value": { "$from": "input", "path": "/n" } }),
            Value::Null,
        ));
        assert_eq!(s.body, ReplyBody::Markdown { text: "3".into() });
        assert_eq!(s.tone, ReplyTone::Neutral);
        // オブジェクトを文章にしない（黙って JSON を見せない）。
        let res = run(
            json!({ "format": "text", "value": { "$from": "input" } }),
            Value::Null,
        );
        assert!(!res.ok);
        assert_eq!(res.error.unwrap().code, "invalid");
    }

    #[test]
    fn unresolved_reference_fails_the_step() {
        let res = run(
            json!({ "format": "text", "value": { "$from": "nodes.missing.output", "path": "/text" } }),
            json!({}),
        );
        assert!(!res.ok);
        assert_eq!(res.error.unwrap().code, "expr_resolve_error");
    }

    #[test]
    fn table_uses_columns_or_keys_in_order_and_limits_rows_and_cells() {
        let rows: Vec<Value> = (0..120)
            .map(|i| json!({ "clause": format!("第{i}条"), "verdict": null, "quote": { "x": i } }))
            .collect();
        let s = section(&run(
            json!({ "format": "table", "value": { "$from": "nodes.q.output", "path": "/items" },
                    "columns": [{ "label": "条項", "path": "/clause" }, { "label": "判定", "path": "/verdict" },
                                { "label": "引用", "path": "/quote" }] }),
            json!({ "q": { "items": rows } }),
        ));
        let ReplyBody::Table {
            columns,
            rows,
            total_rows,
        } = s.body
        else {
            panic!("表ではない");
        };
        assert_eq!(columns, ["条項", "判定", "引用"]);
        assert_eq!(total_rows, 120);
        assert_eq!(rows.len(), MAX_TABLE_ROWS);
        assert_eq!(rows[1], ["第1条", "", "{\"x\":1}"]);
        assert!(s.truncated);
        // 列を省略すると先頭の行から順にキーを集める（1 行の中は名前順・serde_json のオブジェクトの順）。
        // 長いセルは 500 字で切る。
        let long = "あ".repeat(600);
        let s = section(&run(
            json!({ "format": "table", "value": [{ "b": 1, "a": long }, { "c": true }] }),
            Value::Null,
        ));
        let ReplyBody::Table { columns, rows, .. } = s.body else {
            panic!("表ではない");
        };
        assert_eq!(columns, ["a", "b", "c"]);
        assert_eq!(rows[0][0].chars().count(), MAX_CELL_CHARS);
        assert_eq!(rows[1], ["", "", "true"]);
        // 配列の行は位置の列にする。行がスカラーなら誤り。
        let s = section(&run(
            json!({ "format": "table", "value": [["x", 1], ["y"]] }),
            Value::Null,
        ));
        let ReplyBody::Table { columns, rows, .. } = s.body else {
            panic!("表ではない");
        };
        assert_eq!(columns, ["1", "2"]);
        assert_eq!(rows[1], ["y", ""]);
        assert!(!run(json!({ "format": "table", "value": [1, 2] }), Value::Null).ok);
        assert!(!run(json!({ "format": "table", "value": "x" }), Value::Null).ok);
    }

    #[test]
    fn file_links_dedupe_and_reject_non_uuid_ids() {
        let a = Uuid::new_v4().to_string();
        let b = Uuid::new_v4().to_string();
        let s = section(&run(
            json!({ "format": "file_link", "value": { "$from": "nodes.s.output", "path": "/results" },
                    "file": { "id_path": "/file_id", "name_path": "/file_name" } }),
            json!({ "s": { "results": [
                { "file_id": a, "file_name": "規程.md" },
                { "file_id": a, "file_name": "規程.md" },
                { "file_id": b }
            ] } }),
        ));
        let ReplyBody::FileLink { files } = s.body else {
            panic!("リンクではない");
        };
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "規程.md");
        assert_eq!(files[1].name, "ファイル");
        let res = run(
            json!({ "format": "file_link", "value": [{ "id": "../etc/passwd" }] }),
            Value::Null,
        );
        assert!(!res.ok);
        let many: Vec<Value> = (0..30)
            .map(|_| json!({ "id": Uuid::new_v4().to_string(), "name": "x" }))
            .collect();
        let s = section(&run(
            json!({ "format": "file_link", "value": many }),
            Value::Null,
        ));
        let ReplyBody::FileLink { files } = s.body else {
            panic!("リンクではない");
        };
        assert_eq!(files.len(), MAX_FILES);
        assert!(s.truncated);
    }

    #[test]
    fn sections_fit_in_16kb_at_char_boundaries() {
        let long = "長".repeat(10_000); // 30KB
        let s = section(&run(
            json!({ "format": "markdown", "value": long }),
            Value::Null,
        ));
        assert!(s.truncated);
        assert!(size_of(&s) <= MAX_SECTION_BYTES, "{}", size_of(&s));
        let ReplyBody::Markdown { text } = &s.body else {
            panic!("markdown ではない");
        };
        assert!(text.chars().all(|c| c == '長'));
        assert!(!text.is_empty());
        let rows: Vec<Value> = (0..100)
            .map(|_| json!({ "a": "x".repeat(400), "b": "y".repeat(400) }))
            .collect();
        let s = section(&run(
            json!({ "format": "table", "value": rows }),
            Value::Null,
        ));
        assert!(s.truncated);
        assert!(size_of(&s) <= MAX_SECTION_BYTES);
    }
}
