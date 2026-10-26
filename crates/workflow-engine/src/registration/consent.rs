//! 同意計画（consent_plan）の静的分析（registration.rs の行数ゲート対応で分離）。

use super::{RegistrationService, SuggestedGrant};
use crate::ir::{Trigger, WorkflowIr};

impl RegistrationService {
    /// 同意画面の提案 grants を IR から静的に列挙する（純関数・最終選択は有効化者）。
    #[must_use]
    #[allow(clippy::too_many_lines)] // ノード種ごとの提案の表（exhaustive に並べる方が漏れを見つけやすい）。
    pub fn consent_plan(ir: &WorkflowIr) -> Vec<SuggestedGrant> {
        let mut out: Vec<SuggestedGrant> = Vec::new();
        let mut push = |g: SuggestedGrant| {
            if !out.iter().any(|e| {
                e.scope == g.scope
                    && e.object_kind == g.object_kind
                    && e.object_id == g.object_id
                    && e.object_name == g.object_name
                    && e.relation == g.relation
            }) {
                out.push(g);
            }
        };

        // イベントトリガの束縛: トリガ元コンテキストの読取提案（典型パターン）。データ変更のトリガは、
        // 委譲元（テーブルの viewer を委譲した人）の見え方で変更を読むので、テーブルの閲覧を提案する（GAP-07）。
        for (i, t) in ir.triggers.iter().enumerate() {
            if let Trigger::Event(ev) = t {
                if let Some(folder) = ev.scope.get("folder").and_then(|v| v.as_str()) {
                    push(SuggestedGrant {
                        scope: "storage.read".into(),
                        object_kind: "folder".into(),
                        object_id: Some(folder.to_string()),
                        object_name: None,
                        relation: "viewer".into(),
                        source: format!("trigger:{i}"),
                        needs_user_pick: false,
                    });
                }
                if let Some(table) = ev.scope.get("table").and_then(|v| v.as_str()) {
                    push(SuggestedGrant {
                        scope: "data.read".into(),
                        object_kind: "data_table".into(),
                        object_id: Some(table.to_string()),
                        object_name: None,
                        relation: "viewer".into(),
                        source: format!("trigger:{i}"),
                        needs_user_pick: false,
                    });
                }
            }
        }

        for node in &ir.nodes {
            let source = format!("node:{}", node.id);
            let Some(nt) = crate::vocab::NodeType::parse(&node.node_type) else {
                continue;
            };
            match nt {
                crate::vocab::NodeType::StorageRead => {
                    let file = literal_str(&node.params, "file");
                    push(SuggestedGrant {
                        scope: "storage.read".into(),
                        object_kind: "file".into(),
                        object_id: file.clone(),
                        object_name: None,
                        relation: "viewer".into(),
                        source,
                        needs_user_pick: file.is_none(),
                    });
                }
                crate::vocab::NodeType::StorageList => {
                    let folder = literal_str(&node.params, "folder");
                    push(SuggestedGrant {
                        scope: "storage.read".into(),
                        object_kind: "folder".into(),
                        object_id: folder.clone(),
                        object_name: None,
                        relation: "viewer".into(),
                        source,
                        needs_user_pick: folder.is_none(),
                    });
                }
                crate::vocab::NodeType::StorageWrite => {
                    let folder = literal_str(&node.params, "folder");
                    push(SuggestedGrant {
                        scope: "storage.write".into(),
                        object_kind: "folder".into(),
                        object_id: folder.clone(),
                        object_name: None,
                        relation: "editor".into(),
                        source,
                        needs_user_pick: folder.is_none(),
                    });
                }
                // CSV 表データ（Task 11P.9）: 実行は TabularService のファイル ReBAC で決まる
                // （query = viewer@file・patch = editor@file・write = 保存先フォルダの作成権限）。
                // schedule / event の run は委譲が無いと「csv 権限なし」で落ちるので、storage.* と
                // 同じ形で対象を提案する（BUG-10）。
                crate::vocab::NodeType::CsvQuery | crate::vocab::NodeType::CsvPatch => {
                    let file = literal_str(&node.params, "file");
                    let read = nt == crate::vocab::NodeType::CsvQuery;
                    push(SuggestedGrant {
                        scope: if read { "csv.read" } else { "csv.write" }.into(),
                        object_kind: "file".into(),
                        object_id: file.clone(),
                        object_name: None,
                        relation: if read { "viewer" } else { "editor" }.into(),
                        source,
                        needs_user_pick: file.is_none(),
                    });
                }
                crate::vocab::NodeType::CsvWrite => {
                    let folder = literal_str(&node.params, "folder");
                    push(SuggestedGrant {
                        scope: "csv.write".into(),
                        object_kind: "folder".into(),
                        object_id: folder.clone(),
                        object_name: None,
                        relation: "editor".into(),
                        source,
                        needs_user_pick: folder.is_none(),
                    });
                }
                crate::vocab::NodeType::RagSearch => {
                    // 検索範囲はユーザーが選ぶ（実効 = 委譲 ∩ pre/post filter）。
                    push(SuggestedGrant {
                        scope: "rag.query".into(),
                        object_kind: "folder".into(),
                        object_id: None,
                        object_name: None,
                        relation: "viewer".into(),
                        source,
                        needs_user_pick: true,
                    });
                }
                crate::vocab::NodeType::HttpRequest => {
                    if let Some(name) = node
                        .params
                        .get("secret")
                        .and_then(|s| s.get("name"))
                        .and_then(|v| v.as_str())
                    {
                        push(SuggestedGrant {
                            scope: "http.egress".into(),
                            object_kind: "secret".into(),
                            object_id: None,
                            object_name: Some(name.to_string()),
                            relation: "can_use".into(),
                            source,
                            needs_user_pick: false,
                        });
                    }
                }
                // 構造化データ（Task 15.7）: テーブル単位で委譲する（読取 = viewer・書込 = editor）。
                // schedule / event の run は委譲元ユーザーの権限で行が絞られる（ポート実装）。
                crate::vocab::NodeType::DataQuery
                | crate::vocab::NodeType::DataRecordCreate
                | crate::vocab::NodeType::DataRecordUpdate
                | crate::vocab::NodeType::DataRecordDelete
                | crate::vocab::NodeType::DataTransition
                | crate::vocab::NodeType::DataBulkUpsert => {
                    let table = literal_str(&node.params, "table");
                    let read = nt == crate::vocab::NodeType::DataQuery;
                    push(SuggestedGrant {
                        scope: if read { "data.read" } else { "data.write" }.into(),
                        object_kind: "data_table".into(),
                        object_id: table.clone(),
                        object_name: None,
                        relation: if read { "viewer" } else { "editor" }.into(),
                        source,
                        needs_user_pick: table.is_none(),
                    });
                }
                // wait(event) のフォルダ束縛: 起床は実行主体が保存されたファイルを読めるときだけ
                // （relay の閲覧確認）なので、schedule / event の run にはフォルダの閲覧の委譲が要る。
                // データ変更の wait はテーブルの閲覧（変更は実行主体の見え方で受け取る・GAP-07）。
                // scope の無い購読は委譲したものの中でしか起きないので、提案しない。
                crate::vocab::NodeType::ControlWait => {
                    let scope = node.params.get("scope");
                    let key = |k: &str| {
                        scope
                            .and_then(|s| s.get(k))
                            .and_then(serde_json::Value::as_str)
                    };
                    if node.params.get("kind").and_then(|k| k.as_str()) == Some("event") {
                        if let Some(folder) = key("folder") {
                            push(SuggestedGrant {
                                scope: "storage.read".into(),
                                object_kind: "folder".into(),
                                object_id: Some(folder.to_string()),
                                object_name: None,
                                relation: "viewer".into(),
                                source: source.clone(),
                                needs_user_pick: false,
                            });
                        }
                        if let Some(table) = key("table") {
                            push(SuggestedGrant {
                                scope: "data.read".into(),
                                object_kind: "data_table".into(),
                                object_id: Some(table.to_string()),
                                object_name: None,
                                relation: "viewer".into(),
                                source,
                                needs_user_pick: false,
                            });
                        }
                    }
                }
                // 承認（GAP-08）: 停止時に束縛した行を読み、判断の遷移を実行主体の見え方で受け取るので、
                // schedule / event の run にはテーブルの閲覧の委譲が要る（判断そのものは承認者本人の権限）。
                crate::vocab::NodeType::HumanApproval => {
                    if let Some(table) = node
                        .params
                        .get("record")
                        .and_then(|r| r.get("table"))
                        .and_then(serde_json::Value::as_str)
                    {
                        push(SuggestedGrant {
                            scope: "data.read".into(),
                            object_kind: "data_table".into(),
                            object_id: Some(table.to_string()),
                            object_name: None,
                            relation: "viewer".into(),
                            source,
                            needs_user_pick: false,
                        });
                    }
                }
                // AI エージェント（BUG-6）: 社内を読む道具は、schedule / event の run が委譲したフォルダの中で
                // 読む（実効 = 委譲 ∩ 実行時の ReBAC）。範囲は有効化者が選ぶ。web・コードの実行は対象を
                // 持たない（スコープの同意だけ・宛先は管理者の天井）。
                crate::vocab::NodeType::AgentInvoke => {
                    for scope in agent_folder_scopes(&node.params) {
                        push(SuggestedGrant {
                            scope: scope.into(),
                            object_kind: "folder".into(),
                            object_id: None,
                            object_name: None,
                            relation: "viewer".into(),
                            source: source.clone(),
                            needs_user_pick: true,
                        });
                    }
                }
                crate::vocab::NodeType::WorkflowStart => {
                    let name = literal_str(&node.params, "name");
                    push(SuggestedGrant {
                        scope: "workflow.start".into(),
                        object_kind: "workflow".into(),
                        object_id: None,
                        object_name: name.clone(),
                        relation: "viewer".into(),
                        source,
                        needs_user_pick: name.is_none(),
                    });
                }
                _ => {}
            }
        }
        out
    }
}

