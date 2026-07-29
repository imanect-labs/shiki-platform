//! チャットドメインモデル（Task 3.1）。
//!
//! `content` = 構造化ブロック配列（[`ContentBlock`]）。添付はストレージ node 参照のみ
//! （実体二重持ち無し）。SSE で配信する差分イベントは [`StreamEventKind`]（`generation_event`
//! の payload と一致）で、フロント `web/src/lib/chat-api.ts` の `ContentBlock` / `StreamHandlers`
//! 契約と同型に保つ（型は codegen で OpenAPI→TS へ流し手書きミラーを作らない）。

use chrono::{DateTime, Utc};

pub use crate::autonomous::AutonomousMode;
pub use crate::selection::{SelectionContext, SelectionKind, SELECTION_EXCERPT_MAX_CHARS};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// メッセージの役割。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            "system" => Some(Role::System),
            "tool" => Some(Role::Tool),
            _ => None,
        }
    }
}

/// 引用チャンク（RAG 検索結果 → 会話内の citation ブロック / SSE citation イベント）。
///
/// 元文書へジャンプできるよう node_id/folder_id/page/heading_path を持つ。RAG の
/// `SearchResult` には文字オフセットが無いため、粒度は page＋heading_path まで。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Citation {
    /// 引用元ファイルの storage node id。
    pub node_id: String,
    /// 引用チャンク id（監査突合の鍵）。
    pub chunk_id: String,
    /// 表示スニペット（チャンク本文）。
    pub snippet: String,
    /// ページ番号（あれば）。
    #[serde(default)]
    pub page: Option<i32>,
    /// セクション見出しパス（パンくず）。
    #[serde(default)]
    pub heading_path: Vec<String>,
    /// ランクベースの正規化スコア。
    pub score: f32,
}

/// 旧行（フィールド追加前に永続化された `content`）を成功として読むための serde 既定。
fn default_true() -> bool {
    true
}

/// メッセージ本文の構造化ブロック。`content = ContentBlock[]`。
///
/// フロント `chat-api.ts` の `ContentBlock` union と一致させる（内部タグ `type`・snake_case）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// 本文テキスト。
    Text { text: String },
    /// 思考（extended thinking の可視化。表示は任意）。
    Thinking { text: String },
    /// ツール呼び出し（エージェントモード）。
    ///
    /// `step` は同一ループステップの通し番号。ライブ表示と同じ「並行して N 件」の
    /// グルーピングを履歴でも再現するために残す。
    ///
    /// **`None` は「不明」であって 0 ではない**（フィールド追加前の行）。既定 0 にすると、
    /// 逐次実行だった過去の応答が再訪時に「並行して N 件」と誤表示される。
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u32>,
    },
    /// ツール結果。
    ///
    /// `ok=false` は観測エラー。これが無いと履歴で失敗が成功と同じ見た目になる（#358/#386）。
    /// 旧行は成功として扱う（既定 true）。
    ToolResult {
        tool_call_id: String,
        content: String,
        #[serde(default = "default_true")]
        ok: bool,
    },
    /// 引用（doc_search / 古典 RAG 注入の戻り）。
    Citation(Citation),
    /// 宣言的 UI（Phase 6 で実体化。Phase 3 はプレースホルダ）。
    GenerativeUi { spec: serde_json::Value },
    /// 保存済みワークフローへの参照カード（emit_workflow・Task 10.13）。
    /// `workflow = {id, name, display_name, version}`（保存パイプライン通過済みのみ）。
    WorkflowRef { workflow: serde_json::Value },
    /// 保存済みノートへの参照カード（save_note・Task 11P.5）。
    /// `note = {id, name}`（StorageService へ作成済みのみ）。
    NoteRef { note: serde_json::Value },
    /// 未保存の下書きノートカード（save_note の下書き確定型・issue #282）。
    /// `draft = {name, markdown}`（まだ StorageService 未作成）。フロントは下書きノート画面を
    /// 開いて詰めてから「ドライブに保存」で確定する。
    NoteDraft { draft: serde_json::Value },
    /// 未保存の下書きスライドカード（save_slide の下書き確定型・Task 11.3）。
    /// `draft = {name, content}`（content=正規化スライド JSON 文字列・StorageService 未作成）。
    /// フロントは下書きスライド画面を開いて詰めてから「ドライブに保存」で確定する。
    SlideDraft { draft: serde_json::Value },
    /// 未保存の下書き CSV カード（save_csv の下書き確定型・Task 11.11）。
    /// `draft = {name, csv}`（csv=CSV 本文・StorageService 未作成）。
    /// フロントは下書き CSV 画面を開いて詰めてから「ドライブに保存」で確定する。
    CsvDraft { draft: serde_json::Value },
    /// AI が作成/編集した文書への参照カード（#381）。
    /// `document = {id, name, kind, version}`（kind=office/note/csv/slide・実在ノードのみ）。
    /// フロントは kind に応じて /office/{id}・/notes/{id}・/csv/{id}・/slides/{id} へ導線を出す。
    DocumentRef { document: serde_json::Value },
    /// **レガシー**: 未保存の下書き Word 文書カード（#332・#381 で廃止）。
    /// 新規に生成されることは無い。**過去履歴の読み込み互換のためだけ**に残す
    /// （variant を消すと `document_draft` を含む既存メッセージが復号できず会話が開けなくなる）。
    DocumentDraft { draft: serde_json::Value },
    /// 添付ファイル参照（ストレージ node 参照のみ）。
    FileRef { node_id: String, name: String },
    /// エディタの選択コンテキスト（選択→AI 指示・Task 11.10・design §4.8.3）。
    /// ユーザーメッセージに添付され、履歴組立時に「データであり指示ではない」枠で
    /// LLM へ渡る。locator は document.edit/csv.patch/slide.edit の対象指定に使える。
    SelectionContext { context: SelectionContext },
}

