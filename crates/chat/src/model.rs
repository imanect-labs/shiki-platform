//! チャットドメインモデル（Task 3.1）。
//!
//! `content` = 構造化ブロック配列（[`ContentBlock`]）。添付はストレージ node 参照のみ
//! （実体二重持ち無し）。SSE で配信する差分イベントは [`StreamEventKind`]（`generation_event`
//! の payload と一致）で、フロント `web/src/lib/chat-api.ts` の `ContentBlock` / `StreamHandlers`
//! 契約と同型に保つ（型は codegen で OpenAPI→TS へ流し手書きミラーを作らない）。

use chrono::{DateTime, Utc};
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
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// ツール結果。
    ToolResult {
        tool_call_id: String,
        content: String,
    },
    /// 引用（doc_search / 古典 RAG 注入の戻り）。
    Citation(Citation),
    /// 宣言的 UI（Phase 6 で実体化。Phase 3 はプレースホルダ）。
    GenerativeUi { spec: serde_json::Value },
    /// 添付ファイル参照（ストレージ node 参照のみ）。
    FileRef { node_id: String, name: String },
}

/// メッセージ添付（ストレージ node 参照のみ・実体二重持ち無し）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Attachment {
    pub node_id: String,
    pub name: String,
}

/// スレッド（会話）。API DTO 兼ドメイン。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Thread {
    pub id: Uuid,
    pub title: String,
    /// thread 既定のエージェントモード（message 単位で上書き可）。
    pub agent_mode: bool,
    /// 適用する skill のバージョンピン（Task 6.7・作成時に固定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_version: Option<i64>,
    /// ミニアプリ経由のセッション（Task 6.10）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini_app_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini_app_version: Option<i64>,
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

/// SSE で配信する構造化イベント（`generation_event.payload` と一致）。
///
/// フロント `StreamHandlers`（onToken/onThinking/onToolCall/onToolResult/onCitation/onError）
/// と対応する。各イベントは `generation_event(run_id, seq)` に append され、SSE では
/// `id: <seq>` を付けて配信する（Last-Event-ID で replay-then-subscribe）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEventKind {
    /// 本文トークン（差分）。
    Token { text: String },
    /// 思考トークン（差分）。
    Thinking { text: String },
    /// ツール呼び出し開始（エージェントモード可視化）。
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// ツール結果。
    ToolResult {
        tool_call_id: String,
        ok: bool,
        content: String,
    },
    /// 引用。
    Citation(Citation),
    /// ツール成果物のファイル参照（code_interpreter の保存済み成果物・Task 4.11）。
    FileRef { node_id: String, name: String },
    /// 宣言的 UI（Phase 6）。
    GenerativeUi { spec: serde_json::Value },
    /// 計画の改訂（自律エージェント・Task 5.2）。サブタスク列を丸ごと配信する。
    Plan { subtasks: Vec<PlanSubtask> },
    /// 予算上限への接近警告（Task 5.7）。
    BudgetWarning { kind: String, used: u64, limit: u64 },
    /// 承認要求（破壊系/egress/高コスト・Task 5.6）。UI が承認ダイアログを出す。
    ApprovalRequested {
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
        reason: String,
    },
    /// 承認結果（許可/却下・Task 5.6）。
    ApprovalResolved {
        tool_call_id: String,
        approved: bool,
    },
    /// 失敗回復の判断（自己修正リトライ／ループ検出停止・Task 5.5）。
    FailureRecovery { detail: String, action: String },
    /// 状態遷移（running/waiting_approval/done/failed/cancelled）。UI の生成状態表示に使う。
    Status { status: RunStatus },
    /// エラー（生成失敗）。
    Error { message: String },
    /// 完了（確定した assistant message id）。
    Done { message_id: Uuid },
}

/// 計画のサブタスク 1 件（SSE `plan` イベント用・agent-core `Subtask` のミラー）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlanSubtask {
    pub id: String,
    pub title: String,
    /// todo / doing / done / blocked。
    pub status: String,
}

impl StreamEventKind {
    /// `generation_event.type` 列に入れる短い種別名（デバッグ/索引用）。
    pub fn tag(&self) -> &'static str {
        match self {
            StreamEventKind::Token { .. } => "token",
            StreamEventKind::Thinking { .. } => "thinking",
            StreamEventKind::ToolCall { .. } => "tool_call",
            StreamEventKind::ToolResult { .. } => "tool_result",
            StreamEventKind::Citation(_) => "citation",
            StreamEventKind::FileRef { .. } => "file_ref",
            StreamEventKind::GenerativeUi { .. } => "generative_ui",
            StreamEventKind::Plan { .. } => "plan",
            StreamEventKind::BudgetWarning { .. } => "budget_warning",
            StreamEventKind::ApprovalRequested { .. } => "approval_requested",
            StreamEventKind::ApprovalResolved { .. } => "approval_resolved",
            StreamEventKind::FailureRecovery { .. } => "failure_recovery",
            StreamEventKind::Status { .. } => "status",
            StreamEventKind::Error { .. } => "error",
            StreamEventKind::Done { .. } => "done",
        }
    }
}

/// SSE / replay の 1 イベント（seq 付き）。`id: <seq>` で重複排除する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StreamEvent {
    /// run ごと単調増加の seq（＝SSE の `id` / `Last-Event-ID`）。
    pub seq: i64,
    #[serde(flatten)]
    pub event: StreamEventKind,
}

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
