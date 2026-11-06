//! 手動の実行（押した本人・実行した本人の権限）で、ワークフローが対象に要る権限（GAP-37）。
//!
//! ミニアプリのボタン・共有されたワークフローの実行は、押した人の ReBAC で動く（engine.md §6・miniapp-platform §7）。
//! 作る人が共有の前に「相手が使えるか」を確かめ、使う人が押す前に「押せない理由」を知るための材料を、IR から静的に
//! 挙げる。判定そのもの（OpenFGA の check）は呼び出し側（`gui::access`）が単一のチョークポイントで行う。
//!
//! # 単一の定義
//!
//! どのブロックがどの対象を読む・書くかは、自動実行の同意の提案（[`RegistrationService::consent_plan`]）と同じ
//! 定義（`consent::suggested_grants`）で、ここはそれを手動の実行の言葉に読み替えるだけ（対象の欄の名前やブロックの
//! 種類ごとの読む・書くをもう一度書かない）。同意の提案は同じ対象・relation を 1 つにまとめるが、ここではまとめる前の
//! ブロックごとの提案を使う（同じテーブルへの追加と書き換えをまとめると、書き換えに要る編集を落とす）。
//! 読み替えるのは次の 3 つ。
//!
//! 1. **レコードの追加はテーブルの「追加だけ」でもできる**（D-113）。委譲は編集（editor）を提案するが、本人の実行では
//!    追加だけ（submitter）で足りる（[`AccessKind::Add`]）。
//! 2. **書き換え・状態を進めるは、追加だけの人も自分のレコードで許されることがある**（D-127）。編集が要るが、
//!    追加だけでも足りうる（[`AccessKind::Change`]）。削除・追加か更新・まとめて登録は編集が要る（[`AccessKind::Edit`]）。
//! 3. **きっかけ・AI の参照範囲は要る権限として確かめない**。きっかけは自動実行の委譲で読むもので、手動の実行では
//!    使わない。AI エージェントの読むツール・文書検索・フォルダーを省いた一覧（ドライブのトップ）は「使う人が見られる
//!    ものだけ」を使い、足りなくても失敗しない（対象も決まっていない）ので、下の「前もって確かめられないブロック」に
//!    挙げる。フォルダーを省いた保存もドライブのトップで、組織のメンバーなら誰でも書ける。
//!
//! 対象が入力や前の結果で決まるとき（値の式）・まだ選んでいないときは、対象を決めずに挙げる（`object_id = None`。
//! 呼び出し側は確かめずに「実行のときに決まる」と言う）。
//!
//! # 前もって確かめられないブロック（GAP-82）
//!
//! 次のブロックが使う権限は、ブロックが動くまで対象が決まらないか、テーブル・ファイル・フォルダーの共有では決まらない
//! ので、要る権限としては挙げない。代わりに [`RegistrationService::access_gaps`] が挙げ、作る人の画面が「前もって
//! 確かめられないブロック」として名前を出す（「要る権限はありません」「全員持っています」と言い切らないため）。
//!
//! - スクリプトを実行（使ってよい操作にテーブル・ファイル・外部のサービス・別のワークフローがあるとき。中の
//!   `Shiki.data`・`Shiki.storage` などが何を読み書きするかは、スクリプトが動くまで決まらない）。使ってよい操作は
//!   ワークフロー全体のもの（`declared_scopes`）で、スクリプトのブロックごとには無いので、中でそれを使わない
//!   スクリプトも挙げる（言い過ぎの側）。本文の字面で見分けると、名前を組み立てて `Shiki` を引くスクリプトを
//!   取りこぼし、「確かめられた」と言い切ってしまう。
//! - AI エージェントの、社内の文書やファイルを読むツール（使う人が見られるものだけを読む）。
//! - 文書検索（使う人が見られる文書だけを探す）と、フォルダーを省いた一覧（ドライブのトップで、使う人が見られる
//!   ものだけを並べる）。AI エージェントと同じく、失敗はしないが、使う人によって結果が変わる（「全員持っています」と
//!   言い切ると、相手に見えない文書を前提にしたワークフローに気づかない・GAP-82）。
//! - シークレットを使う外部への送信（使う人がそのシークレットを使えるか）。
//! - 別のワークフローを実行（名前で引くワークフローと、そのワークフローが要る権限）。
//! - ドライブのトップへの保存（保存先のフォルダーを省いた保存。保存できるのは組織のメンバーだけ）。
//!
//! # 必ず通るブロックと、道によって通るブロック（BUG-216）
//!
//! 要る権限には、そのブロックが毎回動くかを添える（[`AccessNeed::conditional`]・決まりは [`crate::ir::paths`] の
//! 1 か所）。分かれた流れ・「エラー時」の出口の先と、道によって通る繰り返しの中のブロックは、入力や結果によって動かない
//! ので、使う人のボタンはそれだけでは止めない（通らない入力でも押せなくすると、前は動いていたボタンを止める）。毎回動く
//! 繰り返しの中は、リストが空でない実行を基準に毎回動くとみなす（GAP-97）。
//!
//! 毎回動くブロックでも、失敗を作った人が決めた道で扱うブロック（`on_error: continue` で `error` の出口から線が出て
//! いるものと、失敗を扱う繰り返しの中・[`crate::ir::paths::failure_handled`]）が要る権限は、場合によって要るものとする。
//! 権限が足りなければそのブロックが失敗し、作った人が決めた道（誤りの返答・失敗を記録して続けるなど）で終わるので、使う人
//! のボタンと「いま実行する」は止めない（ir.md §7.10 が業務上の失敗をこの形で扱うよう勧めている）。

