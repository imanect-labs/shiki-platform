//! V4: ブロックときっかけが名指しするテーブル・フォルダー・ファイルの実在（ir.md §8 V4・GAP-35・GAP-91）。
//!
//! 業務データのブロック（データを調べる・追加・更新・追加か更新・削除・まとめて登録）・データ変更のきっかけ
//! （追加・更新）・データを待つブロック（wait(event)）のテーブルと、ファイルを保存・フォルダ一覧・CSV を保存の
//! フォルダー、ファイル保存のきっかけとそれを待つブロックのフォルダー、ファイルを読む・CSV を集計・CSV を編集の
//! ファイル（CSV を集計は JOIN のために足す CSV も）が、保存する人に見えるものであること。
//! 見えないもの（共有されていない・ほかの組織のもの）と無いもの（削除された・書き損じた id）は、同じ code と文言に
//! する（存在を明かさない・付録 C の P5）。承認と状態を進めるのテーブルは、状態の進め方も要るので
//! [`super::approval`]・[`super::transition`] が照らす。
//!
//! 照らすのはリテラルで書いた id だけ（参照で決めるものは実行時まで分からない・実行時は実行主体の権限で断る）。空の
//! 欄は照らさない（置いたばかりのブロックの空の欄は、必須の欄の注意が言う・BUG-151）。材料は API 層が保存者の権限で
//! 引いて詰める（[`Catalog::table_ids`]・[`Catalog::folder_ids`]・[`Catalog::file_ids`]・`None` なら照らさない）。何を
//! 引くかは [`referenced_tables`]・[`referenced_folders`]・[`referenced_files`] が決める（照合と同じ位置の表を使う）。
//! 保存のときだけの照合で、保存済みの版の実行は変えない（実行は実行主体の権限で断る）。

use std::collections::BTreeSet;

use serde_json::Value;
use uuid::Uuid;

use super::{Catalog, ValidationError};
use crate::ir::{Trigger, WorkflowIr};
use crate::vocab::{EventSource, NodeType};

/// 誤りのコード: 見つからない（見えない）テーブル（承認・状態を進めると同じ）。
const UNKNOWN_TABLE: &str = "ir.unknown_table";
/// 誤りのコード: 見つからない（見えない）フォルダー。
pub const UNKNOWN_FOLDER: &str = "ir.unknown_folder";
/// 誤りのコード: 見つからない（見えない）ファイル（GAP-91）。
pub const UNKNOWN_FILE: &str = "ir.unknown_file";

/// 名指しの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Table,
    Folder,
    File,
}

/// 名指しを照らすところ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// ここ（実在だけ）。
    Here,
    /// 承認・状態を進める（状態の進め方も要るので、それぞれの照合が言う）。
    Elsewhere,
}

/// IR の中の、リテラルで名指ししたテーブル・フォルダー 1 つ。
struct Site<'a> {
    kind: Kind,
    owner: Owner,
    raw: &'a str,
    /// ブロックなら id（位置はブロックの中）。きっかけなら `None`（位置は IR の根から）。
    node: Option<&'a str>,
    path: String,
    /// id の形（UUID か）を V3 が照らす位置か（きっかけと待つブロックの scope・同じ誤りを二重に言わない）。
    shaped: bool,
}

