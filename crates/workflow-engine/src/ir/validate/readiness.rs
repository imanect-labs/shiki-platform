//! 保存は止めず、有効化と実行の前に止める条件（D-40・D-43・D-83）。
//!
//! - **必須の欄が空**（[`REQUIRED_FIELDS`] の表と、業務データ・ファイルを選ぶブロックの [`REQUIRED_VALUES`] の表（項目が
//!   無いのも空・BUG-151）と、`control.branch` の条件の調べる値と、`ai.decide` の質問ごとの
//!   「AI に尋ねること」（D-108）と、出口を分ける `ai.decide` のしきい値（既定を置かない・D-88））: 書きかけでも保存は
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
//! - **中にブロックの無い繰り返し**（`control.map` の領域が空・D-85）: エディタで置いた直後の繰り返しは中が空なので、
//!   保存は止めずに警告（[`EMPTY_REGION`]）にとどめる。空のまま動かすと、1 件目から中の最後のブロックを
//!   いつまでも待つので、有効化と対話の実行の前に止める（D-83 と同じ扱い）。
//!
//! - **値を書く欄が中身の無い入れ物**（`data.record.create` の `data: {}` など・[`EMPTY_CONTAINERS`]）: 保存の注意
//!   （[`EMPTY_VALUES`]）だけにし、有効化も実行も止めない。状態の進め方のあるテーブルに最初の状態だけのレコードを
//!   作るのは正しい使い方で、前から保存されている定義にもある。
//!
//! どれも IR だけで決まる純関数。有効化（`RegistrationService::enable`）・同意計画の API・対話の起動
//! （`WorkflowRunLauncher`）が同じ関数を呼ぶ。

use serde_json::Value;

use super::{block_name, ValidationError};
use crate::ir::{Trigger, WorkflowIr};
use crate::vocab::NodeType;

/// 警告・止める理由のコード: 必須の欄が空（D-43・D-83）。
pub const REQUIRED_TEXT_EMPTY: &str = "ir.required_text_empty";
/// 警告・止める理由のコード: 繰り返し（`control.map`）の中にブロックが無い（D-85）。
pub const EMPTY_REGION: &str = "ir.empty_region";
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
    // スキルを選ぶまでは保存だけ通す（形とインストールの照合は V4・BUG-151）。
    (NodeType::SkillInvoke, "skill", "実行するスキル", Fill::Pick),
];

/// 業務データのブロックのテーブルの欄の見出し。
const TABLE_LABEL: &str = "対象のテーブル";
/// 業務データのブロックのレコードの欄の見出し。
const RECORD_LABEL: &str = "対象のレコード";

/// 必須の値の欄（業務データのブロックと、ファイルを選ぶブロック・D-83 と同じ扱い・BUG-151）。
///
/// 保存の契約（V1）は項目が無くても通す（`ir::params` の `unset`）ので、項目が無いのも空として数える（エディタの値の
/// 欄は、読めない JSON・入れ方の切り替え・数の欄を空にしたときに項目を外す）。中身の無い項目のまとまり・リスト（`{}`・
/// `[]`）は空と数えない（[`EMPTY_CONTAINERS`] の注意にとどめる）。
const REQUIRED_VALUES: &[(NodeType, &str, &str, Fill)] = &[
    (NodeType::StorageRead, "file", "読むファイル", Fill::Pick),
    (
        NodeType::CsvQuery,
        "file",
        "集計する CSV ファイル",
        Fill::Pick,
    ),
    (
        NodeType::CsvPatch,
        "file",
        "編集する CSV ファイル",
        Fill::Pick,
    ),
    (NodeType::CsvPatch, "base_rev", "編集前の版", Fill::Text),
    (
        NodeType::CsvPatch,
        "ops",
        "編集の内容（操作の並び）",
        Fill::Text,
    ),
    (NodeType::DataQuery, "table", TABLE_LABEL, Fill::Pick),
    (NodeType::DataRecordCreate, "table", TABLE_LABEL, Fill::Pick),
    (
        NodeType::DataRecordCreate,
        "data",
        "レコードの値",
        Fill::Text,
    ),
    (NodeType::DataRecordUpdate, "table", TABLE_LABEL, Fill::Pick),
    (
        NodeType::DataRecordUpdate,
        "record",
        RECORD_LABEL,
        Fill::Pick,
    ),
    (
        NodeType::DataRecordUpdate,
        "patch",
        "変更する値",
        Fill::Text,
    ),
    (NodeType::DataRecordDelete, "table", TABLE_LABEL, Fill::Pick),
    (
        NodeType::DataRecordDelete,
        "record",
        RECORD_LABEL,
        Fill::Pick,
    ),
    (NodeType::DataTransition, "table", TABLE_LABEL, Fill::Pick),
    (NodeType::DataTransition, "record", RECORD_LABEL, Fill::Pick),
    (NodeType::DataTransition, "to", "次の状態", Fill::Pick),
    (NodeType::DataBulkUpsert, "table", TABLE_LABEL, Fill::Pick),
    (NodeType::DataBulkUpsert, "rows", "登録する行", Fill::Text),
];

