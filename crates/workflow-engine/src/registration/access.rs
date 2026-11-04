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
//! 3. **きっかけ・AI の参照範囲は確かめない**。きっかけは自動実行の委譲で読むもので、手動の実行では使わない。
//!    AI エージェント・文書検索の範囲は「選んだフォルダーの中で、使う人が見られる文書だけ」で、足りなくても失敗しない。
//!    フォルダーを省いた保存・一覧はドライブのトップで、組織のメンバーなら誰でも書ける・読める。
//!
//! 対象が入力や前の結果で決まるとき（値の式）・まだ選んでいないときは、対象を決めずに挙げる（`object_id = None`。
//! 呼び出し側は確かめずに「実行のときに決まる」と言う）。シークレット・別のワークフローは名前で引くので、ここでは挙げない。

use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use super::consent::suggested_grants;
use super::{RegistrationService, SuggestedGrant};
use crate::ir::WorkflowIr;
use crate::vocab::NodeType;

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
}

impl RegistrationService {
    /// 手動の実行で本人に要る権限を IR から挙げる（純関数・並びはブロックの順・同じものは 1 つ）。
    ///
    /// 読む・書くの区別と対象は、同意の提案と同じ定義から読み替える（[モジュールの説明](self)）。
    #[must_use]
    pub fn access_needs(ir: &WorkflowIr) -> Vec<AccessNeed> {
        let mut out: Vec<AccessNeed> = Vec::new();
        for grant in suggested_grants(ir) {
            let Some(need) = need_of(ir, &grant) else {
                continue;
            };
            if !out.contains(&need) {
                out.push(need);
            }
        }
        out
    }
}

/// 同意の提案 1 つを、手動の実行で本人に要る権限に読み替える（読み替えない提案は `None`）。
fn need_of(ir: &WorkflowIr, grant: &SuggestedGrant) -> Option<AccessNeed> {
    // きっかけ（trigger:i）は自動実行の委譲で読むもの（手動の実行では使わない）。
    let node_id = grant.source.strip_prefix("node:")?;
    let node = ir.nodes.iter().find(|n| n.id == node_id)?;
    let node_type = NodeType::parse(&node.node_type)?;
    // AI エージェント・文書検索の範囲は、使う人が見られる文書だけを使う（足りなくても失敗しない）。
    if matches!(node_type, NodeType::AgentInvoke | NodeType::RagSearch) {
        return None;
    }
    let object = AccessObject::from_object_kind(&grant.object_kind)?;
    let object_id = grant
        .object_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s.trim()).ok());
    // フォルダーを省いた保存・一覧はドライブのトップ（組織のメンバーなら誰でも書ける・読める）。
    if object == AccessObject::Folder
        && object_id.is_none()
        && node
            .params
            .get("folder")
            .is_none_or(serde_json::Value::is_null)
    {
        return None;
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
    Some(AccessNeed {
        node_id: node_id.to_string(),
        object,
        object_id,
        kind,
    })
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
    /// なので挙げない。値の式の対象は対象を決めずに挙げる。
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
