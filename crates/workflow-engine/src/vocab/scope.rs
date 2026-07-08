//! 宣言スコープの閉集合（将来予約を含む・500 行ゲート対応で vocab から分離）。

use super::vocab_enum;

vocab_enum! {
    /// 宣言スコープ（declared_scopes）の閉集合。IR が宣言できる権限の天井。
    ///
    /// 将来ノード（issue #180）の要求スコープも先行予約する。Stage A で有効な部分集合は
    /// [`Scope::available_stage_a`] が返し、それ以外は V3 が保存時に拒否する。
    pub enum Scope {
        DataRead => "data.read",
        DataWrite => "data.write",
        StorageRead => "storage.read",
        StorageWrite => "storage.write",
        RagQuery => "rag.query",
        NotifySend => "notify.send",
        HttpEgress => "http.egress",
        WorkflowStart => "workflow.start",
        // ---- 将来予約（issue #180）----
        SheetRead => "sheet.read",
        SheetWrite => "sheet.write",
        DocRead => "doc.read",
        DocWrite => "doc.write",
        MemoryRead => "memory.read",
        MemoryWrite => "memory.write",
        EventPublish => "event.publish",
        /// サンドボックス内コマンド実行（sandbox.exec ノード）。危険度が高く独立スコープ。
        SandboxExec => "sandbox.exec",
    }
}

impl Scope {
    /// 能力 API（ノード type / HostCall api 名）に必要なスコープを返す（scope 天井・engine.md §9.2）。
    ///
    /// 制御ノード（control.*）・transform.*・debug.log は天井対象外（`None`）。
    /// llm.* / ai.* / agent.invoke / skill.invoke も専用スコープを設けず内部推論として
    /// 天井対象外（外部到達は http.egress・データ到達は storage/rag 等のスコープで縛る）。
    /// storage.list は read で足りる。
    pub fn for_api(api: &str) -> Option<Self> {
        Some(match api {
            "storage.read" | "storage.list" => Scope::StorageRead,
            "storage.write" => Scope::StorageWrite,
            "rag.search" => Scope::RagQuery,
            "http.request" | "graphql.query" => Scope::HttpEgress,
            "workflow.start" | "workflow.call" => Scope::WorkflowStart,
            "data.query" | "data.get" => Scope::DataRead,
            "data.record.create" | "data.record.update" | "data.transition" => Scope::DataWrite,
            "notify.send" | "human.approval" => Scope::NotifySend,
            "sheet.read" => Scope::SheetRead,
            "sheet.write" | "sheet.append" => Scope::SheetWrite,
            "doc.read" => Scope::DocRead,
            "doc.edit" | "doc.comment" => Scope::DocWrite,
            "memory.get" => Scope::MemoryRead,
            "memory.set" => Scope::MemoryWrite,
            "event.publish" => Scope::EventPublish,
            "sandbox.exec" => Scope::SandboxExec,
            _ => return None,
        })
    }

    /// Stage A で有効なスコープ（V3 が照合する集合）。data.* / notify.send / 予約分は Stage B 以降。
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
