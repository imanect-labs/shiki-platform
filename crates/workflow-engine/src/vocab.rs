//! ワークフロー語彙の単一ソース（Single Source of Truth・Task 10.1a）。
//!
//! ノード type・スコープ・イベント source・run イベント種・Shiki.* api を Rust enum で
//! **閉じた集合**として定義し、`#[derive(TS)]` で TypeScript 型を生成する。保存時検証 V3 は
//! この閉集合（の Stage A で有効な部分集合）へ IR を照合し、ハルシネーション境界を貫く。
//!
//! authz の `vocab.rs`（Relation/ObjectType）と同型の設計。enum は v1 の全語彙を定義し、
//! Stage A で有効化する部分集合は `*_stage_a()` が返す（Stage B 追加が variant 追加だけになる）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 宣言スコープ（declared_scopes）の閉集合。IR が宣言できる権限の天井。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Scope {
    #[serde(rename = "data.read")]
    DataRead,
    #[serde(rename = "data.write")]
    DataWrite,
    #[serde(rename = "storage.read")]
    StorageRead,
    #[serde(rename = "storage.write")]
    StorageWrite,
    #[serde(rename = "rag.query")]
    RagQuery,
    #[serde(rename = "notify.send")]
    NotifySend,
    #[serde(rename = "http.egress")]
    HttpEgress,
    #[serde(rename = "workflow.start")]
    WorkflowStart,
}

impl Scope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Scope::DataRead => "data.read",
            Scope::DataWrite => "data.write",
            Scope::StorageRead => "storage.read",
            Scope::StorageWrite => "storage.write",
            Scope::RagQuery => "rag.query",
            Scope::NotifySend => "notify.send",
            Scope::HttpEgress => "http.egress",
            Scope::WorkflowStart => "workflow.start",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "data.read" => Scope::DataRead,
            "data.write" => Scope::DataWrite,
            "storage.read" => Scope::StorageRead,
            "storage.write" => Scope::StorageWrite,
            "rag.query" => Scope::RagQuery,
            "notify.send" => Scope::NotifySend,
            "http.egress" => Scope::HttpEgress,
            "workflow.start" => Scope::WorkflowStart,
            _ => return None,
        })
    }

    /// Stage A で有効なスコープ（V3 が照合する集合）。data.* / notify.send は Stage B。
    pub fn available_stage_a(self) -> bool {
        matches!(
            self,
            Scope::StorageRead
                | Scope::StorageWrite
                | Scope::RagQuery
                | Scope::HttpEgress
                | Scope::WorkflowStart
        )
    }
}

/// ノード type の閉集合（Stage A 実装分のみ variant 化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NodeType {
    #[serde(rename = "control.branch")]
    ControlBranch,
    #[serde(rename = "control.switch")]
    ControlSwitch,
    #[serde(rename = "control.join")]
    ControlJoin,
    #[serde(rename = "control.map")]
    ControlMap,
    #[serde(rename = "control.wait")]
    ControlWait,
    #[serde(rename = "storage.read")]
    StorageRead,
    #[serde(rename = "storage.write")]
    StorageWrite,
    #[serde(rename = "storage.list")]
    StorageList,
    #[serde(rename = "rag.search")]
    RagSearch,
    #[serde(rename = "llm.invoke")]
    LlmInvoke,
    #[serde(rename = "agent.invoke")]
    AgentInvoke,
    #[serde(rename = "http.request")]
    HttpRequest,
    #[serde(rename = "script.run")]
    ScriptRun,
    #[serde(rename = "workflow.start")]
    WorkflowStart,
}

impl NodeType {
    pub const fn as_str(self) -> &'static str {
        match self {
            NodeType::ControlBranch => "control.branch",
            NodeType::ControlSwitch => "control.switch",
            NodeType::ControlJoin => "control.join",
            NodeType::ControlMap => "control.map",
            NodeType::ControlWait => "control.wait",
            NodeType::StorageRead => "storage.read",
            NodeType::StorageWrite => "storage.write",
            NodeType::StorageList => "storage.list",
            NodeType::RagSearch => "rag.search",
            NodeType::LlmInvoke => "llm.invoke",
            NodeType::AgentInvoke => "agent.invoke",
            NodeType::HttpRequest => "http.request",
            NodeType::ScriptRun => "script.run",
            NodeType::WorkflowStart => "workflow.start",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "control.branch" => NodeType::ControlBranch,
            "control.switch" => NodeType::ControlSwitch,
            "control.join" => NodeType::ControlJoin,
            "control.map" => NodeType::ControlMap,
            "control.wait" => NodeType::ControlWait,
            "storage.read" => NodeType::StorageRead,
            "storage.write" => NodeType::StorageWrite,
            "storage.list" => NodeType::StorageList,
            "rag.search" => NodeType::RagSearch,
            "llm.invoke" => NodeType::LlmInvoke,
            "agent.invoke" => NodeType::AgentInvoke,
            "http.request" => NodeType::HttpRequest,
            "script.run" => NodeType::ScriptRun,
            "workflow.start" => NodeType::WorkflowStart,
            _ => return None,
        })
    }

    /// 制御ノードか（能力ゲートウェイを経由しない・pure）。
    pub fn is_control(self) -> bool {
        matches!(
            self,
            NodeType::ControlBranch
                | NodeType::ControlSwitch
                | NodeType::ControlJoin
                | NodeType::ControlMap
                | NodeType::ControlWait
        )
    }
}