/// agent.invoke の道具のうち、フォルダの閲覧の委譲が要るもののスコープ（重複なし・道具の順）。
fn agent_folder_scopes(params: &serde_json::Value) -> Vec<&'static str> {
    use crate::vocab::AgentTool;
    let mut out: Vec<&'static str> = Vec::new();
    let tools = params
        .get("allowed_tools")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter_map(AgentTool::parse);
    for t in tools {
        let scope = match t {
            AgentTool::RagSearch
            | AgentTool::StorageList
            | AgentTool::StorageRead
            | AgentTool::DocRead => t.required_scope().as_str(),
            _ => continue,
        };
        if !out.contains(&scope) {
            out.push(scope);
        }
    }
    out
}

/// params のフィールドが文字列リテラルならその値（`$from`/`$template` は None）。
fn literal_str(params: &serde_json::Value, key: &str) -> Option<String> {
    match params.get(key) {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{RegistrationService, SuggestedGrant};
    use crate::ir::WorkflowIr;
    use serde_json::json;

    const FILE: &str = "11111111-2222-3333-4444-555555555555";
    const FOLDER: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

    fn plan(nodes: &serde_json::Value) -> Vec<SuggestedGrant> {
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf",
            "declared_scopes": ["csv.read", "csv.write"],
            "nodes": nodes, "edges": []
        }))
        .expect("test IR");
        RegistrationService::consent_plan(&ir)
    }

    fn by_source<'a>(plan: &'a [SuggestedGrant], node: &str) -> &'a SuggestedGrant {
        let source = format!("node:{node}");
        let found = plan.iter().find(|g| g.source == source);
        assert!(found.is_some(), "{source} の提案が無い: {plan:?}");
        found.expect("提案がある")
    }

    /// csv.* もリテラルの対象は id 付きで提案する（schedule / event の run が委譲で読める・BUG-10）。
    #[test]
    fn csv_nodes_suggest_literal_files_and_folders() {
        let plan = plan(&json!([
            { "id": "q", "type": "csv.query", "params": { "file": FILE, "sql": "SELECT 1" } },
            { "id": "p", "type": "csv.patch",
              "params": { "file": FILE, "base_rev": 1, "ops": [] } },
            { "id": "w", "type": "csv.write",
              "params": { "folder": FOLDER, "name": "out", "content": "a\n1\n" } }
        ]));
        let q = by_source(&plan, "q");
        assert_eq!(
            (
                q.scope.as_str(),
                q.object_kind.as_str(),
                q.relation.as_str()
            ),
            ("csv.read", "file", "viewer")
        );
        assert_eq!(q.object_id.as_deref(), Some(FILE));
        assert!(!q.needs_user_pick);
        let p = by_source(&plan, "p");
        assert_eq!(
            (
                p.scope.as_str(),
                p.object_kind.as_str(),
                p.relation.as_str()
            ),
            ("csv.write", "file", "editor")
        );
        assert_eq!(p.object_id.as_deref(), Some(FILE));
        assert!(!p.needs_user_pick);
        let w = by_source(&plan, "w");
        assert_eq!(
            (
                w.scope.as_str(),
                w.object_kind.as_str(),
                w.relation.as_str()
            ),
            ("csv.write", "folder", "editor")
        );
        assert_eq!(w.object_id.as_deref(), Some(FOLDER));
        assert!(!w.needs_user_pick);
    }

    /// wait(event) のフォルダは閲覧の委譲を提案する（起床は実行主体が読めるファイルの保存だけ）。
    /// scope の無い wait・時間の wait は提案しない。
    #[test]
    fn wait_event_folder_suggests_a_viewer_grant() {
        let plan = plan(&json!([
            { "id": "w", "type": "control.wait",
              "params": { "kind": "event", "source": "storage.write", "scope": { "folder": FOLDER } } },
            { "id": "any", "type": "control.wait",
              "params": { "kind": "event", "source": "storage.write", "scope": {} } },
            { "id": "t", "type": "control.wait", "params": { "kind": "duration", "duration_sec": 60 } }
        ]));
        let w = by_source(&plan, "w");
        assert_eq!(
            (
                w.scope.as_str(),
                w.object_kind.as_str(),
                w.relation.as_str()
            ),
            ("storage.read", "folder", "viewer")
        );
        assert_eq!(w.object_id.as_deref(), Some(FOLDER));
        assert!(!w.needs_user_pick);
        assert_eq!(plan.len(), 1, "{plan:?}");
    }

    /// データ変更の wait(event) もテーブルの閲覧の委譲を提案する（GAP-07 の T3）。
    #[test]
    fn data_wait_suggests_a_table_viewer_grant() {
        let plan = plan(&json!([
            { "id": "w", "type": "control.wait",
              "params": { "kind": "event", "source": "data.record.updated", "scope": { "table": FILE } } }
        ]));
        let w = by_source(&plan, "w");
        assert_eq!(
            (
                w.scope.as_str(),
                w.object_kind.as_str(),
                w.relation.as_str()
            ),
            ("data.read", "data_table", "viewer")
        );
        assert_eq!(w.object_id.as_deref(), Some(FILE));
    }

    /// 承認のテーブルは閲覧の委譲を提案する（停止時に行を読み、判断を実行主体の見え方で受け取る・GAP-08）。
    #[test]
    fn approval_suggests_a_table_viewer_grant() {
        let plan = plan(&json!([
            { "id": "ok", "type": "human.approval", "params": { "title": "承認",
              "record": { "table": FILE, "id": { "$from": "trigger", "path": "/record_id" } },
              "assignees": [{ "field": "approver" }], "approve": { "to": "approved" } } }
        ]));
        let g = by_source(&plan, "ok");
        assert_eq!(
            (
                g.scope.as_str(),
                g.object_kind.as_str(),
                g.relation.as_str()
            ),
            ("data.read", "data_table", "viewer")
        );
        assert_eq!(g.object_id.as_deref(), Some(FILE));
        assert!(!g.needs_user_pick);
    }

    /// データ変更トリガのテーブルは閲覧の委譲を提案する（変更は委譲元の見え方で読む・GAP-07）。
    #[test]
    fn data_change_trigger_suggests_a_table_viewer_grant() {
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "declared_scopes": ["data.read"],
            "triggers": [{ "kind": "event", "source": "data.record.created", "scope": { "table": FILE } }],
            "nodes": [], "edges": []
        }))
        .expect("test IR");
        let plan = RegistrationService::consent_plan(&ir);
        assert_eq!(plan.len(), 1, "{plan:?}");
        let g = &plan[0];
        assert_eq!(
            (
                g.scope.as_str(),
                g.object_kind.as_str(),
                g.relation.as_str()
            ),
            ("data.read", "data_table", "viewer")
        );
        assert_eq!(g.object_id.as_deref(), Some(FILE));
        assert_eq!(g.source, "trigger:0");
        assert!(!g.needs_user_pick);
    }

    /// AI エージェントの社内を読む道具は、読む範囲のフォルダを有効化者が選ぶ（web・コードは対象を持たない）。
    #[test]
    fn agent_tools_suggest_folder_picks() {
        let plan = plan(&json!([
            { "id": "ag", "type": "agent.invoke", "params": { "instruction": "x",
              "allowed_tools": ["rag.search", "storage.read", "storage.list", "doc.read", "web.search", "code.run"] } },
            { "id": "none", "type": "agent.invoke", "params": { "instruction": "y" } }
        ]));
        let scopes: Vec<&str> = plan.iter().map(|g| g.scope.as_str()).collect();
        assert_eq!(
            scopes,
            ["rag.query", "storage.read", "doc.read"],
            "{plan:?}"
        );
        for g in &plan {
            assert_eq!(g.source, "node:ag");
            assert_eq!(
                (g.object_kind.as_str(), g.relation.as_str()),
                ("folder", "viewer")
            );
            assert!(g.needs_user_pick);
        }
    }

    /// 対象が値式（前の結果・入力・トリガ）なら、有効化者が選ぶ。csv.write のフォルダ省略も
    /// storage.write と同じく選ばせる。
    #[test]
    fn csv_nodes_with_dynamic_targets_need_a_pick() {
        let plan = plan(&json!([
            { "id": "q", "type": "csv.query",
              "params": { "file": { "$from": "trigger", "path": "/file_id" }, "sql": "SELECT 1" } },
            { "id": "p", "type": "csv.patch",
              "params": { "file": { "$from": "input", "path": "/file" }, "base_rev": 1, "ops": [] } },
            { "id": "w", "type": "csv.write", "params": { "name": "out", "content": "a\n1\n" } }
        ]));
        for id in ["q", "p", "w"] {
            let g = by_source(&plan, id);
            assert!(g.needs_user_pick, "{id}: {g:?}");
            assert_eq!(g.object_id, None, "{id}");
        }
        assert_eq!(by_source(&plan, "w").object_kind, "folder");
    }
}
