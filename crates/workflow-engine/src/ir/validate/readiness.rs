//! 保存は止めず、有効化と実行の前に止める条件（D-40・D-43・D-83）。
//!
//! - **必須の欄が空**（[`REQUIRED_FIELDS`] の表と、業務データ・ファイルを選ぶブロックの [`REQUIRED_VALUES`] の表（項目が
//!   無いのも空・BUG-151）と、`control.branch` の条件の調べる値と、`ai.decide` の質問ごとの
//!   「AI に尋ねること」（D-108）と、出口を分ける `ai.decide` のしきい値（既定を置かない・D-88）と、置いたばかりの
//!   `human.approval` の件名・テーブル・承認者・判断の行き先（D-129）と、`csv.query` のつなぐ CSV の名前とファイル（#1037）と、
//!   `workflow.reply` の返す値（BUG-168））: 書きかけでも保存はできるように、保存時は path つきの警告
//!   （[`REQUIRED_TEXT_EMPTY`]）にとどめる。空のまま動かすと、空の依頼で
//!   AI や検索を呼ぶ・いつも同じ出口へ進む・リストでない値を繰り返そうとして失敗する、などになるので、有効化
//!   （[`enable_blockers`]）と対話の実行（[`run_blockers`]）と自動実行・子の実行の起動（[`unattended_run_blockers`]）の
//!   前に止める。置いたばかりのブロックは、選ぶ欄も空の文字（`""`）で置く（エディタの既定・D-83）。
//!   - 例外は `workflow.reply` の返す値: 結果が出るのはボタン・会話から起動した run だけなので、対話の実行の前にだけ
//!     止め、有効化と自動実行・子の実行の起動では止めない（値が使われないのに、前から動いていた自動実行を止めない・
//!     BUG-168 と BUG-51 の重なり）。
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
//! どれも IR だけで決まる純関数。有効化（`RegistrationService::enable`）・同意計画の API・対話と自動実行と子の実行の
//! 起動（`WorkflowRunLauncher`）が同じ関数を呼ぶ。

use serde_json::Value;

use super::{block_name, ValidationError};
use crate::ir::params::ReplyFormat;
use crate::ir::{Node, Trigger, WorkflowIr};
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

/// 置いたばかりの承認を待つ（`human.approval`）の必須の欄のうち、承認者より前の欄（params の中の位置・見出し・
/// 入れ方・D-129）。位置は `/params` の下の JSON Pointer（`record/table` など）で、項目が無いのは数えない（V1 の
/// 誤り）。並びはエディタの欄の並び（承認者は [`blank_assignees`]、判断の行き先は [`APPROVAL_CHOICES`]）。
const APPROVAL_HEAD: &[(&str, &str, Fill)] = &[
    ("title", "受信箱の件名", Fill::Text),
    ("record/table", "承認するテーブル", Fill::Pick),
];

/// 承認を待つの判断の行き先（使う判断だけ・D-129）。見出しはエディタの欄の読み上げの名前と同じ。
const APPROVAL_CHOICES: &[(&str, &str, Fill)] = &[
    ("approve/to", "承認したときに進める状態", Fill::Pick),
    ("reject/to", "却下したときに進める状態", Fill::Pick),
    ("return/to", "差し戻したときに進める状態", Fill::Pick),
];

/// 承認を待つの承認者の欄の項目（エディタは列が空の承認者も 0 件もこの欄の下で言う）。
const APPROVAL_ASSIGNEES: &str = "assignees";

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
    (NodeType::DataRecordUpsert, "table", TABLE_LABEL, Fill::Pick),
    (
        NodeType::DataRecordUpsert,
        "match_on",
        "同じレコードを探す列",
        Fill::Pick,
    ),
    (NodeType::DataRecordUpsert, "data", "入れる値", Fill::Text),
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