use std::collections::HashSet;

use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use super::consent::suggested_grants;
use super::{RegistrationService, SuggestedGrant};
use crate::ir::paths::{always_run, failure_handled};
use crate::ir::validate::block_name;
use crate::ir::WorkflowIr;
use crate::vocab::{NodeType, Scope};

/// 権限を確かめる対象の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessObject {
    /// テーブル（`data_table`）。
    Table,
    /// ファイル。
    File,
    /// フォルダー。
    Folder,
}

impl AccessObject {
    /// 同意の提案の対象の種類（`object_kind`）から。テーブル・ファイル・フォルダーのほかは `None`。
    #[must_use]
    pub fn from_object_kind(kind: &str) -> Option<Self> {
        match kind {
            "data_table" => Some(AccessObject::Table),
            "file" => Some(AccessObject::File),
            "folder" => Some(AccessObject::Folder),
            _ => None,
        }
    }
}

/// 対象に要る操作（手動の実行・本人の権限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    /// 読む（テーブルは閲覧。追加だけの人も、自分が追加したレコードだけは読める）。
    Read,
    /// レコードを追加する（テーブルの追加だけか編集）。
    Add,
    /// レコードを書き換える・状態を進める（テーブルの編集。追加だけの人も、状態の進め方が作成者に許した自分の
    /// レコードは直せる・D-127）。
    Change,
    /// 編集が要る操作（レコードの削除・追加か更新・まとめて登録、CSV の編集、フォルダーへの保存）。
    Edit,
}

/// ワークフローが対象に要る権限 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccessNeed {
    /// 要るブロックの id。
    pub node_id: String,
    pub object: AccessObject,
    /// 対象（UUID を直接入れた欄のとき）。入力や前の結果で決まる・まだ選んでいないときは `None`。
    pub object_id: Option<Uuid>,
    pub kind: AccessKind,
    /// 場合によって要る: 道によって通るブロック（分かれた流れ・「エラー時」の出口の先・道によって通る繰り返しの中・
    /// [`crate::ir::paths`]）か、失敗を作った人が決めた道で扱うブロック（「エラー時」の出口・失敗を扱う繰り返しの中）
    /// だけが要る。偽なら、前のブロックがうまくいったとき毎回要り、足りなければ実行が失敗する。
    pub conditional: bool,
}

/// 前もって確かめられないブロックの種類（GAP-82・[モジュールの説明](self)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessUnchecked {
    /// スクリプトを実行（中で読み書きするテーブル・ファイルなどは、スクリプトが動くまで決まらない）。
    Script,
    /// AI エージェントの、社内の文書やファイルを読むツール（使う人が見られるものだけを読む）。
    AgentTools,
    /// 文書検索（使う人が見られる文書だけを探す・失敗はしない）。
    DocSearch,
    /// フォルダーを省いた一覧（ドライブのトップで、使う人が見られるものだけを並べる・失敗はしない）。
    DriveTopList,
    /// シークレットを使う外部への送信（使う人がそのシークレットを使えるか）。
    Secret,
    /// 別のワークフローを実行（名前で引くワークフローと、そのワークフローが要る権限）。
    StartWorkflow,
    /// ドライブのトップへの保存（保存できるのは組織のメンバーだけ）。
    DriveTop,
}