/// 文字のリテラル（空・空白だけは名指しに数えない）。
fn literal(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

/// IR の中の名指しを、照合の位置ごとに挙げる。
fn sites<'a>(ir: &'a WorkflowIr) -> Vec<Site<'a>> {
    let mut out = Vec::new();
    for node in &ir.nodes {
        let Some(nt) = NodeType::parse(&node.node_type) else {
            continue;
        };
        let p = &node.params;
        let mut add = |kind, owner, raw: Option<&'a str>, path: &str| {
            if let Some(raw) = raw {
                out.push(Site {
                    kind,
                    owner,
                    raw,
                    node: Some(node.id.as_str()),
                    path: path.to_string(),
                    shaped: path.starts_with("/params/scope/"),
                });
            }
        };
        match nt {
            NodeType::DataQuery
            | NodeType::DataRecordCreate
            | NodeType::DataRecordUpdate
            | NodeType::DataRecordUpsert
            | NodeType::DataRecordDelete
            | NodeType::DataBulkUpsert => {
                add(
                    Kind::Table,
                    Owner::Here,
                    literal(p.get("table")),
                    "/params/table",
                );
            }
            NodeType::DataTransition => add(
                Kind::Table,
                Owner::Elsewhere,
                literal(p.get("table")),
                "/params/table",
            ),
            NodeType::HumanApproval => add(
                Kind::Table,
                Owner::Elsewhere,
                literal(p.pointer("/record/table")),
                "/params/record/table",
            ),
            NodeType::StorageWrite | NodeType::StorageList | NodeType::CsvWrite => {
                add(
                    Kind::Folder,
                    Owner::Here,
                    literal(p.get("folder")),
                    "/params/folder",
                );
            }
            // ドライブから選んだファイル（GAP-91）。CSV を集計は、JOIN のために足す CSV（`tables` の各 `file`）も。
            NodeType::StorageRead | NodeType::CsvQuery | NodeType::CsvPatch => {
                add(
                    Kind::File,
                    Owner::Here,
                    literal(p.get("file")),
                    "/params/file",
                );
                if nt == NodeType::CsvQuery {
                    let tables = p.get("tables").and_then(Value::as_array);
                    for (i, item) in tables.into_iter().flatten().enumerate() {
                        add(
                            Kind::File,
                            Owner::Here,
                            literal(item.get("file")),
                            &format!("/params/tables/{i}/file"),
                        );
                    }
                }
            }
            NodeType::ControlWait if p.get("kind").and_then(Value::as_str) == Some("event") => {
                let source = p
                    .get("source")
                    .and_then(Value::as_str)
                    .and_then(EventSource::parse);
                match source {
                    Some(s) if s.is_data() => add(
                        Kind::Table,
                        Owner::Here,
                        literal(p.pointer("/scope/table")),
                        "/params/scope/table",
                    ),
                    Some(EventSource::StorageWrite) => add(
                        Kind::Folder,
                        Owner::Here,
                        literal(p.pointer("/scope/folder")),
                        "/params/scope/folder",
                    ),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    for (i, t) in ir.triggers.iter().enumerate() {
        let Trigger::Event(e) = t else { continue };
        let (kind, owner, key) = match EventSource::parse(&e.source) {
            Some(EventSource::DataTransition) => (Kind::Table, Owner::Elsewhere, "table"),
            Some(s) if s.is_data() => (Kind::Table, Owner::Here, "table"),
            Some(EventSource::StorageWrite) => (Kind::Folder, Owner::Here, "folder"),
            _ => continue,
        };
        if let Some(raw) = literal(e.scope.get(key)) {
            out.push(Site {
                kind,
                owner,
                raw,
                node: None,
                path: format!("/triggers/{i}/scope/{key}"),
                shaped: true,
            });
        }
    }
    out
}

/// IR が名指しするテーブル（V4 の材料として API 層が保存者の権限で引く・承認と状態を進めるのテーブルも含む）。
#[must_use]
pub fn referenced_tables(ir: &WorkflowIr) -> BTreeSet<Uuid> {
    ids_of(ir, Kind::Table)
}

/// IR が名指しするフォルダー（V4 の材料として API 層が保存者の権限で引く）。
#[must_use]
pub fn referenced_folders(ir: &WorkflowIr) -> BTreeSet<Uuid> {
    ids_of(ir, Kind::Folder)
}

/// IR が名指しするファイル（V4 の材料として API 層が保存者の権限で引く・GAP-91）。
#[must_use]
pub fn referenced_files(ir: &WorkflowIr) -> BTreeSet<Uuid> {
    ids_of(ir, Kind::File)
}

fn ids_of(ir: &WorkflowIr, kind: Kind) -> BTreeSet<Uuid> {
    sites(ir)
        .into_iter()
        .filter(|s| s.kind == kind)
        .filter_map(|s| Uuid::parse_str(s.raw).ok())
        .collect()
}

/// 材料の鍵の形（小文字・ハイフンつき）。UUID でなければ `None`（書き損じた id は見つからないものと同じ）。
fn key_of(raw: &str) -> Option<String> {
    Uuid::parse_str(raw)
        .ok()
        .map(|u| u.hyphenated().to_string())
}

/// V4: 名指ししたテーブル・フォルダー・ファイルが保存者に見えるか。
pub(super) fn v4(ir: &WorkflowIr, catalog: &Catalog, errors: &mut Vec<ValidationError>) {
    for site in sites(ir).into_iter().filter(|s| s.owner == Owner::Here) {
        let (known, code, what) = match site.kind {
            Kind::Table => (catalog.table_ids.as_ref(), UNKNOWN_TABLE, "テーブル"),
            Kind::Folder => (catalog.folder_ids.as_ref(), UNKNOWN_FOLDER, "フォルダー"),
            Kind::File => (catalog.file_ids.as_ref(), UNKNOWN_FILE, "ファイル"),
        };
        let Some(known) = known else { continue };
        match key_of(site.raw) {
            Some(k) if known.contains(&k) => continue,
            // scope の形の誤り（UUID でない）は V3 が言う。
            None if site.shaped => continue,
            _ => {}
        }
        let head = if site.node.is_some() {
            ""
        } else {
            "きっかけ: "
        };
        let err = ValidationError::new(
            code,
            format!(
                "{head}選んだ{what}が見つかりません。削除されたか、見る権限がありません。{what}を選び直してください。"
            ),
        )
        .at_path(site.path);
        errors.push(match site.node {
            Some(id) => err.at_node(id),
            None => err,
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// 誤りの（code, ブロック, 位置）。
    type At<'a> = (&'a str, Option<&'a str>, Option<&'a str>);

    const SEEN: &str = "00000000-0000-0000-0000-0000000000a1";
    const GONE: &str = "00000000-0000-0000-0000-0000000000b2";
    const DIR: &str = "00000000-0000-0000-0000-0000000000c3";
    const LOST: &str = "00000000-0000-0000-0000-0000000000d4";
    const DOC: &str = "00000000-0000-0000-0000-0000000000e5";
    const TRASHED: &str = "00000000-0000-0000-0000-0000000000f6";

    fn ir() -> WorkflowIr {
        WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf",
            "triggers": [
                { "kind": "event", "source": "data.record.created", "scope": { "table": GONE } },
                { "kind": "event", "source": "data.record.updated", "scope": { "table": SEEN.to_uppercase() } },
                { "kind": "event", "source": "storage.write", "scope": { "folder": LOST } },
                { "kind": "event", "source": "data.transition", "scope": { "table": GONE } },
                { "kind": "event", "source": "data.record.created", "scope": { "table": "経費" } }
            ],
            "nodes": [
                { "id": "q", "type": "data.query", "params": { "table": SEEN } },
                { "id": "add", "type": "data.record.create", "params": { "table": GONE, "data": {} } },
                { "id": "named", "type": "data.record.delete", "params": { "table": "経費", "record": "x" } },
                { "id": "ref", "type": "data.record.update", "params": {
                    "table": { "$from": "input", "path": "/t" }, "record": "x", "patch": { "a": 1 } } },
                { "id": "blank", "type": "data.bulk_upsert", "params": { "table": "" } },
                { "id": "move", "type": "data.transition", "params": { "table": GONE, "record": "x", "to": "a" } },
                { "id": "save", "type": "storage.write", "params": { "folder": LOST, "name": "a", "content": "b" } },
                { "id": "list", "type": "storage.list", "params": { "folder": DIR } },
                { "id": "csv", "type": "csv.write", "params": { "name": "a.csv", "rows": [] } },
                { "id": "read", "type": "storage.read", "params": { "file": DOC.to_uppercase() } },
                { "id": "sum", "type": "csv.query", "params": { "file": TRASHED, "sql": "select 1" } },
                { "id": "edit", "type": "csv.patch", "params": { "file": "請求.csv", "ops": [] } },
                { "id": "pick", "type": "storage.read", "params": { "file": { "$from": "input", "path": "/f" } } },
                { "id": "wait", "type": "control.wait", "params": {
                    "kind": "event", "source": "data.record.updated", "scope": { "table": GONE } } }
            ],
            "edges": []
        }))
        .unwrap()
    }

    fn catalog() -> Catalog {
        Catalog {
            table_ids: Some([SEEN.to_string()].into_iter().collect()),
            folder_ids: Some([DIR.to_string()].into_iter().collect()),
            file_ids: Some([DOC.to_string()].into_iter().collect()),
            ..Catalog::default()
        }
    }

    /// 見えない・無い・書き損じたテーブル・フォルダー・ファイルは、同じ文言で位置つきで止める（参照・空の欄は照らさない。
    /// きっかけの scope の書き損じは V3 が言う）。
    #[test]
    fn unseen_tables_and_folders_are_refused_where_they_are_named() {
        let mut errors = Vec::new();
        v4(&ir(), &catalog(), &mut errors);
        let got: Vec<At<'_>> = errors
            .iter()
            .map(|e| (e.code.as_str(), e.node_id.as_deref(), e.path.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (UNKNOWN_TABLE, Some("add"), Some("/params/table")),
                (UNKNOWN_TABLE, Some("named"), Some("/params/table")),
                (UNKNOWN_FOLDER, Some("save"), Some("/params/folder")),
                (UNKNOWN_FILE, Some("sum"), Some("/params/file")),
                (UNKNOWN_FILE, Some("edit"), Some("/params/file")),
                (UNKNOWN_TABLE, Some("wait"), Some("/params/scope/table")),
                (UNKNOWN_TABLE, None, Some("/triggers/0/scope/table")),
                (UNKNOWN_FOLDER, None, Some("/triggers/2/scope/folder")),
            ]
        );
        assert_eq!(
            errors[0].message,
            "選んだテーブルが見つかりません。削除されたか、見る権限がありません。テーブルを選び直してください。"
        );
        assert_eq!(
            errors[3].message,
            "選んだファイルが見つかりません。削除されたか、見る権限がありません。ファイルを選び直してください。"
        );
        assert_eq!(
            errors[7].message,
            "きっかけ: 選んだフォルダーが見つかりません。削除されたか、見る権限がありません。フォルダーを選び直してください。"
        );
        // 材料を引かない構成（None）では照らさない。
        let mut none = Vec::new();
        v4(&ir(), &Catalog::default(), &mut none);
        assert!(none.is_empty());
    }

    /// CSV を集計の JOIN のために足す CSV（`tables` の各 `file`）も、`file` と同じく照らし、引く材料に入れる（値式と
    /// 空の欄は照らさない。`tables` 自体を値式で書いたときも照らさない）。
    #[test]
    fn joined_csv_files_are_checked_too() {
        let ir = |tables: Value| {
            WorkflowIr::from_json(&json!({
                "ir_version": 1, "name": "wf",
                "nodes": [{ "id": "sum", "type": "csv.query",
                            "params": { "file": DOC, "sql": "select 1", "tables": tables } }],
                "edges": []
            }))
            .unwrap()
        };
        let listed = ir(json!([
            { "name": "master", "file": DOC },
            { "name": "gone", "file": TRASHED },
            { "name": "typo", "file": "請求.csv" },
            { "name": "picked", "file": { "$from": "input", "path": "/f" } },
            { "name": "blank", "file": "" }
        ]));
        let mut errors = Vec::new();
        v4(&listed, &catalog(), &mut errors);
        let got: Vec<At<'_>> = errors
            .iter()
            .map(|e| (e.code.as_str(), e.node_id.as_deref(), e.path.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (UNKNOWN_FILE, Some("sum"), Some("/params/tables/1/file")),
                (UNKNOWN_FILE, Some("sum"), Some("/params/tables/2/file")),
            ]
        );
        let parse = |s: &str| Uuid::parse_str(s).unwrap();
        assert_eq!(
            referenced_files(&listed),
            [parse(DOC), parse(TRASHED)].into_iter().collect()
        );
        let dynamic = ir(json!({ "$from": "input", "path": "/tables" }));
        let mut none = Vec::new();
        v4(&dynamic, &catalog(), &mut none);
        assert!(none.is_empty(), "{none:?}");
    }

    /// 引く材料は、承認・状態を進めるのテーブルも含めたリテラルの id だけ（書き損じ・参照は引かない）。
    #[test]
    fn referenced_ids_are_the_literal_ones() {
        let parse = |s: &str| Uuid::parse_str(s).unwrap();
        assert_eq!(
            referenced_tables(&ir()),
            [parse(SEEN), parse(GONE)].into_iter().collect()
        );
        assert_eq!(
            referenced_folders(&ir()),
            [parse(DIR), parse(LOST)].into_iter().collect()
        );
        assert_eq!(
            referenced_files(&ir()),
            [parse(DOC), parse(TRASHED)].into_iter().collect()
        );
    }
}
