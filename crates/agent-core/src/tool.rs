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
    /// AI が作成/編集した文書への参照（#381）。
    /// `{id, name, kind, version}` の JSON。**StorageService へ作成済み/編集済みのノードのみ**
    /// を入れる（chat 側で document_ref ブロックへ写り、成果物への導線カードになる）。
    /// 作成系（save_document / save_sheet）と編集系（office.* / document.edit / csv.patch）が共有する。
    pub document_refs: Vec<serde_json::Value>,
    /// skill ツールの発動記録（skill のみ・他ツールは空・#344 Task 10.11）。
    /// `{skill_id, skill_version, name}` の JSON。**発話ユーザー権限で解決に成功した**発動のみ
    /// を入れる（run イベントへ append され「何をいつ適用したか」の完全な列が残る＝監査・再現性）。
    pub skill_invocations: Vec<serde_json::Value>,
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
            document_refs: Vec::new(),
            skill_invocations: Vec::new(),
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
            document_refs: Vec::new(),
            skill_invocations: Vec::new(),
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

/// 会話に添付されたファイル 1 件（storage node 参照のみ・実体二重持ち無し）。
///
/// chat の `ContentBlock::FileRef` と同型。`code_interpreter` はこれを guest の
/// `/workspace/<name>` へ seed する（#379）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    /// storage node id。
    pub node_id: String,
    /// 表示ファイル名（guest 上のファイル名にもなる）。
    pub name: String,
}

/// 添付ファイルの実体取得（差し替え点・#379）。
///
/// 実装は shiki-server 側で `StorageService`（認可・監査の単一チョークポイント）へ配線する。
/// 読み取りは**発話ユーザーの `AuthContext`** で行い昇格しない（confused-deputy 回避）。
#[async_trait::async_trait]
pub trait AttachmentStore: Send + Sync {
    /// 添付の実体を読む。`max_bytes` を超えるものは [`ToolError::Invalid`] で断る
    /// （サンドボックスへの巨大コピーを実体取得**前**に構造的に防ぐ）。
    async fn read(
        &self,
        ctx: &AuthContext,
        node_id: &str,
        max_bytes: u64,
        trace_id: Option<&str>,
    ) -> Result<Vec<u8>, ToolError>;
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

    /// **冪等・副作用なしの read** なら true（issue #349）。
    ///
    /// true のツールだけが同一ステップ内で**有界並列**に実行される（deep research の
    /// 検索→複数取得のファンアウトが直列にならない）。既定は false の**オプトイン**:
    /// 「確認不要 ＝ 並列にしてよい」ではない（承認不要でも副作用を持つツールはある）ため、
    /// 並列化の可否は各ツールが自分で表明する。
    fn is_read_only(&self) -> bool {
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
