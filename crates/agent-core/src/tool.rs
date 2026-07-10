//! `Tool` トレイト（ツールセット非依存の差し込み点）と関連型。
//!
//! agent-core は LLM↔ツールのループだけを担い、具体ツール（doc_search 等）はこのトレイト裏で
//! 差す。Phase 4/5 でフルツール（shell/CRUD）化するときも同じコアを使う。

use authz::AuthContext;
use serde::{Deserialize, Serialize};

/// ツール実行の引用チャンク（doc_search の戻り。UI の citation ブロックへ）。
/// フロント `chat-api.ts` / `chat::Citation` と同型のフィールドを持つ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    pub node_id: String,
    pub chunk_id: String,
    pub snippet: String,
    #[serde(default)]
    pub page: Option<i32>,
    #[serde(default)]
    pub heading_path: Vec<String>,
    pub score: f32,
}

/// ツール実行のエラー。
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// 呼び出し不正（入力パース失敗・必須欠落）。
    #[error("invalid tool input: {0}")]
    Invalid(String),
    /// 依存サービス（RAG 等）の一時障害。
    #[error("tool unavailable: {0}")]
    Unavailable(String),
    /// 内部エラー。
    #[error("tool internal error: {0}")]
    Internal(String),
}

/// ツールが保存した成果物への参照（ストレージ node 参照のみ・実体二重持ち無し）。
/// chat 側で `ContentBlock::FileRef` / SSE `file_ref` イベントへ写す（Task 4.11）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// 保存先の storage node id。
    pub node_id: String,
    /// 表示ファイル名。
    pub name: String,
}

/// ツール実行結果。`content` はモデルへ返すテキスト、`citations` は UI 引用へ。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// モデルが読む観測テキスト（tool_result の content）。
    pub content: String,
    /// UI へ流す引用（doc_search のみ・他ツールは空）。
    pub citations: Vec<Citation>,
    /// ツールが保存した成果物（code_interpreter のみ・他ツールは空）。
    pub artifacts: Vec<ArtifactRef>,
    /// 検証済み generative UI スペック（emit_ui のみ・他ツールは空・Phase 6 Task 6.4）。
    /// **検証層を通過した JSON のみ**を入れること（chat 側で generative_ui ブロックへ写る）。
    pub ui_specs: Vec<serde_json::Value>,
    /// 保存済みワークフローへの参照（emit_workflow のみ・Task 10.13）。
    /// `{id, name, display_name, version}` の JSON。**保存パイプライン（V1〜V7）を通過し
    /// artifact 化されたもののみ**を入れること（chat 側で workflow_ref ブロックへ写る）。
    pub workflow_refs: Vec<serde_json::Value>,
    /// 実行がエラーだったか（tool_result.is_error）。
    pub is_error: bool,
}

impl ToolOutcome {
    /// 通常の成功結果。
    pub fn ok(content: impl Into<String>) -> Self {
        ToolOutcome {
            content: content.into(),
            citations: Vec::new(),
            artifacts: Vec::new(),
            ui_specs: Vec::new(),
            workflow_refs: Vec::new(),
            is_error: false,
        }
    }

    /// エラー結果（モデルに観測させて回復させる）。
    pub fn error(content: impl Into<String>) -> Self {
        ToolOutcome {
            content: content.into(),
            citations: Vec::new(),
            artifacts: Vec::new(),
            ui_specs: Vec::new(),
            workflow_refs: Vec::new(),
            is_error: true,
        }
    }
}

/// ツール成果物の保存先（差し替え点）。
///
/// 実装は shiki-server 側で `StorageService::write_file_internal` に配線する（発話ユーザーの
/// `AuthContext` で保存＝confused-deputy 回避）。agent-core はストレージ実装に依存せず、
/// テストではフェイクを差す。
#[async_trait::async_trait]
pub trait ArtifactStore: Send + Sync {
    /// バイト列を発話ユーザー権限で保存し、参照を返す。
    async fn save(
        &self,
        ctx: &AuthContext,
        name: &str,
        bytes: Vec<u8>,
        content_type: &str,
        trace_id: Option<&str>,
    ) -> Result<ArtifactRef, ToolError>;
}

/// ツール（LLM に提示し、モデルが自律的に呼ぶ）。
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// ツール名（LLM のツール定義 name）。
    fn name(&self) -> &str;
    /// 説明（モデルが呼び出し判断に使う）。
    fn description(&self) -> &str;
    /// 入力 JSON Schema。
    fn input_schema(&self) -> serde_json::Value;

    /// **破壊的/権限/高コスト系**なら true（明示許可が要る・Task 3.9）。
    /// 既定は false（doc_search 等の安全なツール）。true のツールは確認なしに実行されない。
    fn requires_confirmation(&self) -> bool {
        false
    }

    /// 呼び出しユーザーの権限（`ctx`）で実行する。confused-deputy を避けるため、
    /// ツールは常に発話ユーザーの `AuthContext` で権限判定する（昇格しない）。
    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError>;
}