/// イベントトリガの source 閉集合（Stage A は storage.write のみ有効）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EventSource {
    #[serde(rename = "storage.write")]
    StorageWrite,
    #[serde(rename = "data.record.created")]
    DataRecordCreated,
    #[serde(rename = "data.record.updated")]
    DataRecordUpdated,
    #[serde(rename = "data.transition")]
    DataTransition,
}

impl EventSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            EventSource::StorageWrite => "storage.write",
            EventSource::DataRecordCreated => "data.record.created",
            EventSource::DataRecordUpdated => "data.record.updated",
            EventSource::DataTransition => "data.transition",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "storage.write" => EventSource::StorageWrite,
            "data.record.created" => EventSource::DataRecordCreated,
            "data.record.updated" => EventSource::DataRecordUpdated,
            "data.transition" => EventSource::DataTransition,
            _ => return None,
        })
    }

    /// Stage A で有効な source（storage.write のみ・data 系は 9.10 後）。
    pub fn available_stage_a(self) -> bool {
        matches!(self, EventSource::StorageWrite)
    }
}

/// run_event の種（engine.md §3.3 の 12 種）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunEventKind {
    RunStarted,
    StepReady,
    StepStarted,
    StepSucceeded,
    StepFailed,
    StepRetrying,
    StepSkipped,
    StepWaiting,
    StepWoken,
    RunSucceeded,
    RunFailed,
    RunCancelled,
}

impl RunEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            RunEventKind::RunStarted => "run.started",
            RunEventKind::StepReady => "step.ready",
            RunEventKind::StepStarted => "step.started",
            RunEventKind::StepSucceeded => "step.succeeded",
            RunEventKind::StepFailed => "step.failed",
            RunEventKind::StepRetrying => "step.retrying",
            RunEventKind::StepSkipped => "step.skipped",
            RunEventKind::StepWaiting => "step.waiting",
            RunEventKind::StepWoken => "step.woken",
            RunEventKind::RunSucceeded => "run.succeeded",
            RunEventKind::RunFailed => "run.failed",
            RunEventKind::RunCancelled => "run.cancelled",
        }
    }
}

/// ノード type が要求するスコープ（scope_ceiling 交差・能力ゲートウェイが検証）。
///
/// 制御ノードは能力を呼ばないため `None`。
pub fn required_scope(node: NodeType) -> Option<Scope> {
    Some(match node {
        NodeType::StorageRead | NodeType::StorageList => Scope::StorageRead,
        NodeType::StorageWrite => Scope::StorageWrite,
        NodeType::RagSearch => Scope::RagQuery,
        NodeType::HttpRequest => Scope::HttpEgress,
        NodeType::WorkflowStart => Scope::WorkflowStart,
        // llm.invoke / agent.invoke / script.run は宣言スコープを固定的に要求しない
        // （llm=予算・agent=サンドボックス縮小・script=内部の Shiki.* が個別に要求）。
        NodeType::LlmInvoke | NodeType::AgentInvoke | NodeType::ScriptRun => return None,
        _ if node.is_control() => return None,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_roundtrip() {
        for s in [
            Scope::DataRead,
            Scope::StorageWrite,
            Scope::RagQuery,
            Scope::HttpEgress,
            Scope::WorkflowStart,
        ] {
            assert_eq!(Scope::parse(s.as_str()), Some(s));
        }
        assert_eq!(Scope::parse("bogus.scope"), None);
    }

    #[test]
    fn scope_stage_a_subset() {
        assert!(Scope::StorageRead.available_stage_a());
        assert!(!Scope::DataRead.available_stage_a());
        assert!(!Scope::NotifySend.available_stage_a());
    }

    #[test]
    fn node_type_roundtrip_all() {
        for n in [
            NodeType::ControlBranch,
            NodeType::ControlMap,
            NodeType::StorageWrite,
            NodeType::RagSearch,
            NodeType::LlmInvoke,
            NodeType::HttpRequest,
            NodeType::ScriptRun,
            NodeType::WorkflowStart,
        ] {
            assert_eq!(NodeType::parse(n.as_str()), Some(n));
        }
        assert_eq!(NodeType::parse("data.query"), None);
    }

    #[test]
    fn event_source_stage_a() {
        assert!(EventSource::StorageWrite.available_stage_a());
        assert!(!EventSource::DataTransition.available_stage_a());
    }

    #[test]
    fn required_scope_mapping() {
        assert_eq!(
            required_scope(NodeType::StorageWrite),
            Some(Scope::StorageWrite)
        );
        assert_eq!(required_scope(NodeType::RagSearch), Some(Scope::RagQuery));
        assert_eq!(required_scope(NodeType::ControlBranch), None);
        assert_eq!(required_scope(NodeType::ScriptRun), None);
    }

    #[test]
    fn serde_snake_dotted() {
        assert_eq!(
            serde_json::to_string(&NodeType::StorageWrite).unwrap(),
            "\"storage.write\""
        );
        let n: NodeType = serde_json::from_str("\"control.map\"").unwrap();
        assert_eq!(n, NodeType::ControlMap);
        assert!(serde_json::from_str::<NodeType>("\"unknown\"").is_err());
    }
}
