//! 保存は止めず、有効化と実行の前に止める条件（D-40・D-43・D-83）。
//!
//! - **必須の欄が空**（[`REQUIRED_FIELDS`] の表と、`control.branch` の条件の調べる値）: 書きかけでも保存は
//!   できるように、保存時は path つきの警告（[`REQUIRED_TEXT_EMPTY`]）にとどめる。空のまま動かすと、空の依頼で
//!   AI や検索を呼ぶ・いつも同じ出口へ進む・リストでない値を繰り返そうとして失敗する、などになるので、有効化
//!   （[`enable_blockers`]）と対話の実行（[`run_blockers`]）の前に止める。置いたばかりのブロックは、選ぶ欄も
//!   空の文字（`""`）で置く（エディタの既定・D-83）。
//! - **schedule / event で動く IR の、folder を省略した `storage.write`・`csv.write`**: 省略はルートへの保存で、
//!   schedule / event の run はワークフロー主体で動くので、ルートには書けない（フォルダの委譲でも救えない）。
//!   有効化の時点で止め、保存先のフォルダを決めさせる（[`WRITE_FOLDER_REQUIRED`]）。対話の run は起動した
//!   本人のルートに書けるので止めない。folder を省略した `storage.list` も同じ理由で一覧できないので、有効化の
//!   時点で止める（[`LIST_FOLDER_REQUIRED`]・D-83）。
//!
//! どれも IR だけで決まる純関数。有効化（`RegistrationService::enable`）・同意計画の API・対話の起動
//! （`WorkflowRunLauncher`）が同じ関数を呼ぶ。

use serde_json::Value;

use super::ValidationError;
use crate::ir::{Trigger, WorkflowIr};
use crate::vocab::NodeType;

/// 警告・止める理由のコード: 必須の欄が空（D-43・D-83）。
pub const REQUIRED_TEXT_EMPTY: &str = "ir.required_text_empty";
/// 止める理由のコード: schedule / event で動く IR の保存先フォルダが省略されている（D-40）。
pub const WRITE_FOLDER_REQUIRED: &str = "ir.write_folder_required";
/// 止める理由のコード: schedule / event で動く IR の一覧にするフォルダが省略されている（D-83）。
pub const LIST_FOLDER_REQUIRED: &str = "ir.list_folder_required";

/// 欄の入れ方（止める文言の動詞を分ける・付録 D の N1.11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// 文字を入れる欄（「入れてください」）。
    Text,
    /// 一覧や入力・結果から選ぶ欄（「選んでください」）。
    Pick,
}

/// 必須の欄（ノード種・params の項目・画面の欄の見出し・入れ方）。
///
/// 見出しはエディタの欄の見出しと同じにする（文言が欄を名指しする）。
const REQUIRED_FIELDS: &[(NodeType, &str, &str, Fill)] = &[
    (NodeType::LlmInvoke, "prompt", "AI への指示", Fill::Text),
    (
        NodeType::AgentInvoke,
        "instruction",
        "エージェントへの依頼",
        Fill::Text,
    ),
    (NodeType::RagSearch, "query", "検索する内容", Fill::Text),
    (NodeType::CsvQuery, "sql", "SQL", Fill::Text),
    // 相手を選ぶまでは保存だけ通す（相手の照合は V4・BUG-107）。
    (
        NodeType::WorkflowStart,
        "name",
        "実行するワークフロー",
        Fill::Pick,
    ),
    // 置いたばかりの選ぶ欄・文字の欄（D-83・付録 D の N7 Q3）。
    (NodeType::ControlSwitch, "value", "振り分ける値", Fill::Pick),
    (NodeType::ControlMap, "items", "繰り返すリスト", Fill::Pick),
    (NodeType::AiDecide, "model", "判定用のモデル", Fill::Pick),
    (NodeType::AiDecide, "state", "判定の材料", Fill::Text),
    (NodeType::StorageWrite, "name", "ファイル名", Fill::Text),
    (NodeType::StorageWrite, "content", "内容", Fill::Text),
    (NodeType::CsvWrite, "name", "ファイル名", Fill::Text),
    (NodeType::CsvWrite, "content", "CSV の内容", Fill::Text),
    (NodeType::HttpRequest, "url", "URL", Fill::Text),
];

/// 条件で分ける（`control.branch`）の調べる値の見出し。
const BRANCH_LEFT_LABEL: &str = "調べる値";

