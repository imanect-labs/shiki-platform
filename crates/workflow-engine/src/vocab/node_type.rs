//! ノード type の閉集合（将来予約を含む全語彙・500 行ゲート対応で vocab から分離）。
//!
//! Stage A 実装分に加え、将来ノードを variant として**先行予約**する（issue #180）。
//! serde 名（＝IR の "type" 値・TS 型・UI カタログ）を先に確定させて後方互換性を固定し、
//! 未実装分は [`NodeType::available_stage_a`] が false を返して保存時検証 V3 が拒否する。
//! 各ノードの設計仕様（用途・冪等性・timeout）は docs/workflow/ir.md §7 が正。

use super::vocab_enum;

vocab_enum! {
    /// ノード type の閉集合。未実装 variant は保存時に V3 が Stage で弾く（予約語彙）。
    pub enum NodeType {
        // ---- 制御（Stage A 実装済み）----
        ControlBranch => "control.branch",
        ControlSwitch => "control.switch",
        ControlJoin => "control.join",
        ControlMap => "control.map",
        ControlWait => "control.wait",
        // ---- 能力・AI・script（Stage A 実装済み）----
        StorageRead => "storage.read",
        StorageWrite => "storage.write",
        StorageList => "storage.list",
        RagSearch => "rag.search",
        LlmInvoke => "llm.invoke",
        AgentInvoke => "agent.invoke",
        HttpRequest => "http.request",
        ScriptRun => "script.run",
        WorkflowStart => "workflow.start",
        // ---- Stage B（ir.md §7 設計済み・未実装）----
        DataQuery => "data.query",
        DataRecordCreate => "data.record.create",
        DataRecordUpdate => "data.record.update",
        DataTransition => "data.transition",
        NotifySend => "notify.send",
        SkillInvoke => "skill.invoke",
        // ---- 将来予約: 制御・デバッグ（issue #180）----
        /// 条件ループ（DAG にサイクルを持ち込まずエンジン側で展開）。
        ControlLoop => "control.loop",
        /// 条件不成立で run を失敗させるアサーション。
        ControlAssert => "control.assert",
        /// 実行履歴への構造化ログ出力。
        DebugLog => "debug.log",
        // ---- 将来予約: 宣言的データ加工（pure・ノーコード向け）----
        TransformTemplate => "transform.template",
        TransformParse => "transform.parse",
        TransformSerialize => "transform.serialize",
        TransformRegex => "transform.regex",
        TransformMap => "transform.map",
        TransformFilter => "transform.filter",
        TransformReduce => "transform.reduce",
        // ---- 将来予約: オフィス系能力（中粒度）----
        SheetRead => "sheet.read",
        SheetWrite => "sheet.write",
        SheetAppend => "sheet.append",
        DocRead => "doc.read",
        DocEdit => "doc.edit",
        DocComment => "doc.comment",
        // ---- 将来予約: 状態・イベント・サブフロー ----
        MemoryGet => "memory.get",
        MemorySet => "memory.set",
        EventPublish => "event.publish",
        /// 同期サブフロー（結果を待つ）。fire-and-forget は workflow.start。
        WorkflowCall => "workflow.call",
        // ---- 将来予約: LLM/AI モダリティ ----
        LlmEmbed => "llm.embed",
        /// スキーマ指定の構造化出力（出力スキーマ検証まで契約に含む）。
        LlmExtract => "llm.extract",
        AiReview => "ai.review",
        AiEval => "ai.eval",
        AiOcr => "ai.ocr",
        AiImageAnalyze => "ai.image.analyze",
        AiImageGenerate => "ai.image.generate",
        AiTranscribe => "ai.transcribe",
        AiSpeech => "ai.speech",
        // ---- 将来予約: 外部連携・実行・人間参加 ----
        GraphqlQuery => "graphql.query",
        /// サンドボックス内の単発コマンド/Python 実行（secure-exec 基盤を転用）。
        SandboxExec => "sandbox.exec",
        /// 人間の承認を待って分岐する human-in-the-loop。
        HumanApproval => "human.approval",
    }
}

impl NodeType {
    /// 制御ノードか（能力ゲートウェイを経由しない・pure）。
    pub fn is_control(self) -> bool {
        matches!(
            self,
            NodeType::ControlBranch
                | NodeType::ControlSwitch
                | NodeType::ControlJoin
                | NodeType::ControlMap
                | NodeType::ControlWait
                | NodeType::ControlLoop
                | NodeType::ControlAssert
        )
    }

    /// Stage A で有効なノード type（V3 が照合する集合）。
    ///
    /// ここに無い variant は**予約語彙**であり、保存時に「Stage A では未対応」として
    /// 拒否される。実装したら本メソッドへ追加する（variant 追加は不要＝後方互換）。
    pub fn available_stage_a(self) -> bool {
        matches!(
            self,
            NodeType::ControlBranch
                | NodeType::ControlSwitch
                | NodeType::ControlJoin
                | NodeType::ControlMap
                | NodeType::ControlWait
                | NodeType::StorageRead
                | NodeType::StorageWrite
                | NodeType::StorageList
                | NodeType::RagSearch
                | NodeType::LlmInvoke
                | NodeType::AgentInvoke
                | NodeType::HttpRequest
                | NodeType::ScriptRun
                | NodeType::WorkflowStart
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_a_matches_implemented_set() {
        let stage_a: Vec<&str> = NodeType::ALL
            .iter()
            .filter(|n| n.available_stage_a())
            .map(|n| n.as_str())
            .collect();
        assert_eq!(
            stage_a,
            [
                "control.branch",
                "control.switch",
                "control.join",
                "control.map",
                "control.wait",
                "storage.read",
                "storage.write",
                "storage.list",
                "rag.search",
                "llm.invoke",
                "agent.invoke",
                "http.request",
                "script.run",
                "workflow.start",
            ]
        );
    }

    #[test]
    fn reserved_vocab_is_parsed_but_gated() {
        for name in ["data.query", "sheet.read", "ai.review", "human.approval"] {
            let nt = NodeType::parse(name).expect("予約語彙は閉集合に含まれる");
            assert!(!nt.available_stage_a(), "{name} は Stage A では無効のはず");
        }
    }

    #[test]
    fn control_includes_reserved_control_nodes() {
        assert!(NodeType::ControlLoop.is_control());
        assert!(NodeType::ControlAssert.is_control());
        assert!(!NodeType::DebugLog.is_control());
        assert!(!NodeType::HumanApproval.is_control());
    }
}
