//! ワークフロー語彙の単一ソース（Single Source of Truth・Task 10.1a）。
//!
//! ノード type・スコープ・イベント source・run イベント種を Rust enum で**閉じた集合**として
//! 定義し、`#[derive(TS)]` で TypeScript 型を生成する。保存時検証 V3 はこの閉集合
//! （の Stage で有効な部分集合）へ IR を照合し、ハルシネーション境界を貫く。
//!
//! authz の `vocab.rs`（Relation/ObjectType）と同型の設計。enum は将来ノードを含む
//! **全語彙を先行定義**し（issue #180・serde 名＝IR/TS 表現を先に確定して後方互換性を固定）、
//! 現ステージで有効化する部分集合は `available_stage_a()` が返す。
//! 未実装 variant の IR は V3 が「Stage A では未対応」として保存を拒否する。

mod node_type;
mod scope;

pub use node_type::NodeType;
pub use scope::Scope;

/// variant と serde/IR 名の対応を単一定義から生成する（as_str/parse の乖離を構造的に防ぐ）。
macro_rules! vocab_enum {
    (
        $(#[$attr:meta])*
        $vis:vis enum $enum_name:ident {
            $( $(#[$vattr:meta])* $variant:ident => $name:literal, )+
        }
    ) => {
        $(#[$attr])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, ts_rs::TS,
        )]
        #[ts(export)]
        $vis enum $enum_name {
            $( $(#[$vattr])* #[serde(rename = $name)] $variant, )+
        }

        impl $enum_name {
            /// serde/IR/DB/TS で共通の文字列表現（ドット表記）。
            $vis const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $name, )+ }
            }

            /// 文字列から閉集合へ（未知は None）。
            $vis fn parse(s: &str) -> Option<Self> {
                match s { $( $name => Some(Self::$variant), )+ _ => None }
            }

            /// 全 variant（カタログ列挙・roundtrip テスト用）。
            $vis const ALL: &'static [$enum_name] = &[ $( Self::$variant, )+ ];
        }
    };
}
pub(crate) use vocab_enum;

vocab_enum! {
    /// イベントトリガの source 閉集合（Stage A は storage.write のみ有効）。
    pub enum EventSource {
        StorageWrite => "storage.write",
        DataRecordCreated => "data.record.created",
        DataRecordUpdated => "data.record.updated",
        DataTransition => "data.transition",
        // ---- 将来予約（issue #180）----
        /// event.publish ノードが発行するカスタムイベントの購読。
        EventCustom => "event.custom",
        /// 外部システムからの webhook 受信（署名検証・レート制限は受信側で設計）。
        WebhookReceived => "webhook.received",
    }
}

impl EventSource {
    /// Stage A で有効な source（storage.write のみ・data 系は 9.10 後）。
    pub fn available_stage_a(self) -> bool {
        matches!(self, EventSource::StorageWrite)
    }
}

vocab_enum! {
    /// run_event の種（engine.md §3.3 の 12 種）。DB の run_event.kind に書く値と一致。
    pub enum RunEventKind {
        RunStarted => "run.started",
        StepReady => "step.ready",
        StepStarted => "step.started",
        StepSucceeded => "step.succeeded",
        StepFailed => "step.failed",
        StepRetrying => "step.retrying",
        StepSkipped => "step.skipped",
        StepWaiting => "step.waiting",
        StepWoken => "step.woken",
        RunSucceeded => "run.succeeded",
        RunFailed => "run.failed",
        RunCancelled => "run.cancelled",
    }
}

/// ノード type が要求するスコープ（scope_ceiling 交差・能力ゲートウェイが検証）。
///
/// 能力ノードの type 名は能力 API（HostCall api）名と同一文字列に揃えてあるため、
/// 対応表は [`Scope::for_api`] に単一化し、ここは委譲のみ（対応表の重複を持たない）。
/// 制御ノード・transform.*・debug.log・llm/ai/agent/script/skill 系が `None` になる
/// 理由は [`Scope::for_api`] のドキュメントを参照。
pub fn required_scope(node: NodeType) -> Option<Scope> {
    Scope::for_api(node.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_vocab() {
        for n in NodeType::ALL {
            assert_eq!(NodeType::parse(n.as_str()), Some(*n));
        }
        for s in Scope::ALL {
            assert_eq!(Scope::parse(s.as_str()), Some(*s));
        }
        for e in EventSource::ALL {
            assert_eq!(EventSource::parse(e.as_str()), Some(*e));
        }
        assert_eq!(NodeType::parse("bogus.node"), None);
        assert_eq!(Scope::parse("bogus.scope"), None);
    }

    #[test]
    fn scope_stage_a_subset() {
        assert!(Scope::StorageRead.available_stage_a());
        assert!(!Scope::DataRead.available_stage_a());
        assert!(!Scope::NotifySend.available_stage_a());
        assert!(!Scope::SheetRead.available_stage_a());
        assert!(!Scope::SandboxExec.available_stage_a());
    }

    #[test]
    fn event_source_stage_a() {
        assert!(EventSource::StorageWrite.available_stage_a());
        assert!(!EventSource::DataTransition.available_stage_a());
        assert!(!EventSource::EventCustom.available_stage_a());
        assert!(!EventSource::WebhookReceived.available_stage_a());
    }

    #[test]
    fn required_scope_mapping() {
        assert_eq!(
            required_scope(NodeType::StorageWrite),
            Some(Scope::StorageWrite)
        );
        assert_eq!(required_scope(NodeType::RagSearch), Some(Scope::RagQuery));
        assert_eq!(required_scope(NodeType::DataQuery), Some(Scope::DataRead));
        assert_eq!(
            required_scope(NodeType::HumanApproval),
            Some(Scope::NotifySend)
        );
        assert_eq!(
            required_scope(NodeType::WorkflowCall),
            Some(Scope::WorkflowStart)
        );
        assert_eq!(
            required_scope(NodeType::SandboxExec),
            Some(Scope::SandboxExec)
        );
        assert_eq!(required_scope(NodeType::ControlBranch), None);
        assert_eq!(required_scope(NodeType::ScriptRun), None);
        assert_eq!(required_scope(NodeType::TransformTemplate), None);
        assert_eq!(required_scope(NodeType::AiOcr), None);
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
        assert_eq!(
            serde_json::to_string(&RunEventKind::RunStarted).unwrap(),
            "\"run.started\""
        );
    }
}