/// 前もって確かめられないブロック 1 つ。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccessGap {
    /// ブロックの id。
    pub node_id: String,
    /// ブロックの名前（付けた名前、無ければブロックの種類の名前・保存時の誤りと同じ呼び方）。
    pub block: String,
    pub kind: AccessUnchecked,
}

/// スクリプトの中から使えると、前もって確かめられない操作（`Shiki.data`・`Shiki.storage`・`Shiki.http`・
/// `Shiki.workflow`）。使ってよい操作にこのどれかがあれば、スクリプトを実行を挙げる。
const SCRIPT_REACH: [Scope; 6] = [
    Scope::DataRead,
    Scope::DataWrite,
    Scope::StorageRead,
    Scope::StorageWrite,
    Scope::HttpEgress,
    Scope::WorkflowStart,
];

impl RegistrationService {
    /// 手動の実行で本人に要る権限を IR から挙げる（純関数・並びはブロックの順・同じものは 1 つ）。
    ///
    /// 読む・書くの区別と対象は、同意の提案と同じ定義から読み替える（[モジュールの説明](self)）。
    #[must_use]
    pub fn access_needs(ir: &WorkflowIr) -> Vec<AccessNeed> {
        let sure = sure_nodes(ir);
        let mut out: Vec<AccessNeed> = Vec::new();
        for grant in suggested_grants(ir) {
            let Some(Reading::Need(need)) = read_grant(ir, &grant, &sure) else {
                continue;
            };
            if !out.contains(&need) {
                out.push(need);
            }
        }
        out
    }

    /// 手動の実行で、前もって確かめられないブロックを IR から挙げる（純関数・並びはブロックの順・同じものは 1 つ・
    /// GAP-82）。
    #[must_use]
    pub fn access_gaps(ir: &WorkflowIr) -> Vec<AccessGap> {
        let sure = sure_nodes(ir);
        let mut found: Vec<(String, AccessUnchecked)> = Vec::new();
        for grant in suggested_grants(ir) {
            if let Some(Reading::Unchecked(kind)) = read_grant(ir, &grant, &sure) {
                let node_id = grant.source.trim_start_matches("node:").to_string();
                if !found.contains(&(node_id.clone(), kind)) {
                    found.push((node_id, kind));
                }
            }
        }
        let script_reaches = ir
            .declared_scopes
            .iter()
            .any(|s| SCRIPT_REACH.iter().any(|r| r.as_str() == s));
        if script_reaches {
            for n in &ir.nodes {
                if NodeType::parse(&n.node_type) == Some(NodeType::ScriptRun) {
                    found.push((n.id.clone(), AccessUnchecked::Script));
                }
            }
        }
        // ブロックの順に並べる（同じブロックの中は見つけた順）。
        let mut out = Vec::new();
        for n in &ir.nodes {
            for (id, kind) in &found {
                if *id == n.id {
                    out.push(AccessGap {
                        node_id: id.clone(),
                        block: block_name(n),
                        kind: *kind,
                    });
                }
            }
        }
        out
    }
}

/// 要る権限が足りないと使う人のボタンを止めるブロック（前のブロックがうまくいったとき毎回動き、失敗を「エラー時」の
/// 出口で扱わないもの・[モジュールの説明](self)）。
fn sure_nodes(ir: &WorkflowIr) -> HashSet<&str> {
    let handled = failure_handled(ir);
    always_run(ir)
        .into_iter()
        .filter(|id| !handled.contains(id))
        .collect()
}

/// 同意の提案 1 つの、手動の実行での読み替え。
enum Reading {
    /// 本人に要る権限。
    Need(AccessNeed),
    /// 前もって確かめられない（GAP-82）。
    Unchecked(AccessUnchecked),
}