/// 必須の欄のうち、選ぶ欄（一覧や入力・結果から選ぶ）の（ノード種の名前, params の項目）。エディタは、空の欄の下の
/// 促しを、この表で「選んでください」と「入力してください」に分ける（web へは `export-workflow-ts` が書き出す・
/// 手書きの対応物を持たない）。項目は params の最上位の名前（エディタは誤りの位置の先頭の項目で欄を引く）。
#[must_use]
pub fn required_pick_fields() -> Vec<(&'static str, &'static str)> {
    let approval = APPROVAL_HEAD
        .iter()
        .chain(APPROVAL_CHOICES)
        .map(|(key, _, fill)| (NodeType::HumanApproval, *key, *fill))
        .chain([(NodeType::HumanApproval, APPROVAL_ASSIGNEES, Fill::Pick)]);
    let mut out: Vec<(&'static str, &'static str)> = Vec::new();
    for (t, key, _) in REQUIRED_FIELDS
        .iter()
        .map(|(t, key, _, fill)| (*t, *key, *fill))
        .chain(
            REQUIRED_VALUES
                .iter()
                .map(|(t, key, _, fill)| (*t, *key, *fill)),
        )
        .chain(approval)
        .filter(|(.., fill)| *fill == Fill::Pick)
    {
        let top = (t.as_str(), key.split('/').next().unwrap_or(key));
        if !out.contains(&top) {
            out.push(top);
        }
    }
    out
}

/// 必須の値の欄が空か（項目なし・[`is_blank`]）。
fn is_unset(v: Option<&Value>) -> bool {
    v.is_none_or(is_blank)
}

/// 必須の欄のうち、ほかの欄に値があれば空のままでよいもの（ノード種・空でよい欄・値があればよい欄）。
///
/// エディタの「入力してください」も、この表から書き出した `BLANK_OK_WHEN_SET` で同じ決まりに従う（`export-workflow-ts`・
/// 手書きの対応物を持たない）。
pub const BLANK_OK_WHEN_SET: &[(NodeType, &str, &str)] = &[
    // 「足す数」だけで書き換えるレコードの更新は、変更する値が空でよい（GAP-16）。
    (NodeType::DataRecordUpdate, "patch", "add"),
];

/// [`BLANK_OK_WHEN_SET`] を（ノード種の名前, 空でよい欄, 値があればよい欄）で返す（web へ書き出す）。
#[must_use]
pub fn blank_ok_when_set() -> Vec<(&'static str, &'static str, &'static str)> {
    BLANK_OK_WHEN_SET
        .iter()
        .map(|(t, key, other)| (t.as_str(), *key, *other))
        .collect()
}

/// `key` の欄が、[`BLANK_OK_WHEN_SET`] のとおりほかの欄に値があるので空でよいか。
fn blank_allowed(nt: NodeType, node: &Node, key: &str) -> bool {
    BLANK_OK_WHEN_SET
        .iter()
        .any(|(t, k, other)| *t == nt && *k == key && has_value(node.params.get(*other)))
}

/// 欄に値があるか（中身の無いまとまりと空は無いのと同じ・参照は実行時まで分からないのであるとみなす）。
fn has_value(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Object(m)) => !m.is_empty(),
        Some(v) => !is_blank(v),
    }
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
            // 足す数で書き換えるなら、変更する値が空でも何かを書く（GAP-16・[`BLANK_OK_WHEN_SET`]）。
            if blank_allowed(nt, node, key) {
                continue;
            }
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
        // 結果を画面に返すの空のリスト（表・ファイルへのリンク）も注意だけにする（BUG-168）。ファイルへのリンクは中身の
        // 無いまとまり（`{}`・「項目を並べる」で 0 行のとき）も 0 件で返すので同じに言う。文章の形のリストと表の形の
        // まとまりは保存の照合（V7 の ir.reply_type）が止める。
        let value = node.params.get("value");
        let empty_list = value.and_then(Value::as_array).is_some_and(Vec::is_empty);
        let empty_object = value
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty);
        let (message, empty) = match reply_format(node) {
            Some(ReplyFormat::Table) => (
                "「表にするリスト」に行がありません。このままでは、行の無い表を返します。",
                empty_list,
            ),
            Some(ReplyFormat::FileLink) => (
                "「ファイルのリスト」にファイルがありません。このままでは、リンクの無い結果を返します。",
                empty_list || empty_object,
            ),
            _ => continue,
        };
        if nt == NodeType::WorkflowReply && empty {
            out.push(
                ValidationError::new(EMPTY_VALUES, message)
                    .at_node(&node.id)
                    .at_path("/params/value"),
            );
        }
    }
    out
}

