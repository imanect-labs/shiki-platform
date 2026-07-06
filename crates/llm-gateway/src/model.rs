//! llm-gateway の**中立 content-block 正規形**（PIT-9 の確定形）。
//!
//! 内部型はプロバイダ非依存の block 列（`text` / `thinking` / `tool_use` / `tool_result`）で、
//! OpenAI 互換・Anthropic・Gemium はアダプタ側で相互変換する。Claude の tool_use / thinking を
//! 一級市民として持ち、最良モデルの機能を最小公倍数で削らない。`effort` も正規形に持ち、
//! 各アダプタが reasoning パラメータへ翻訳する（design §4.5）。

use serde::{Deserialize, Serialize};

/// LLM メッセージの役割。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 中立 content-block。プロバイダ非依存の会話素片。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// 本文テキスト。
    Text { text: String },
    /// 思考（extended thinking）。
    Thinking { text: String },
    /// モデルのツール呼び出し。
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// ツール実行結果（次ターンの入力として渡す）。
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

/// 1 メッセージ（role ＋ block 列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    /// 単一テキストメッセージのショートカット。
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Message {
            role,
            content: vec![Block::Text { text: text.into() }],
        }
    }
}

/// ツール定義（モデルに提示する）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema（input）。
    pub input_schema: serde_json::Value,
}

/// 思考強度の正規化（3 段階）。各アダプタが reasoning budget / thinking へ翻訳する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

/// 生成リクエスト（中立形）。プロバイダ差はアダプタが吸収する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateRequest {
    /// 論理モデル名（カタログ内・アダプタが実 ID へ写す）。空ならプロバイダ既定。
    #[serde(default)]
    pub model: Option<String>,
    /// トップレベル system プロンプト（Anthropic の top-level system 相当）。
    #[serde(default)]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDef>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 温度（未指定はプロバイダ既定）。
    #[serde(default)]
    pub temperature: Option<f32>,
}

impl GenerateRequest {
    /// 最小構成（messages のみ）のリクエストを作る。
    pub fn new(messages: Vec<Message>) -> Self {
        GenerateRequest {
            model: None,
            system: None,
            messages,
            tools: Vec::new(),
            effort: None,
            max_tokens: None,
            temperature: None,
        }
    }
}

/// トークン使用量（会計の素）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// 停止理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// 自然終了。
    EndTurn,
    /// ツール呼び出しで停止（ループ継続点）。
    ToolUse,
    /// max_tokens 到達。
    MaxTokens,
    /// その他/未知。
    Other,
}

/// ストリーミングの差分イベント（中立形）。アダプタが各プロバイダの SSE から写す。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamDelta {
    /// 本文テキストの差分。
    TextDelta { text: String },
    /// 思考テキストの差分。
    ThinkingDelta { text: String },
    /// ツール呼び出し開始（id/name 確定）。
    ToolUseStart { id: String, name: String },
    /// ツール入力 JSON の差分（部分 JSON 文字列）。
    ToolUseInputDelta { id: String, partial_json: String },
    /// ツール呼び出し完了（累積した入力 JSON）。
    ToolUseStop {
        id: String,
        input: serde_json::Value,
    },
    /// ストリーム完了（停止理由＋使用量）。
    Done {
        stop_reason: StopReason,
        usage: Usage,
    },
}