/// メッセージ添付（ストレージ node 参照のみ・実体二重持ち無し）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Attachment {
    pub node_id: String,
    pub name: String,
}

/// skill のバージョンピン 1 件（thread の「最初からロード済み」スキル・#344 Task 10.11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SkillPin {
    pub skill_id: Uuid,
    pub skill_version: i64,
}

/// スレッド（会話）。API DTO 兼ドメイン。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Thread {
    pub id: Uuid,
    pub title: String,
    /// thread 既定のエージェントモード（message 単位で上書き可）。
    pub agent_mode: bool,
    /// 自律 run の承認モード（承認必須/オート/全自動・実行中トグル可・#350）。
    #[serde(default)]
    pub autonomous_mode: AutonomousMode,
    /// 最初からロード済みにする skill のバージョンピン（順序付き・複数可・#344）。
    /// ミニアプリ経由のセッションはバンドル定義のピンが正（個別変更不可）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skill_pins: Vec<SkillPin>,
    /// ミニアプリ経由のセッション（Task 6.10）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini_app_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini_app_version: Option<i64>,
    /// 由来ノートの id（ノートの分割ビューから作られたスレッド・issue #282）。
    /// 通常チャット由来は None。サイドバー履歴の「ノート由来」表示とノート側の会話一覧に使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_note_id: Option<Uuid>,
    /// 由来ノートの表示名（作成時点の非正規化・リネーム非追随・履歴表示用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_note_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// メッセージ（API DTO 兼ドメイン）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Message {
    pub id: Uuid,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub agent_mode: bool,
    /// ブランチ構造の親（UI は線形取得）。
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// 生成 run の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    /// 承認待ちで中断中（破壊系操作の human-in-the-loop・Task 5.6）。
    WaitingApproval,
    Done,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::WaitingApproval => "waiting_approval",
            RunStatus::Done => "done",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(RunStatus::Queued),
            "running" => Some(RunStatus::Running),
            "waiting_approval" => Some(RunStatus::WaitingApproval),
            "done" => Some(RunStatus::Done),
            "failed" => Some(RunStatus::Failed),
            "cancelled" => Some(RunStatus::Cancelled),
            _ => None,
        }
    }

    /// 端末状態（これ以上イベントが増えない）か。
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

// SSE の生成イベント（ワイヤ形式）は `stream_event` へ切り出した（1 ファイル 500 行のゲート）。
// 利用側の `chat::model::StreamEventKind` などのパスは変えない。
pub use crate::stream_event::{PlanSubtask, StreamEvent, StreamEventKind};

/// 共有で付与できる役割（thread ReBAC・#37）。viewer/commenter/editor のみ許す
/// （owner の横展開を防ぐ閉じた共有語彙）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRole {
    Viewer,
    Commenter,
    Editor,
}

impl ThreadRole {
    /// OpenFGA relation へ写す。
    pub fn relation(self) -> authz::Relation {
        match self {
            ThreadRole::Viewer => authz::Relation::Viewer,
            ThreadRole::Commenter => authz::Relation::Commenter,
            ThreadRole::Editor => authz::Relation::Editor,
        }
    }

