//! 同意計画（consent_plan）の静的分析（registration.rs の 500 行ゲート対応で分離）。

use super::{RegistrationService, SuggestedGrant};
use crate::ir::{Trigger, WorkflowIr};

impl RegistrationService {
    /// 同意画面の提案 grants を IR から静的に列挙する（純関数・最終選択は有効化者）。
    #[must_use]
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

        // イベントトリガの folder 束縛: トリガ元コンテキストの読取提案（典型パターン）。
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

/// params のフィールドが文字列リテラルならその値（`$from`/`$template` は None）。
fn literal_str(params: &serde_json::Value, key: &str) -> Option<String> {
    match params.get(key) {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}