/// 必須の値の欄が空か（項目なし・[`is_blank`]）。
fn is_unset(v: Option<&Value>) -> bool {
    v.is_none_or(is_blank)
}

/// 注意のコード: 値を書く欄が中身の無い項目のまとまり・リスト（保存も実行も止めない）。
pub const EMPTY_VALUES: &str = "ir.empty_values";

/// 値を書く欄が中身の無い入れ物（`{}`・`[]`）のときの注意（ノード種・params の項目・文言）。
///
/// 保存・有効化・実行のどれも止めない。状態の進め方のあるテーブルに最初の状態だけのレコードを作る（`data: {}`）のは
/// 正しい使い方で、前から保存されている定義にもある（子の実行・ミニアプリが固定した版も止めない）。何も書かない・
/// 変えないことを、保存のときに言うだけにする。
const EMPTY_CONTAINERS: &[(NodeType, &str, &str)] = &[
    (
        NodeType::DataRecordCreate,
        "data",
        "「レコードの値」に列がありません。このままでは、どの列にも値を入れずにレコードを追加します。",
    ),
    (
        NodeType::DataRecordUpdate,
        "patch",
        "「変更する値」に列がありません。このままでは、どの列も書き換えません。",
    ),
    (
        NodeType::DataBulkUpsert,
        "rows",
        "「登録する行」に行がありません。このままでは、1 行も登録しません。",
    ),
    (
        NodeType::CsvPatch,
        "ops",
        "「編集の内容（操作の並び）」に操作がありません。このままでは、行も列も変えずに保存し直します。",
    ),
];

/// 値を書く欄が中身の無い入れ物のもの（保存時の注意だけ・[`EMPTY_CONTAINERS`]）。
#[must_use]
pub fn empty_values(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = Vec::new();
    for node in &ir.nodes {
        let Some(nt) = NodeType::parse(&node.node_type) else {
            continue;
        };
        for (_, key, message) in EMPTY_CONTAINERS.iter().filter(|(t, ..)| *t == nt) {
            let empty = match node.params.get(*key) {
                Some(Value::Object(m)) => m.is_empty(),
                Some(Value::Array(a)) => a.is_empty(),
                _ => false,
            };
            if empty {
                out.push(
                    ValidationError::new(EMPTY_VALUES, *message)
                        .at_node(&node.id)
                        .at_path(format!("/params/{key}")),
                );
            }
        }
    }
    out
}

