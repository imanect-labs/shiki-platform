//! 保存は止めず、有効化と実行の前に止める条件（D-40・D-43）。
//!
//! - **必須の文字の欄が空**（`llm.invoke` の prompt・`agent.invoke` の instruction・`rag.search` の query・
//!   `csv.query` の sql）: 書きかけでも保存はできるように、保存時は path つきの警告（[`REQUIRED_TEXT_EMPTY`]）に
//!   とどめる。空のまま動かすと、空の依頼で AI や検索を呼んでしまうので、有効化（[`enable_blockers`]）と
//!   対話の実行（[`run_blockers`]）の前に止める。
//! - **schedule / event で動く IR の、folder を省略した `storage.write`・`csv.write`**: 省略はルートへの保存で、
//!   schedule / event の run はワークフロー主体で動くので、ルートには書けない（フォルダの委譲でも救えない）。
//!   有効化の時点で止め、保存先のフォルダを決めさせる（[`WRITE_FOLDER_REQUIRED`]）。対話の run は起動した
//!   本人のルートに書けるので止めない。
//!
//! どれも IR だけで決まる純関数。有効化（`RegistrationService::enable`）・同意計画の API・対話の起動
//! （`WorkflowRunLauncher`）が同じ関数を呼ぶ。

use serde_json::Value;

use super::ValidationError;
use crate::ir::{Trigger, WorkflowIr};
use crate::vocab::NodeType;

/// 警告・止める理由のコード: 必須の文字の欄が空（D-43）。
pub const REQUIRED_TEXT_EMPTY: &str = "ir.required_text_empty";
/// 止める理由のコード: schedule / event で動く IR の保存先フォルダが省略されている（D-40）。
pub const WRITE_FOLDER_REQUIRED: &str = "ir.write_folder_required";

/// 必須の文字の欄（ノード種・params の項目・画面の欄の名前）。
const REQUIRED_TEXTS: &[(NodeType, &str, &str)] = &[
    (NodeType::LlmInvoke, "prompt", "AI への指示"),
    (NodeType::AgentInvoke, "instruction", "エージェントへの依頼"),
    (NodeType::RagSearch, "query", "検索する内容"),
    (NodeType::CsvQuery, "sql", "SQL"),
];

/// 文字の欄が空か（リテラルの空文字・空白だけ、または中身の無い `$template`）。
///
/// 参照（`$from`）は実行時まで分からないので空とみなさない。項目が無いのは V1 の誤り（必須の欠落）で、
/// ここでは数えない（同じ欄に 2 つ出さない）。
fn is_blank_text(v: &Value) -> bool {
    match v {
        Value::String(s) => s.trim().is_empty(),
        Value::Object(m) if m.contains_key("$template") => m
            .get("$template")
            .and_then(Value::as_str)
            .is_some_and(|t| t.trim().is_empty()),
        _ => false,
    }
}

/// 必須の文字の欄が空のもの（保存時の警告・有効化と実行の前に止める理由）。
#[must_use]
pub fn required_text_empty(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = Vec::new();
    for node in &ir.nodes {
        let Some(nt) = NodeType::parse(&node.node_type) else {
            continue;
        };
        for (_, key, label) in REQUIRED_TEXTS.iter().filter(|(t, _, _)| *t == nt) {
            if node.params.get(*key).is_some_and(is_blank_text) {
                out.push(
                    ValidationError::new(
                        REQUIRED_TEXT_EMPTY,
                        format!(
                            "「{label}」が空です。保存はできますが、入力するまで有効化・実行はできません"
                        ),
                    )
                    .at_node(&node.id)
                    .at_path(format!("/params/{key}")),
                );
            }
        }
    }
    out
}

/// schedule / event のきっかけを持つか（ワークフロー主体で動く run があるか）。
fn runs_unattended(ir: &WorkflowIr) -> bool {
    ir.triggers
        .iter()
        .any(|t| matches!(t, Trigger::Schedule(_) | Trigger::Event(_)))
}

/// 有効化の前に止める理由（空なら有効化できる）。
///
/// 必須の文字の欄が空のもの（D-43）と、schedule / event で動く IR の保存先フォルダの省略（D-40）。
#[must_use]
pub fn enable_blockers(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = required_text_empty(ir);
    if !runs_unattended(ir) {
        return out;
    }
    for node in &ir.nodes {
        if !matches!(
            NodeType::parse(&node.node_type),
            Some(NodeType::StorageWrite | NodeType::CsvWrite)
        ) {
            continue;
        }
        // `"folder": null` も省略と同じ（serde の Option は null を None に読む＝ルート）。
        if node.params.get("folder").is_none_or(Value::is_null) {
            out.push(
                ValidationError::new(
                    WRITE_FOLDER_REQUIRED,
                    "保存先のフォルダを指定してください。スケジュール・できごとで動く実行は、省略するとルートに保存しようとして失敗します（フォルダの権限を渡しても保存できません）",
                )
                .at_node(&node.id)
                .at_path("/params/folder"),
            );
        }
    }
    out
}

