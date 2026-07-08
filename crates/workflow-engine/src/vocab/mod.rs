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
/// 制御ノード・transform.*・debug.log は能力を呼ばないため `None`。
/// llm.* / ai.* / agent.invoke / script.run / skill.invoke も固定スコープを要求しない
/// （llm=予算・agent=サンドボックス縮小・script 内 Shiki.*・skill 宣言スコープが個別に要求）。
pub fn required_scope(node: NodeType) -> Option<Scope> {
    use NodeType as N;
    Some(match node {
        N::StorageRead | N::StorageList => Scope::StorageRead,
        N::StorageWrite => Scope::StorageWrite,
        N::RagSearch => Scope::RagQuery,
        N::HttpRequest | N::GraphqlQuery => Scope::HttpEgress,
        N::WorkflowStart | N::WorkflowCall => Scope::WorkflowStart,
        N::DataQuery => Scope::DataRead,
        N::DataRecordCreate | N::DataRecordUpdate | N::DataTransition => Scope::DataWrite,
        // human.approval は承認依頼の通知を伴うため notify.send を要求する。
        N::NotifySend | N::HumanApproval => Scope::NotifySend,
        N::SheetRead => Scope::SheetRead,
        N::SheetWrite | N::SheetAppend => Scope::SheetWrite,
        N::DocRead => Scope::DocRead,
        N::DocEdit | N::DocComment => Scope::DocWrite,
        N::MemoryGet => Scope::MemoryRead,
        N::MemorySet => Scope::MemoryWrite,
        N::EventPublish => Scope::EventPublish,
        N::SandboxExec => Scope::SandboxExec,
        _ => return None,
    })
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
    fn required_scope_consistent_with_for_api() {
        // ノード type と同名の能力 API は同じスコープへ解決される（単一語彙の整合）。
        for n in NodeType::ALL {
            if let Some(scope) = required_scope(*n) {
                // llm/agent 系を除き、同名 API のスコープはノードの要求スコープと一致する。
                if let Some(api_scope) = Scope::for_api(n.as_str()) {
                    assert_eq!(
                        api_scope,
                        scope,
                        "ノード {} の API スコープ乖離",
                        n.as_str()
                    );
                }
            }
        }
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