/// 条件で分ける（`control.branch`）の調べる値の見出し。
const BRANCH_LEFT_LABEL: &str = "調べる値";
/// AI で判定（`ai.decide`）の出口を分けるときのしきい値の見出し（既定を置かない・D-88）。
const DECIDE_THRESHOLD_LABEL: &str = "しきい値（AI の自信）";
/// AI で判定（`ai.decide`）の質問ごとの問いの見出し（D-108）。
const DECIDE_INSTRUCTIONS_LABEL: &str = "AI に尋ねること";

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
        for (_, key, label, fill) in REQUIRED_VALUES.iter().filter(|(t, ..)| *t == nt) {
            if is_unset(node.params.get(*key)) {
                out.push(blank(&node.id, format!("/params/{key}"), label, *fill));
            }
        }
        // 判定の質問ごとの問いは、置いたばかりの空のままでも保存は通し、実行と自動実行のオンの前に止める（D-108）。
        if nt == NodeType::AiDecide {
            let questions = node.params.get("questions").and_then(Value::as_array);
            for (i, q) in questions.into_iter().flatten().enumerate() {
                if q.get("instructions").is_some_and(is_blank) {
                    out.push(blank(
                        &node.id,
                        format!("/params/questions/{i}/instructions"),
                        DECIDE_INSTRUCTIONS_LABEL,
                        Fill::Text,
                    ));
                }
            }
            // 出口を分ける判定のしきい値は、既定を置かず書かせる（版ごとに実測で決める・PIT-80・D-88）。
            if let Some(route) = node.params.get("route").and_then(Value::as_object) {
                if route.get("min_confidence").is_none_or(Value::is_null) {
                    out.push(blank(
                        &node.id,
                        "/params/route/min_confidence".to_string(),
                        DECIDE_THRESHOLD_LABEL,
                        Fill::Text,
                    ));
                }
            }
        }
    }
    out
}

/// 中にブロックの無い繰り返し（保存時の警告・有効化と実行の前に止める理由・D-85）。
///
/// 中のブロックは `parent` がその繰り返しの id のノード（ir.md §5.3）。文言はどの場面（保存・実行・自動実行のオン）
/// でも通る形にする（付録 D の N4.10）。
#[must_use]
pub fn empty_regions(ir: &WorkflowIr) -> Vec<ValidationError> {
    ir.nodes
        .iter()
        .filter(|m| NodeType::parse(&m.node_type) == Some(NodeType::ControlMap))
        .filter(|m| !ir.nodes.iter().any(|n| n.parent.as_deref() == Some(m.id.as_str())))
        .map(|m| {
            ValidationError::new(
                EMPTY_REGION,
                format!(
                    "「{}」の中にブロックがありません。1 件ごとに行う処理を中に置くまで、実行も自動実行のオンもできません。",
                    block_name(m)
                ),
            )
            .at_node(&m.id)
        })
        .collect()
}

