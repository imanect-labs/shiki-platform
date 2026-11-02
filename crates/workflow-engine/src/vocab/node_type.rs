//! ノード type の閉集合（将来予約を含む全語彙・行数ゲート対応で vocab から分離）。
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
        /// 質問に選択肢・段階・はい/いいえで答え、確からしさを付けて返す（決定モデル・FEAT-1・pure・課金あり）。
        AiDecide => "ai.decide",
        HttpRequest => "http.request",
        ScriptRun => "script.run",
        WorkflowStart => "workflow.start",
        /// 結果を起動した本人の画面に返す（ボタン・会話から起動した run だけ・GAP-09・pure）。
        WorkflowReply => "workflow.reply",
        // ---- CSV 表データ（Task 11P.9・隔離 DuckDB 経由の tabular サービス）----
        /// CSV への読み取り専用 SQL（viewer・pure）。
        CsvQuery => "csv.query",
        /// CSV のパッチ編集→新バージョン（editor・EngineDedup）。
        CsvPatch => "csv.patch",
        /// 新規 CSV の保存（作成権限・EngineDedup）。
        CsvWrite => "csv.write",
        // ---- 構造化データ（Task 15.7・data の DataQuery をそのまま使う）----
        DataQuery => "data.query",
        DataRecordCreate => "data.record.create",
        DataRecordUpdate => "data.record.update",
        /// 選んだ列（重複を許さない列）の値が同じレコードがあれば書き換え、なければ追加する（GAP-16・EngineDedup）。
        DataRecordUpsert => "data.record.upsert",
        DataTransition => "data.transition",
        /// レコードの削除（rev 楽観ロック・EngineDedup）。
        DataRecordDelete => "data.record.delete",
        /// 一括 upsert（data の一括書込パス・更新行は 1 件の更新と同じ入口・EngineDedup）。
        DataBulkUpsert => "data.bulk_upsert",
        // ---- Stage B（ir.md §7 設計済み・未実装）----
        NotifySend => "notify.send",
        /// インストール済み skill の実行（`skill:<name>@<version>`・Stage A 昇格・#344）。
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
        /// 担当者の判断（承認・却下・差し戻し）を待って分岐する（テーブルの 1 行に束縛・GAP-08）。
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
                | NodeType::AiDecide
                | NodeType::HttpRequest
                | NodeType::ScriptRun
                | NodeType::SkillInvoke
                | NodeType::WorkflowStart
                | NodeType::WorkflowReply
                | NodeType::CsvQuery
                | NodeType::CsvPatch
                | NodeType::CsvWrite
                | NodeType::DataQuery
                | NodeType::DataRecordCreate
                | NodeType::DataRecordUpdate
                | NodeType::DataRecordUpsert
                | NodeType::DataRecordDelete
                | NodeType::DataTransition
                | NodeType::DataBulkUpsert
                | NodeType::HumanApproval
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
                "ai.decide",
                "http.request",
                "script.run",
                "workflow.start",
                "workflow.reply",
                "csv.query",
                "csv.patch",
                "csv.write",
                "data.query",
                "data.record.create",
                "data.record.update",
                "data.record.upsert",
                "data.transition",
                "data.record.delete",
                "data.bulk_upsert",
                "skill.invoke",
                "human.approval",
            ]
        );
    }

    #[test]
    fn reserved_vocab_is_parsed_but_gated() {
        for name in ["notify.send", "sheet.read", "ai.review", "sandbox.exec"] {
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