/// 欄が空か（リテラルの空文字・空白だけ・null、または中身の無い `$template`）。
///
/// 参照（`$from`）は実行時まで分からないので空とみなさない。項目が無いのは V1 の誤り（必須の欠落）で、
/// ここでは数えない（同じ欄に 2 つ出さない）。
fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Object(m) if m.contains_key("$template") => m
            .get("$template")
            .and_then(Value::as_str)
            .is_some_and(|t| t.trim().is_empty()),
        _ => false,
    }
}

/// 空の欄を止める文言（保存の警告・実行と自動実行のオンを止める理由のどれでも通る形・付録 D の N4.10）。
/// 選ぶ欄は「選ぶまで」と言う（N1.11）。
fn blank_message(label: &str, fill: Fill) -> String {
    let until = match fill {
        Fill::Text => "入れるまで",
        Fill::Pick => "選ぶまで",
    };
    format!("「{label}」が空です。{until}、実行も自動実行のオンもできません。")
}

fn blank(node_id: &str, path: String, label: &str, fill: Fill) -> ValidationError {
    ValidationError::new(REQUIRED_TEXT_EMPTY, blank_message(label, fill))
        .at_node(node_id)
        .at_path(path)
}

/// 条件の木（`{cmp}`・`{all}`・`{any}`・`{not}`）の中の、調べる値が空の比較の path。
fn blank_lefts(cond: &Value, at: &str, out: &mut Vec<String>) {
    let Some(o) = cond.as_object() else { return };
    if let Some(cmp) = o.get("cmp") {
        if cmp.get("left").is_some_and(is_blank) {
            out.push(format!("{at}/cmp/left"));
        }
    }
    for key in ["all", "any"] {
        for (i, c) in o
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            blank_lefts(c, &format!("{at}/{key}/{i}"), out);
        }
    }
    if let Some(c) = o.get("not") {
        blank_lefts(c, &format!("{at}/not"), out);
    }
}