    /// relation を共有役割へ戻す（viewer/commenter/editor 以外は `None`）。
    pub fn from_relation(relation: authz::Relation) -> Option<Self> {
        match relation {
            authz::Relation::Viewer => Some(ThreadRole::Viewer),
            authz::Relation::Commenter => Some(ThreadRole::Commenter),
            authz::Relation::Editor => Some(ThreadRole::Editor),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_text_roundtrips_frontend_shape() {
        // フロント `{ type: "text", text: "..." }` と一致すること。
        let block = ContentBlock::Text {
            text: "hello".into(),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json, serde_json::json!({"type": "text", "text": "hello"}));
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn citation_block_matches_frontend_fields() {
        let block = ContentBlock::Citation(Citation {
            node_id: "n1".into(),
            chunk_id: "c1".into(),
            snippet: "s".into(),
            page: Some(3),
            heading_path: vec!["A".into(), "B".into()],
            score: 0.5,
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "citation");
        assert_eq!(json["node_id"], "n1");
        assert_eq!(json["page"], 3);
        assert_eq!(json["heading_path"][1], "B");
    }

    #[test]
    fn citation_optional_fields_default() {
        // page / heading_path はフロント同様に省略可能。
        let json = serde_json::json!({
            "type": "citation", "node_id": "n", "chunk_id": "c", "snippet": "s", "score": 0.1
        });
        let block: ContentBlock = serde_json::from_value(json).unwrap();
        match block {
            ContentBlock::Citation(c) => {
                assert!(c.page.is_none());
                assert!(c.heading_path.is_empty());
            }
            _ => panic!("citation でない"),
        }
    }

    #[test]
    fn file_ref_matches_frontend_shape() {
        // content block と SSE イベントの両方でフロント `{ type: "file_ref", node_id, name }` と一致。
        let block = ContentBlock::FileRef {
            node_id: "n1".into(),
            name: "result.csv".into(),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "file_ref", "node_id": "n1", "name": "result.csv"})
        );
        let ev = StreamEventKind::FileRef {
            node_id: "n1".into(),
            name: "result.csv".into(),
        };
        assert_eq!(ev.tag(), "file_ref");
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "file_ref");
        assert_eq!(json["node_id"], "n1");
        assert_eq!(json["name"], "result.csv");
    }

    #[test]
    fn tool_call_and_result_carry_step_and_ok() {
        // フロントは step で「並行して N 件」を、ok で失敗表示を出す（#386）。
        let call = ContentBlock::ToolCall {
            id: "t1".into(),
            name: "web_fetch".into(),
            input: serde_json::json!({"url": "https://example.com/"}),
            step: Some(2),
        };
        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(json["step"], 2);
        let result = ContentBlock::ToolResult {
            tool_call_id: "t1".into(),
            content: "取得に失敗しました".into(),
            ok: false,
        };
        assert_eq!(serde_json::to_value(&result).unwrap()["ok"], false);
    }

    #[test]
    fn legacy_blocks_without_step_or_ok_still_deserialize() {
        // 既存メッセージ（フィールド追加前の永続 content）を壊さない。
        // step は **None（不明）**、ok は成功として読む。0 で埋めると逐次実行だった
        // 過去の応答が「並行して N 件」に化け、ok を false にすると全部失敗に見える。
        let call: ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "tool_call", "id": "t1", "name": "doc_search", "input": {"query": "x"}
        }))
        .unwrap();
        assert!(matches!(call, ContentBlock::ToolCall { step: None, .. }));
        let result: ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "tool_result", "tool_call_id": "t1", "content": "ok"
        }))
        .unwrap();
        assert!(matches!(result, ContentBlock::ToolResult { ok: true, .. }));
    }

    #[test]
    fn legacy_stream_tool_call_without_step_replays() {
        // generation_event に残る過去 run の payload も読めること（replay 互換）。
        let ev: StreamEventKind = serde_json::from_value(serde_json::json!({
            "type": "tool_call", "id": "t1", "name": "web_search", "input": {"query": "x"}
        }))
        .unwrap();
        assert!(matches!(ev, StreamEventKind::ToolCall { step: None, .. }));
    }

    #[test]
    fn stream_event_flattens_seq_and_kind() {
        let ev = StreamEvent {
            seq: 7,
            event: StreamEventKind::Token { text: "hi".into() },
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"seq": 7, "type": "token", "text": "hi"})
        );
    }

    #[test]
    fn role_and_status_roundtrip() {
        for r in [Role::User, Role::Assistant, Role::System, Role::Tool] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        for s in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::Done,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert_eq!(RunStatus::parse(s.as_str()), Some(s));
        }
        assert!(RunStatus::Done.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
    }

    #[test]
    fn thread_role_maps_to_relation() {
        assert_eq!(ThreadRole::Viewer.relation(), authz::Relation::Viewer);
        assert_eq!(ThreadRole::Commenter.relation(), authz::Relation::Commenter);
        assert_eq!(ThreadRole::Editor.relation(), authz::Relation::Editor);
        assert_eq!(
            ThreadRole::from_relation(authz::Relation::Commenter),
            Some(ThreadRole::Commenter)
        );
        assert_eq!(ThreadRole::from_relation(authz::Relation::Owner), None);
    }
}