/// 対話の実行の前に止める理由（空なら起動できる）。必須の文字の欄が空のもの（D-43）。
///
/// 保存先フォルダの省略は止めない（対話の run は起動した本人のルートに書ける）。
#[must_use]
pub fn run_blockers(ir: &WorkflowIr) -> Vec<ValidationError> {
    required_text_empty(ir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)]
    fn ir(triggers: Value, nodes: Value) -> WorkflowIr {
        WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "triggers": triggers, "nodes": nodes, "edges": []
        }))
        .unwrap()
    }

    /// （コード・ノード id・path）の並び。
    type Located<'a> = (&'a str, Option<&'a str>, Option<&'a str>);

    fn codes(errs: &[ValidationError]) -> Vec<Located<'_>> {
        errs.iter()
            .map(|e| (e.code.as_str(), e.node_id.as_deref(), e.path.as_deref()))
            .collect()
    }

    #[test]
    fn blank_required_texts_are_listed_with_their_path() {
        let ir = ir(
            json!([{ "kind": "interactive" }]),
            json!([
                { "id": "ask", "type": "llm.invoke", "params": { "prompt": "  " } },
                { "id": "agent", "type": "agent.invoke", "params": { "instruction": { "$template": "", "vars": {} } } },
                { "id": "search", "type": "rag.search", "params": { "query": "" } },
                { "id": "sum", "type": "csv.query", "params": { "file": "00000000-0000-0000-0000-000000000001", "sql": "" } },
                // 参照は実行時まで分からない・中身のある文字は空でない。
                { "id": "ok_ref", "type": "llm.invoke", "params": { "prompt": { "$from": "input", "path": "/q" } } },
                { "id": "ok_text", "type": "rag.search", "params": { "query": "規程" } }
            ]),
        );
        let w = required_text_empty(&ir);
        assert_eq!(
            codes(&w),
            vec![
                (REQUIRED_TEXT_EMPTY, Some("ask"), Some("/params/prompt")),
                (
                    REQUIRED_TEXT_EMPTY,
                    Some("agent"),
                    Some("/params/instruction")
                ),
                (REQUIRED_TEXT_EMPTY, Some("search"), Some("/params/query")),
                (REQUIRED_TEXT_EMPTY, Some("sum"), Some("/params/sql")),
            ]
        );
        assert!(w[0].message.contains("「AI への指示」"), "{}", w[0].message);
        // 対話の起動も同じ理由で止める。
        assert_eq!(run_blockers(&ir).len(), 4);
        assert_eq!(enable_blockers(&ir).len(), 4);
    }

    /// D-40: schedule / event で動く IR の保存先フォルダの省略（null も同じ）は有効化を止める。
    /// 対話だけの IR は止めない（本人のルートに書ける）。フォルダを参照で決めるものは止めない。
    #[test]
    fn unattended_writes_need_a_folder() {
        let nodes = json!([
            { "id": "save", "type": "storage.write", "params": { "name": "a.txt", "content": "x" } },
            { "id": "save_null", "type": "storage.write", "params": { "folder": null, "name": "a.txt", "content": "x" } },
            { "id": "export", "type": "csv.write", "params": { "name": "a.csv", "content": "a,b" } },
            { "id": "save_ref", "type": "storage.write",
              "params": { "folder": { "$from": "input", "path": "/folder" }, "name": "a.txt", "content": "x" } },
            { "id": "save_lit", "type": "storage.write",
              "params": { "folder": "00000000-0000-0000-0000-000000000002", "name": "a.txt", "content": "x" } }
        ]);
        let scheduled = ir(
            json!([{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]),
            nodes.clone(),
        );
        assert_eq!(
            codes(&enable_blockers(&scheduled)),
            vec![
                (WRITE_FOLDER_REQUIRED, Some("save"), Some("/params/folder")),
                (
                    WRITE_FOLDER_REQUIRED,
                    Some("save_null"),
                    Some("/params/folder")
                ),
                (
                    WRITE_FOLDER_REQUIRED,
                    Some("export"),
                    Some("/params/folder")
                ),
            ]
        );
        assert!(enable_blockers(&scheduled)[0]
            .message
            .starts_with("保存先のフォルダを指定してください"));
        let on_save = ir(
            json!([{ "kind": "event", "source": "storage.write",
                     "scope": { "folder": "00000000-0000-0000-0000-000000000003" } }]),
            nodes.clone(),
        );
        assert_eq!(enable_blockers(&on_save).len(), 3);
        let interactive = ir(json!([{ "kind": "interactive" }]), nodes);
        assert!(enable_blockers(&interactive).is_empty());
        assert!(run_blockers(&scheduled).is_empty());
    }
}