/// 必須の欄が空のもの（保存時の警告・有効化と実行の前に止める理由）。
#[must_use]
pub fn required_text_empty(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = Vec::new();
    for node in &ir.nodes {
        let Some(nt) = NodeType::parse(&node.node_type) else {
            continue;
        };
        if nt == NodeType::ControlBranch {
            let mut paths = Vec::new();
            if let Some(cond) = node.params.get("condition") {
                blank_lefts(cond, "/params/condition", &mut paths);
            }
            for path in paths {
                out.push(blank(&node.id, path, BRANCH_LEFT_LABEL, Fill::Pick));
            }
            continue;
        }
        for (_, key, label, fill) in REQUIRED_FIELDS.iter().filter(|(t, ..)| *t == nt) {
            if node.params.get(*key).is_some_and(is_blank) {
                out.push(blank(&node.id, format!("/params/{key}"), label, *fill));
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
/// 必須の欄が空のもの（D-43・D-83）と、schedule / event で動く IR の保存先フォルダの省略（D-40）・一覧に
/// するフォルダの省略（D-83）。
#[must_use]
pub fn enable_blockers(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = required_text_empty(ir);
    if !runs_unattended(ir) {
        return out;
    }
    for node in &ir.nodes {
        // `"folder": null` も省略と同じ（serde の Option は null を None に読む＝ルート）。
        if !node.params.get("folder").is_none_or(Value::is_null) {
            continue;
        }
        match NodeType::parse(&node.node_type) {
            Some(NodeType::StorageWrite | NodeType::CsvWrite) => out.push(
                ValidationError::new(
                    WRITE_FOLDER_REQUIRED,
                    "保存先のフォルダを指定してください。スケジュール・できごとで動く実行は、省略するとルートに保存しようとして失敗します（フォルダの権限を渡しても保存できません）",
                )
                .at_node(&node.id)
                .at_path("/params/folder"),
            ),
            Some(NodeType::StorageList) => out.push(
                ValidationError::new(
                    LIST_FOLDER_REQUIRED,
                    "「一覧にするフォルダー」を選んでください。自動実行では、ドライブのトップは一覧にできません。",
                )
                .at_node(&node.id)
                .at_path("/params/folder"),
            ),
            _ => {}
        }
    }
    out
}

/// 対話の実行の前に止める理由（空なら起動できる）。必須の欄が空のもの（D-43・D-83）。
///
/// 保存先・一覧にするフォルダの省略は止めない（対話の run は起動した本人のルートを使える）。
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

    /// D-83: 置いたばかりの選ぶ欄・文字の欄（調べる値・振り分ける値・繰り返すリスト・判定用のモデルと判定の
    /// 材料・ファイル名と内容・URL）が空なら、保存の警告にとどめ、実行と自動実行のオンの前に止める。
    /// 参照や中身のある値は止めない。文言は欄の見出しを名指しし、選ぶ欄は「選ぶまで」と言う。
    #[test]
    fn freshly_placed_blank_fields_block_run_and_enable() {
        let ir = ir(
            json!([{ "kind": "interactive" }]),
            json!([
                { "id": "branch", "type": "control.branch",
                  "params": { "condition": { "cmp": { "left": "", "op": "eq", "right": "" } } } },
                { "id": "nested", "type": "control.branch",
                  "params": { "condition": { "all": [
                      { "cmp": { "left": { "$from": "input", "path": "/kind" }, "op": "exists" } },
                      { "not": { "cmp": { "left": "  ", "op": "exists" } } }
                  ] } } },
                { "id": "switch", "type": "control.switch", "params": { "value": "", "cases": [] } },
                { "id": "map", "type": "control.map", "params": { "items": "" } },
                { "id": "decide", "type": "ai.decide", "params": {
                    "model": "", "state": "",
                    "questions": [{ "key": "q1", "type": "boolean", "instructions": "請求か" }] } },
                { "id": "save", "type": "storage.write",
                  "params": { "name": "", "content": "", "on_name_taken": "add_number" } },
                { "id": "csv", "type": "csv.write", "params": { "name": "", "content": "" } },
                { "id": "http", "type": "http.request", "params": { "url": "" } },
                // 選んだ値・書いた値は止めない。
                { "id": "ok_switch", "type": "control.switch",
                  "params": { "value": { "$from": "input", "path": "/kind" }, "cases": [] } },
                { "id": "ok_save", "type": "storage.write",
                  "params": { "name": "a.md", "content": { "$from": "input", "path": "/body" } } }
            ]),
        );
        let w = required_text_empty(&ir);
        assert_eq!(
            codes(&w),
            vec![
                (
                    REQUIRED_TEXT_EMPTY,
                    Some("branch"),
                    Some("/params/condition/cmp/left")
                ),
                (
                    REQUIRED_TEXT_EMPTY,
                    Some("nested"),
                    Some("/params/condition/all/1/not/cmp/left")
                ),
                (REQUIRED_TEXT_EMPTY, Some("switch"), Some("/params/value")),
                (REQUIRED_TEXT_EMPTY, Some("map"), Some("/params/items")),
                (REQUIRED_TEXT_EMPTY, Some("decide"), Some("/params/model")),
                (REQUIRED_TEXT_EMPTY, Some("decide"), Some("/params/state")),
                (REQUIRED_TEXT_EMPTY, Some("save"), Some("/params/name")),
                (REQUIRED_TEXT_EMPTY, Some("save"), Some("/params/content")),
                (REQUIRED_TEXT_EMPTY, Some("csv"), Some("/params/name")),
                (REQUIRED_TEXT_EMPTY, Some("csv"), Some("/params/content")),
                (REQUIRED_TEXT_EMPTY, Some("http"), Some("/params/url")),
            ]
        );
        assert_eq!(
            w[0].message,
            "「調べる値」が空です。選ぶまで、実行も自動実行のオンもできません。"
        );
        assert_eq!(
            w[6].message,
            "「ファイル名」が空です。入れるまで、実行も自動実行のオンもできません。"
        );
        assert_eq!(run_blockers(&ir).len(), w.len());
        assert_eq!(enable_blockers(&ir).len(), w.len());
    }

    /// D-83: schedule / event で動く IR の、一覧にするフォルダを省略した storage.list は有効化を止める。対話だけの
    /// IR と、フォルダを決めたものは止めない。
    #[test]
    fn unattended_lists_need_a_folder() {
        let nodes = json!([
            { "id": "ls", "type": "storage.list", "params": {} },
            { "id": "ls_null", "type": "storage.list", "params": { "folder": null } },
            { "id": "ls_ok", "type": "storage.list",
              "params": { "folder": "00000000-0000-0000-0000-000000000002" } }
        ]);
        let scheduled = ir(
            json!([{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]),
            nodes.clone(),
        );
        let blockers = enable_blockers(&scheduled);
        assert_eq!(
            codes(&blockers),
            vec![
                (LIST_FOLDER_REQUIRED, Some("ls"), Some("/params/folder")),
                (
                    LIST_FOLDER_REQUIRED,
                    Some("ls_null"),
                    Some("/params/folder")
                ),
            ]
        );
        assert!(blockers[0]
            .message
            .starts_with("「一覧にするフォルダー」を選んでください。"));
        assert!(run_blockers(&scheduled).is_empty());
        assert!(enable_blockers(&ir(json!([{ "kind": "interactive" }]), nodes)).is_empty());
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