/// 結果を画面に返す（`workflow.reply`）の形（読めなければ `None`・V1 が報告する）。
fn reply_format(node: &Node) -> Option<ReplyFormat> {
    if NodeType::parse(&node.node_type) != Some(NodeType::WorkflowReply) {
        return None;
    }
    serde_json::from_value(node.params.get("format")?.clone()).ok()
}

/// 結果を画面に返すの値の欄の見出し（形ごと・エディタの欄の見出しと同じ・BUG-168）。
fn reply_value_label(node: &Node) -> &'static str {
    match reply_format(node) {
        Some(ReplyFormat::Table) => "表にするリスト",
        Some(ReplyFormat::FileLink) => "ファイルのリスト",
        _ => "返す内容",
    }
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
pub(super) fn is_blank(v: &Value) -> bool {
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

/// 承認を待つの必須の欄が空のもの（D-129）。件名・テーブル・承認者・判断の行き先の順（エディタの欄の並び）。
fn approval_blanks(node: &Node, out: &mut Vec<ValidationError>) {
    let blank_at = |key: &str| {
        node.params
            .pointer(&format!("/{key}"))
            .is_some_and(is_blank)
    };
    for (key, label, fill) in APPROVAL_HEAD {
        if blank_at(key) {
            out.push(blank(&node.id, format!("/params/{key}"), label, *fill));
        }
    }
    blank_assignees(node, out);
    for (key, label, fill) in APPROVAL_CHOICES {
        if blank_at(key) {
            out.push(blank(&node.id, format!("/params/{key}"), label, *fill));
        }
    }
}

/// 承認を待つの承認者が空のもの（D-129）。
///
/// 列を選んでいない承認者（`{"field": ""}`）はその承認者を言う（実行すると「列を読めない」で失敗する）。そのほかに
/// 値のある承認者が 1 人もいなければ（0 件・ユーザーと部署が空の文字だけ）、承認者の欄を言う（実行すると担当者が
/// いないので失敗する）。空の文字のユーザー・部署は、ほかに承認者がいれば数えずに動くので止めない。
fn blank_assignees(node: &Node, out: &mut Vec<ValidationError>) {
    let list = node
        .params
        .get(APPROVAL_ASSIGNEES)
        .and_then(Value::as_array);
    let mut filled = false;
    let before = out.len();
    for (i, a) in list.into_iter().flatten().enumerate() {
        match a.get("field") {
            Some(f) if is_blank(f) => out.push(blank(
                &node.id,
                format!("/params/{APPROVAL_ASSIGNEES}/{i}/field"),
                &format!("承認者 {} の列", i + 1),
                Fill::Pick,
            )),
            Some(_) => filled = true,
            None => {
                filled |= ["user", "role"]
                    .iter()
                    .any(|k| a.get(*k).is_some_and(|v| !is_blank(v)));
            }
        }
    }
    if !filled && out.len() == before {
        out.push(
            ValidationError::new(
                REQUIRED_TEXT_EMPTY,
                "「承認者」がいません。追加するまで、実行も自動実行のオンもできません。",
            )
            .at_node(&node.id)
            .at_path(format!("/params/{APPROVAL_ASSIGNEES}")),
        );
    }
}

/// CSV を集計（`csv.query`）のつなぐ CSV（`tables` の各要素）の名前とファイルが空のもの（項目が無いのも空・#1037）。
///
/// 空のまま動かすと、実行は名前・ファイルを読めずに必ず失敗する（同意の提案は空のファイルを選ばせる扱いにするので、
/// 有効化の前に止めないと、有効化だけが通る）。見出しはエディタの欄の見出し（「1 つ目の CSV の SQL での名前」・
/// 「1 つ目のつなぐ CSV」）と同じ。`tables` 自体を値式で書いたときは見ない（中身は実行まで分からない・V4 と同意の提案と
/// 同じ扱い）。名前とファイルが 1 つの項目の下に並ぶので、[`required_pick_fields`] には載せない（エディタは行ごとに
/// 空の欄を言う）。
fn csv_table_blanks(node: &Node, out: &mut Vec<ValidationError>) {
    let tables = node.params.get("tables").and_then(Value::as_array);
    for (i, t) in tables.into_iter().flatten().enumerate() {
        let nth = i + 1;
        if is_unset(t.get("name")) {
            out.push(blank(
                &node.id,
                format!("/params/tables/{i}/name"),
                &format!("{nth} つ目の CSV の SQL での名前"),
                Fill::Text,
            ));
        }
        if is_unset(t.get("file")) {
            out.push(blank(
                &node.id,
                format!("/params/tables/{i}/file"),
                &format!("{nth} つ目のつなぐ CSV"),
                Fill::Pick,
            ));
        }
    }
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
///
/// `workflow.reply` の返す値の空も含む（保存の警告と対話の実行の前に止める理由。有効化と自動実行・子の実行の起動では
/// 止めない・[`unattended_run_blockers`]）。
#[must_use]
pub fn required_text_empty(ir: &WorkflowIr) -> Vec<ValidationError> {
    blank_fields(ir, true)
}

/// [`required_text_empty`] の本体。`with_reply` が偽なら、`workflow.reply` の返す値の空を数えない（結果がどの画面にも
/// 出ない起動・BUG-168）。
fn blank_fields(ir: &WorkflowIr, with_reply: bool) -> Vec<ValidationError> {
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
        if nt == NodeType::HumanApproval {
            approval_blanks(node, &mut out);
        }
        // 返す値が空なら、空の結果を返すだけなので、ボタン・会話からの実行の前に止める（BUG-168）。見出しは形ごとの
        // 欄の見出し。
        if with_reply
            && nt == NodeType::WorkflowReply
            && node.params.get("value").is_some_and(is_blank)
        {
            out.push(
                ValidationError::new(
                    REQUIRED_TEXT_EMPTY,
                    format!(
                        "「{}」が空です。入れるまで、ボタンや会話からは実行できません。",
                        reply_value_label(node)
                    ),
                )
                .at_node(&node.id)
                .at_path("/params/value"),
            );
        }
        for (_, key, label, fill) in REQUIRED_VALUES.iter().filter(|(t, ..)| *t == nt) {
            // 「足す数」だけで書き換えるレコードの更新は、変更する値が空でよい（GAP-16・[`BLANK_OK_WHEN_SET`]）。
            if blank_allowed(nt, node, key) {
                continue;
            }
            if is_unset(node.params.get(*key)) {
                out.push(blank(&node.id, format!("/params/{key}"), label, *fill));
            }
        }
        if nt == NodeType::CsvQuery {
            csv_table_blanks(node, &mut out);
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
            // 出口を分ける判定のしきい値は、既定を置かず書かせる（版ごとに実測で決める・PIT-81・D-88）。
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

/// 保存は通すが、実行と自動実行のオンの前に止めるもの（必須の欄が空・中にブロックの無い繰り返し）。`with_reply` が
/// 偽なら、`workflow.reply` の返す値の空を数えない（結果がどの画面にも出ない起動・BUG-168）。
fn not_ready(ir: &WorkflowIr, with_reply: bool) -> Vec<ValidationError> {
    let mut out = blank_fields(ir, with_reply);
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
/// 保存先フォルダの省略（D-40）・一覧にするフォルダの省略（D-83）。`workflow.reply` の返す値の空は止めない
/// （自動実行の結果はどの画面にも出ない・BUG-168）。
#[must_use]
pub fn enable_blockers(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = not_ready(ir, false);
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

/// 対話の実行（ボタン・会話・ワークフローの画面から起動する run）の前に止める理由（空なら起動できる）。必須の欄が
/// 空のもの（D-43・D-83・`workflow.reply` の返す値の空を含む・BUG-168）と、中にブロックの無い繰り返し（D-85）。
///
/// 保存先・一覧にするフォルダの省略は止めない（対話の run は起動した本人のルートを使える）。
#[must_use]
pub fn run_blockers(ir: &WorkflowIr) -> Vec<ValidationError> {
    not_ready(ir, true)
}

/// 自動実行（schedule / event）と子の実行（`workflow.start`）の起動の前に止める理由（空なら起動できる・BUG-51）。
///
/// [`run_blockers`] から `workflow.reply` の返す値の空を除いたもの。これらの run の結果はどの画面にも出ないので、
/// 返す値が空でも止めない（前から自動実行がオンの版や、呼び出される側のワークフローを黙って止めない・BUG-168）。
#[must_use]
pub fn unattended_run_blockers(ir: &WorkflowIr) -> Vec<ValidationError> {
    not_ready(ir, false)
}

#[cfg(test)]
mod tests;