/// 同意の提案 1 つを、手動の実行で本人に要る権限か、前もって確かめられないブロックに読み替える（どちらでもない提案は
/// `None`）。
fn read_grant(ir: &WorkflowIr, grant: &SuggestedGrant, sure: &HashSet<&str>) -> Option<Reading> {
    // きっかけ（trigger:i）は自動実行の委譲で読むもの（手動の実行では使わない）。
    let node_id = grant.source.strip_prefix("node:")?;
    let node = ir.nodes.iter().find(|n| n.id == node_id)?;
    let node_type = NodeType::parse(&node.node_type)?;
    match node_type {
        // 文書検索と AI エージェントのツールは、使う人が見られる文書やファイルだけを使う（足りなくても失敗しないが、
        // 使う人によって結果が変わる。前もって確かめられない）。
        NodeType::RagSearch => return Some(Reading::Unchecked(AccessUnchecked::DocSearch)),
        NodeType::AgentInvoke => return Some(Reading::Unchecked(AccessUnchecked::AgentTools)),
        _ => {}
    }
    match grant.object_kind.as_str() {
        "secret" => return Some(Reading::Unchecked(AccessUnchecked::Secret)),
        "workflow" => return Some(Reading::Unchecked(AccessUnchecked::StartWorkflow)),
        _ => {}
    }
    let object = AccessObject::from_object_kind(&grant.object_kind)?;
    let object_id = grant
        .object_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s.trim()).ok());
    // フォルダーを省いた保存・一覧はドライブのトップ（前もって確かめられない）。保存は組織のメンバーだけができる。
    // 一覧は見られるものだけを返す（失敗しないが、使う人によって結果が変わる）。
    if object == AccessObject::Folder
        && object_id.is_none()
        && node
            .params
            .get("folder")
            .is_none_or(serde_json::Value::is_null)
    {
        return Some(Reading::Unchecked(if grant.relation == "editor" {
            AccessUnchecked::DriveTop
        } else {
            AccessUnchecked::DriveTopList
        }));
    }
    let kind = match (grant.relation.as_str(), object, node_type) {
        ("viewer", _, _) => AccessKind::Read,
        ("editor", AccessObject::Table, NodeType::DataRecordCreate) => AccessKind::Add,
        ("editor", AccessObject::Table, NodeType::DataRecordUpdate | NodeType::DataTransition) => {
            AccessKind::Change
        }
        ("editor", _, _) => AccessKind::Edit,
        _ => return None,
    };
    Some(Reading::Need(AccessNeed {
        node_id: node_id.to_string(),
        object,
        object_id,
        kind,
        conditional: !sure.contains(node_id),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TABLE: &str = "11111111-2222-3333-4444-555555555555";
    const FOLDER: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";
    const FILE: &str = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";

    fn needs(nodes: &serde_json::Value, triggers: &serde_json::Value) -> Vec<AccessNeed> {
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf",
            "declared_scopes": ["data.read", "data.write", "storage.read", "storage.write", "csv.read", "csv.write", "http.egress", "rag.query"],
            "triggers": triggers,
            "nodes": nodes, "edges": []
        }))
        .expect("test IR");
        RegistrationService::access_needs(&ir)
    }

    fn need(node: &str, object: AccessObject, id: Option<&str>, kind: AccessKind) -> AccessNeed {
        AccessNeed {
            node_id: node.into(),
            object,
            object_id: id.map(|s| Uuid::parse_str(s).unwrap()),
            kind,
            conditional: false,
        }
    }

    /// 追加は追加だけでも足り、書き換え・状態を進めるは追加だけでも足りうる。削除・追加か更新・まとめて登録は編集
    /// （D-113・D-127）。読むものは読む。
    #[test]
    fn table_writes_are_read_as_add_change_or_edit() {
        let table = json!({ "table": TABLE });
        let got = needs(
            &json!([
                { "id": "q", "type": "data.query", "params": { "table": TABLE } },
                { "id": "add", "type": "data.record.create", "params": { "table": TABLE, "data": {} } },
                { "id": "upd", "type": "data.record.update", "params": table },
                { "id": "mv", "type": "data.transition", "params": table },
                { "id": "ups", "type": "data.record.upsert", "params": table },
                { "id": "del", "type": "data.record.delete", "params": table },
                { "id": "bulk", "type": "data.bulk_upsert", "params": table }
            ]),
            &json!([]),
        );
        use AccessKind::*;
        use AccessObject::Table as T;
        assert_eq!(
            got,
            vec![
                need("q", T, Some(TABLE), Read),
                need("add", T, Some(TABLE), Add),
                need("upd", T, Some(TABLE), Change),
                need("mv", T, Some(TABLE), Change),
                need("ups", T, Some(TABLE), Edit),
                need("del", T, Some(TABLE), Edit),
                need("bulk", T, Some(TABLE), Edit),
            ]
        );
    }

    /// ファイルとフォルダー: 読む・一覧は閲覧、保存・CSV の編集は編集。フォルダーを省いた保存・一覧はドライブのトップ
    /// なので要る権限には挙げない（前もって確かめられないブロック・GAP-82）。値の式の対象は対象を決めずに挙げる。
    #[test]
    fn storage_and_csv_targets() {
        let got = needs(
            &json!([
                { "id": "r", "type": "storage.read", "params": { "file": FILE } },
                { "id": "ls", "type": "storage.list", "params": { "folder": FOLDER } },
                { "id": "w", "type": "storage.write", "params": { "folder": FOLDER, "name": "a.txt", "content": "x" } },
                { "id": "top", "type": "storage.write", "params": { "name": "a.txt", "content": "x" } },
                { "id": "q", "type": "csv.query", "params": { "file": FILE, "sql": "SELECT 1" } },
                { "id": "p", "type": "csv.patch", "params": { "file": { "$from": "input", "path": "/file" }, "base_rev": 1, "ops": [] } },
                { "id": "cw", "type": "csv.write", "params": { "folder": { "$from": "input", "path": "/f" }, "name": "o", "content": "a\n" } }
            ]),
            &json!([]),
        );
        use AccessKind::*;
        use AccessObject::*;
        assert_eq!(
            got,
            vec![
                need("r", File, Some(FILE), Read),
                need("ls", Folder, Some(FOLDER), Read),
                need("w", Folder, Some(FOLDER), Edit),
                need("q", File, Some(FILE), Read),
                need("p", File, None, Edit),
                need("cw", Folder, None, Edit),
            ]
        );
    }

    /// きっかけ・AI の参照範囲・シークレット・別のワークフローは挙げない。承認・データの変更を待つは読む。
    /// 同じブロックの同じ対象は 1 つにする。
    #[test]
    fn triggers_ai_scopes_and_named_targets_are_not_needs() {
        let got = needs(
            &json!([
                { "id": "ag", "type": "agent.invoke", "params": { "instruction": "x", "allowed_tools": ["rag.search", "storage.read"] } },
                { "id": "rs", "type": "rag.search", "params": { "query": "x" } },
                { "id": "h", "type": "http.request", "params": { "url": "https://example.com", "secret": { "name": "k" } } },
                { "id": "ok", "type": "human.approval", "params": { "title": "承認",
                  "record": { "table": TABLE, "id": { "$from": "input", "path": "/id" } },
                  "assignees": [{ "field": "approver" }], "approve": { "to": "approved" } } },
                { "id": "wt", "type": "control.wait", "params": { "kind": "event", "source": "data.record.updated", "scope": { "table": TABLE } } }
            ]),
            &json!([{ "kind": "event", "source": "data.record.created", "scope": { "table": TABLE } }]),
        );
        use AccessKind::*;
        use AccessObject::*;
        assert_eq!(
            got,
            vec![
                need("ok", Table, Some(TABLE), Read),
                need("wt", Table, Some(TABLE), Read),
            ]
        );
    }

    fn conditional_of(ir: &serde_json::Value) -> Vec<(String, bool)> {
        let ir = WorkflowIr::from_json(ir).expect("test IR");
        RegistrationService::access_needs(&ir)
            .into_iter()
            .map(|n| (n.node_id, n.conditional))
            .collect()
    }

    /// 分かれた流れの片方・「エラー時」の出口の先だけが使う対象は、道によって要ると印を付ける。毎回通るブロックの
    /// 対象は印を付けない（BUG-216: 通らない入力でも使う人のボタンを止めない）。
    #[test]
    fn needs_on_branches_and_error_paths_are_conditional() {
        let got = conditional_of(&json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["data.read", "data.write"],
            "nodes": [
                { "id": "first", "type": "data.query", "params": { "table": TABLE } },
                { "id": "add", "type": "data.record.create", "params": { "table": TABLE, "data": {} }, "on_error": "continue" },
                { "id": "b", "type": "control.branch", "params": { "condition": { "cmp": { "left": 1, "op": "eq", "right": 1 } } } },
                { "id": "big", "type": "data.record.delete", "params": { "table": FOLDER } },
                { "id": "fix", "type": "data.record.update", "params": { "table": FILE } }
            ],
            "edges": [
                { "from": "first", "to": "add" },
                { "from": "add", "to": "b" },
                { "from": "b", "from_port": "true", "to": "big" },
                { "from": "add", "from_port": "error", "to": "fix" }
            ]
        }));
        let got: Vec<(&str, bool)> = got.iter().map(|(id, c)| (id.as_str(), *c)).collect();
        assert_eq!(
            got,
            vec![
                ("first", false),
                ("add", true),
                ("big", true),
                ("fix", true)
            ]
        );
    }

    /// 毎回動くブロックでも、失敗を「エラー時」の出口で扱う（`on_error: continue` で `error` の出口に線がある）なら、
    /// その権限は場合によって要る（足りなければ作った人が決めた誤りの返答で終わるので、使う人のボタンと「いま実行
    /// する」を止めない）。`continue` でも `error` の出口に線が無いブロックは、毎回要るまま。
    #[test]
    fn needs_of_blocks_that_handle_their_own_failure_are_conditional() {
        let wf = |error_edge: bool| {
            let mut edges = vec![json!({ "from": "add", "to": "done" })];
            if error_edge {
                edges.push(json!({ "from": "add", "from_port": "error", "to": "sorry" }));
            }
            json!({
                "ir_version": 1, "name": "wf", "declared_scopes": ["data.write"],
                "triggers": [{ "kind": "interactive" }],
                "nodes": [
                    { "id": "add", "type": "data.record.create", "params": { "table": TABLE, "data": {} }, "on_error": "continue" },
                    { "id": "done", "type": "workflow.reply", "params": { "text": "申請しました" } },
                    { "id": "sorry", "type": "workflow.reply", "params": { "text": "申請先に書けません", "tone": "error" } }
                ],
                "edges": edges
            })
        };
        assert_eq!(conditional_of(&wf(true)), vec![("add".to_string(), true)]);
        assert_eq!(conditional_of(&wf(false)), vec![("add".to_string(), false)]);
    }

    /// GAP-97: 毎回動く繰り返しの中で 1 件ずつテーブルに足すなら、追加の権限は毎回要る（使う人のボタンを止める）。
    /// 「失敗を記録して続ける」繰り返しの中と、分かれた流れの先の繰り返しの中は、場合によって要る。
    #[test]
    fn needs_inside_always_running_maps_are_not_conditional() {
        let wf = |on_item_error: &str, after_branch: bool| {
            let mut edges = vec![json!({ "from": "find", "to": "each" })];
            if after_branch {
                edges = vec![
                    json!({ "from": "find", "to": "b" }),
                    json!({ "from": "b", "from_port": "true", "to": "each" }),
                ];
            }
            json!({
                "ir_version": 1, "name": "wf", "declared_scopes": ["data.read", "data.write"],
                "triggers": [{ "kind": "interactive" }],
                "nodes": [
                    { "id": "find", "type": "data.query", "params": { "table": TABLE } },
                    { "id": "b", "type": "control.branch", "params": { "condition": { "cmp": { "left": 1, "op": "eq", "right": 1 } } } },
                    { "id": "each", "type": "control.map", "params": {
                        "items": { "$from": "nodes.find.output", "path": "/rows" }, "on_item_error": on_item_error } },
                    { "id": "add", "type": "data.record.create", "parent": "each",
                      "params": { "table": FOLDER, "data": { "src": { "$from": "each", "path": "/item/id" } } } }
                ],
                "edges": edges
            })
        };
        let add = |ir: &serde_json::Value| {
            conditional_of(ir)
                .into_iter()
                .find(|(id, _)| id == "add")
                .map(|(_, c)| c)
        };
        assert_eq!(add(&wf("fail_map", false)), Some(false));
        assert_eq!(add(&wf("collect", false)), Some(true));
        assert_eq!(add(&wf("fail_map", true)), Some(true));
    }

    /// スクリプト（使ってよい操作にテーブルなどがあるとき）・AI エージェントの読むツール・文書検索・シークレット・別の
    /// ワークフローを実行・ドライブのトップへの保存と一覧は、前もって確かめられないブロックとして名前つきで挙げる
    /// （GAP-82）。文書検索・ドライブのトップの一覧・AI エージェントは、使う人が見られるものだけを使う同じ理由で、
    /// そろえて挙げる。外へ届かないスクリプトと、社内を読まない AI エージェントは挙げない。
    #[test]
    fn blocks_beyond_the_check_are_listed_by_name() {
        let ir = |scopes: serde_json::Value| {
            WorkflowIr::from_json(&json!({
                "ir_version": 1, "name": "wf", "declared_scopes": scopes,
                "nodes": [
                    { "id": "s", "type": "script.run", "label": "金額を計算", "params": { "source": { "inline": "x" } } },
                    { "id": "ag", "type": "agent.invoke", "params": { "instruction": "x", "allowed_tools": ["storage.read", "doc.read"] } },
                    { "id": "web", "type": "agent.invoke", "params": { "instruction": "x", "allowed_tools": ["web.search"] } },
                    { "id": "rs", "type": "rag.search", "params": { "query": "x" } },
                    { "id": "h", "type": "http.request", "params": { "url": "https://example.com", "secret": { "name": "k" } } },
                    { "id": "st", "type": "workflow.start", "params": { "name": "child" } },
                    { "id": "top", "type": "storage.write", "params": { "name": "a.txt", "content": "x" } },
                    { "id": "ls", "type": "storage.list", "params": {} }
                ],
                "edges": []
            }))
            .expect("test IR")
        };
        let kinds = |ir: &WorkflowIr| -> Vec<(String, String, AccessUnchecked)> {
            RegistrationService::access_gaps(ir)
                .into_iter()
                .map(|g| (g.node_id, g.block, g.kind))
                .collect()
        };
        use AccessUnchecked::*;
        let got = kinds(&ir(json!([
            "data.write",
            "http.egress",
            "workflow.start",
            "storage.write",
            "rag.query"
        ])));
        let ids: Vec<(&str, AccessUnchecked)> = got.iter().map(|g| (g.0.as_str(), g.2)).collect();
        assert_eq!(
            ids,
            vec![
                ("s", Script),
                ("ag", AgentTools),
                ("rs", DocSearch),
                ("h", Secret),
                ("st", StartWorkflow),
                ("top", DriveTop),
                ("ls", DriveTopList)
            ]
        );
        assert_eq!(got[0].1, "金額を計算", "付けた名前で言う");
        // 外へ届く操作が無いスクリプトは挙げない。
        let got = kinds(&ir(json!(["rag.query"])));
        assert!(got.iter().all(|g| g.2 != Script), "{got:?}");
        // 前の結果でフォルダーを決める一覧は、ドライブのトップではない（要る権限に、対象を決めずに挙げる）。
        let chosen = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["storage.read"],
            "nodes": [{ "id": "ls", "type": "storage.list", "params": { "folder": { "$from": "input", "path": "/folder" } } }],
            "edges": []
        }))
        .expect("test IR");
        assert!(kinds(&chosen).is_empty());
        assert_eq!(
            RegistrationService::access_needs(&chosen),
            vec![need("ls", AccessObject::Folder, None, AccessKind::Read)]
        );
        // 要る権限には挙げない（確かめない）。
        let needs = RegistrationService::access_needs(&ir(json!(["data.write", "storage.write"])));
        assert!(needs.is_empty(), "{needs:?}");
    }

    /// UUID でないリテラル（壊れた欄）は対象を決めずに挙げる（存在しない対象を確かめない）。
    #[test]
    fn non_uuid_literals_have_no_target() {
        let got = needs(
            &json!([{ "id": "add", "type": "data.record.create", "params": { "table": "経費", "data": {} } }]),
            &json!([]),
        );
        assert_eq!(
            got,
            vec![need("add", AccessObject::Table, None, AccessKind::Add)]
        );
    }
}