/// 保存は通すが、実行と自動実行のオンの前に止めるもの（必須の欄が空・中にブロックの無い繰り返し）。
fn not_ready(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = required_text_empty(ir);
    out.extend(empty_regions(ir));
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
/// 必須の欄が空のもの（D-43・D-83）と、中にブロックの無い繰り返し（D-85）と、schedule / event で動く IR の
/// 保存先フォルダの省略（D-40）・一覧にするフォルダの省略（D-83）。
#[must_use]
pub fn enable_blockers(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = not_ready(ir);
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

/// 対話の実行の前に止める理由（空なら起動できる）。必須の欄が空のもの（D-43・D-83）と、中にブロックの無い
/// 繰り返し（D-85）。
///
/// 保存先・一覧にするフォルダの省略は止めない（対話の run は起動した本人のルートを使える）。
#[must_use]
pub fn run_blockers(ir: &WorkflowIr) -> Vec<ValidationError> {
    not_ready(ir)
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

    /// D-108: 判定の質問の「AI に尋ねること」が空なら、保存の警告にとどめ、実行と自動実行のオンの前に止める
    /// （質問ごとの位置を返す）。参照は書けない欄なので、中身のある文だけが空でない。
    #[test]
    fn a_blank_decide_question_blocks_run_and_enable() {
        let ir = ir(
            json!([{ "kind": "interactive" }]),
            json!([{ "id": "triage", "type": "ai.decide", "params": {
                "model": "decide-stub", "state": "本文",
                "questions": [
                    { "key": "q1", "type": "choice", "instructions": "",
                      "options": [{ "key": "option_1" }, { "key": "option_2" }] },
                    { "key": "q2", "type": "boolean", "instructions": "請求か" },
                    { "key": "q3", "type": "boolean", "instructions": "  " }
                ] } }]),
        );
        let w = required_text_empty(&ir);
        assert_eq!(
            codes(&w),
            vec![
                (
                    REQUIRED_TEXT_EMPTY,
                    Some("triage"),
                    Some("/params/questions/0/instructions")
                ),
                (
                    REQUIRED_TEXT_EMPTY,
                    Some("triage"),
                    Some("/params/questions/2/instructions")
                )
            ]
        );
        assert_eq!(
            w[0].message,
            "「AI に尋ねること」が空です。入れるまで、実行も自動実行のオンもできません。"
        );
        assert_eq!(run_blockers(&ir).len(), 2);
        assert_eq!(enable_blockers(&ir).len(), 2);
    }

    /// D-88: 出口を分ける判定のしきい値は既定を置かないので、空なら保存の警告にとどめ、実行と自動実行のオンの
    /// 前に止める（項目が無い・null のどちらも）。しきい値を書いた判定と、出口を分けない判定は止めない。
    #[test]
    fn a_blank_decide_threshold_blocks_run_and_enable() {
        let decide = |id: &str, route: Option<Value>| {
            let mut n = json!({ "id": id, "type": "ai.decide", "params": {
                "model": "decide-stub", "state": "本文",
                "questions": [{ "key": "q1", "type": "boolean", "instructions": "請求か" }] } });
            if let Some(r) = route {
                n["params"]["route"] = r;
            }
            n
        };
        let ir = ir(
            json!([{ "kind": "interactive" }]),
            json!([
                decide("missing", Some(json!({ "question": "q1" }))),
                decide(
                    "null",
                    Some(json!({ "question": "q1", "min_confidence": null }))
                ),
                decide(
                    "set",
                    Some(json!({ "question": "q1", "min_confidence": 0.8 }))
                ),
                decide("no_route", None)
            ]),
        );
        let w = required_text_empty(&ir);
        let at = Some("/params/route/min_confidence");
        assert_eq!(
            codes(&w),
            vec![
                (REQUIRED_TEXT_EMPTY, Some("missing"), at),
                (REQUIRED_TEXT_EMPTY, Some("null"), at)
            ]
        );
        assert_eq!(
            w[0].message,
            "「しきい値（AI の自信）」が空です。入れるまで、実行も自動実行のオンもできません。"
        );
        assert_eq!(run_blockers(&ir).len(), 2);
        assert_eq!(enable_blockers(&ir).len(), 2);
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
                { "id": "map_body", "type": "storage.read", "parent": "map",
                  "params": { "file": { "$from": "each", "path": "/item" } } },
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

    /// BUG-151: 置いたばかりの業務データのブロック（テーブル・値・レコード・次の状態・行が空）は、保存の警告にとどめ、
    /// 実行と自動実行のオンの前に止める。項目が無いのも空。中身の無い項目のまとまり・リスト（「項目を並べる」で 0 行）
    /// は止めず、保存の注意（`ir.empty_values`）だけにする。選んだ値・参照・中身のある値は止めない。
    #[test]
    fn blank_data_block_fields_block_run_and_enable() {
        const T: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
        let r = json!({ "$from": "input", "path": "/id" });
        let ir = ir(
            json!([{ "kind": "interactive" }]),
            json!([
                { "id": "query", "type": "data.query", "params": {} },
                { "id": "add", "type": "data.record.create", "params": { "table": "", "data": "" } },
                { "id": "add_rows", "type": "data.record.create", "params": { "table": T, "data": {} } },
                { "id": "edit", "type": "data.record.update", "params": { "table": T, "record": " ", "patch": {} } },
                { "id": "del", "type": "data.record.delete", "params": { "table": T } },
                { "id": "move", "type": "data.transition", "params": { "table": T, "record": r, "to": "" } },
                { "id": "bulk", "type": "data.bulk_upsert", "params": { "table": T, "rows": [] } },
                // 選んだ値・参照・中身のある値は止めない。
                { "id": "ok_add", "type": "data.record.create",
                  "params": { "table": T, "data": { "title": "a" } } },
                { "id": "ok_bulk", "type": "data.bulk_upsert",
                  "params": { "table": { "$from": "input", "path": "/t" }, "rows": { "$from": "input", "path": "/rows" } } }
            ]),
        );
        let w = required_text_empty(&ir);
        assert_eq!(
            codes(&w),
            vec![
                (REQUIRED_TEXT_EMPTY, Some("query"), Some("/params/table")),
                (REQUIRED_TEXT_EMPTY, Some("add"), Some("/params/table")),
                (REQUIRED_TEXT_EMPTY, Some("add"), Some("/params/data")),
                (REQUIRED_TEXT_EMPTY, Some("edit"), Some("/params/record")),
                (REQUIRED_TEXT_EMPTY, Some("del"), Some("/params/record")),
                (REQUIRED_TEXT_EMPTY, Some("move"), Some("/params/to")),
            ]
        );
        // 中身の無い入れ物は、保存の注意だけ（止めない）。
        let e = empty_values(&ir);
        assert_eq!(
            codes(&e),
            vec![
                (EMPTY_VALUES, Some("add_rows"), Some("/params/data")),
                (EMPTY_VALUES, Some("edit"), Some("/params/patch")),
                (EMPTY_VALUES, Some("bulk"), Some("/params/rows")),
            ]
        );
        assert_eq!(
            e[0].message,
            "「レコードの値」に列がありません。このままでは、どの列にも値を入れずにレコードを追加します。"
        );
        assert_eq!(
            w[0].message,
            "「対象のテーブル」が空です。選ぶまで、実行も自動実行のオンもできません。"
        );
        assert_eq!(
            w[2].message,
            "「レコードの値」が空です。入れるまで、実行も自動実行のオンもできません。"
        );
        assert_eq!(run_blockers(&ir).len(), w.len());
        assert_eq!(enable_blockers(&ir).len(), w.len());
    }

    /// BUG-151: ファイルを選ぶブロック（読むファイル・集計と編集の CSV ファイル・編集前の版・編集の内容）と、実行する
    /// スキルも、置いたばかりの空のまま保存でき、実行と自動実行のオンの前に止める。選んだファイル・参照は止めない。
    #[test]
    fn blank_file_and_skill_fields_block_run_and_enable() {
        const F: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
        let ir = ir(
            json!([{ "kind": "interactive" }]),
            json!([
                { "id": "read", "type": "storage.read", "params": { "file": "" } },
                { "id": "sum", "type": "csv.query", "params": { "sql": "SELECT 1" } },
                { "id": "edit", "type": "csv.patch", "params": { "file": F, "ops": [] } },
                { "id": "skill", "type": "skill.invoke", "params": { "skill": "" } },
                { "id": "ok_read", "type": "storage.read",
                  "params": { "file": { "$from": "input", "path": "/file" } } },
                { "id": "ok_edit", "type": "csv.patch",
                  "params": { "file": F, "base_rev": 3, "ops": [{ "op": "row_delete", "row": 0 }] } }
            ]),
        );
        let w = required_text_empty(&ir);
        assert_eq!(
            codes(&w),
            vec![
                (REQUIRED_TEXT_EMPTY, Some("read"), Some("/params/file")),
                (REQUIRED_TEXT_EMPTY, Some("sum"), Some("/params/file")),
                (REQUIRED_TEXT_EMPTY, Some("edit"), Some("/params/base_rev")),
                (REQUIRED_TEXT_EMPTY, Some("skill"), Some("/params/skill")),
            ]
        );
        assert_eq!(
            codes(&empty_values(&ir)),
            vec![(EMPTY_VALUES, Some("edit"), Some("/params/ops"))]
        );
        assert_eq!(
            w[0].message,
            "「読むファイル」が空です。選ぶまで、実行も自動実行のオンもできません。"
        );
        assert_eq!(run_blockers(&ir).len(), w.len());
        assert_eq!(enable_blockers(&ir).len(), w.len());
    }

    /// 中身の無い入れ物（`data: {}`・`patch: {}`・`rows: []`・`ops: []`）は、実行も自動実行のオンも止めない。
    /// 状態の進め方のあるテーブルに最初の状態だけのレコードを作る `data: {}` は正しい使い方で、前から保存されている
    /// 定義（子の実行・ミニアプリが固定した版も同じ起動の道を通る）を止めない。
    #[test]
    fn empty_containers_do_not_block_run_or_enable() {
        const T: &str = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
        let r = json!({ "$from": "input", "path": "/id" });
        let ir = ir(
            json!([{ "kind": "interactive" }, { "kind": "schedule", "cron": "0 9 * * 1", "tz": "Asia/Tokyo" }]),
            json!([
                { "id": "add", "type": "data.record.create", "params": { "table": T, "data": {} } },
                { "id": "edit", "type": "data.record.update", "params": { "table": T, "record": r, "patch": {} } },
                { "id": "bulk", "type": "data.bulk_upsert", "params": { "table": T, "rows": [] } },
                { "id": "csv", "type": "csv.patch", "params": { "file": T, "base_rev": 1, "ops": [] } }
            ]),
        );
        assert!(run_blockers(&ir).is_empty(), "{:?}", run_blockers(&ir));
        assert!(
            enable_blockers(&ir).is_empty(),
            "{:?}",
            enable_blockers(&ir)
        );
        assert_eq!(empty_values(&ir).len(), 4);
    }

    /// D-85: 中にブロックの無い繰り返しは、保存の警告にとどめ、実行と自動実行のオンの前に止める（置いた直後の
    /// 繰り返しを保存できるように）。中にブロックのある繰り返し（入れ子の中の繰り返しも）は止めない。文言は
    /// 繰り返しの名前（付けた名前・無ければ種類の名前）を名指しする。
    #[test]
    fn an_empty_map_blocks_run_and_enable() {
        let ir = ir(
            json!([{ "kind": "interactive" }]),
            json!([
                { "id": "named", "type": "control.map", "label": "申請ごとに",
                  "params": { "items": { "$from": "input", "path": "/rows" } } },
                { "id": "unnamed", "type": "control.map",
                  "params": { "items": { "$from": "input", "path": "/rows" } } },
                { "id": "outer", "type": "control.map",
                  "params": { "items": { "$from": "input", "path": "/rows" } } },
                { "id": "inner", "type": "control.map", "parent": "outer",
                  "params": { "items": { "$from": "each", "path": "/item" } } },
                { "id": "leaf", "type": "storage.read", "parent": "inner",
                  "params": { "file": { "$from": "each", "path": "/item" } } }
            ]),
        );
        let w = empty_regions(&ir);
        assert_eq!(
            codes(&w),
            vec![
                (EMPTY_REGION, Some("named"), None),
                (EMPTY_REGION, Some("unnamed"), None)
            ]
        );
        assert_eq!(
            w[0].message,
            "「申請ごとに」の中にブロックがありません。1 件ごとに行う処理を中に置くまで、実行も自動実行のオンもできません。"
        );
        assert!(
            w[1].message.starts_with("「繰り返し」の中に"),
            "{}",
            w[1].message
        );
        // 必須の欄は埋まっているので、止める理由は中の空だけ。
        assert!(required_text_empty(&ir).is_empty());
        assert_eq!(codes(&run_blockers(&ir)), codes(&w));
        assert_eq!(codes(&enable_blockers(&ir)), codes(&w));
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
