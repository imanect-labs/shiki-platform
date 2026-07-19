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

/// 未保存の下書きスライド（save_slide の下書き確定型・Task 11.3）。
/// `content` は正規化スライド JSON（`{version, meta, slides}`）を文字列で持つ
/// （note_drafts の `{name, markdown}` と同型のキー＝name 識別・chat 側で slide_draft ブロックへ写る）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlideDraft {
    /// 下書き名（`.slide` なし・会話内の識別キー兼表示名）。
    pub name: String,
    /// 正規化スライド JSON 文字列（サニタイズ済みが正規形・PIT-40）。
    pub content: String,
}

/// 未保存の下書き CSV（save_csv の下書き確定型・Task 11.11）。
/// `csv` は CSV 本文（ヘッダ行＋データ行）を文字列で持つ（note_drafts の `{name, markdown}` と
/// 同型のキー＝name 識別・chat 側で csv_draft ブロックへ写る）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvDraft {
    /// 下書き名（`.csv` なし・会話内の識別キー兼表示名）。
    pub name: String,
    /// CSV 本文（ヘッダ行＋データ行）。
    pub csv: String,
}

/// 開いている Office 文書セッションへの AI ライブ編集（office.live_edit・#328）。
/// ファイルは書き換えず、開いている Collabora セッションの**現在の選択範囲**を `html` で置換する
/// よう指示する（フロントが Action_Paste を実行）。`node_id` は対象ファイルの storage node id、
/// `html` は書込サニタイズ済み（PIT-40 準拠・ammonia）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficeLiveEdit {
    /// 対象 Office ファイルの storage node id。
    pub node_id: String,
    /// 現在の選択範囲を置き換えるサニタイズ済み HTML。
    pub html: String,
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
    /// 保存済みノートへの参照（save_note のみ・Task 11P.5）。
    /// `{id, name}` の JSON。**StorageService へ作成済みのノードのみ**を入れること
    /// （chat 側で note_ref ブロックへ写る）。
    pub note_refs: Vec<serde_json::Value>,
    /// 未保存の下書きノート（save_note の下書き確定型・issue #282）。
    /// `{name, markdown}` の JSON。**まだ StorageService へ作成していない**下書き本文を入れる
    /// （chat 側で note_draft ブロックへ写り、フロントが下書きノート画面で詰めてから確定保存する）。
    pub note_drafts: Vec<serde_json::Value>,
    /// 未保存の下書きスライド（save_slide の下書き確定型・Task 11.3）。
    /// **まだ StorageService へ作成していない**下書きスライドを入れる（chat 側で slide_draft
    /// ブロックへ写り、フロントが下書きスライド画面で詰めてから「ドライブに保存」で確定する）。
    pub slide_drafts: Vec<SlideDraft>,
    /// 未保存の下書き CSV（save_csv の下書き確定型・Task 11.11）。
    /// **まだ StorageService へ作成していない**下書き CSV を入れる（chat 側で csv_draft
    /// ブロックへ写り、フロントが下書き CSV 画面で詰めてから「ドライブに保存」で確定する）。
    pub csv_drafts: Vec<CsvDraft>,
    /// 未保存の下書き Word 文書（save_document の下書き確定型・#332）。
    /// `{name, markdown}` の JSON（note_drafts と同表現）。**まだ .docx 化も保存もしていない**
    /// 下書き本文を入れる（chat 側で document_draft ブロックへ写り、フロントが下書き画面で
    /// 詰めてから「ドライブに保存」で .docx 化・確定保存する）。
    pub document_drafts: Vec<serde_json::Value>,
    /// 開いている Office セッションへの AI ライブ編集（office.live_edit のみ・他ツールは空・#328）。
    /// **ファイルは書き換えない**（現在の選択範囲を置換する指示のみ）。ライブ専用イベントとして
    /// SSE へ流れ、message.content へは projection しない（履歴再生で二重 paste しないため）。
    pub office_live_edits: Vec<OfficeLiveEdit>,
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
            note_refs: Vec::new(),
            note_drafts: Vec::new(),
            slide_drafts: Vec::new(),
            csv_drafts: Vec::new(),
            document_drafts: Vec::new(),
            office_live_edits: Vec::new(),
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
            note_refs: Vec::new(),
            note_drafts: Vec::new(),
            slide_drafts: Vec::new(),
            csv_drafts: Vec::new(),
            document_drafts: Vec::new(),
            office_live_edits: Vec::new(),
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
