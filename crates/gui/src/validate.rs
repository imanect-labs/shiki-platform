//! 検証エラー（skill・ミニアプリの本文の検証が返す・Task 6.3）。
//!
//! 違反は**拒否**（暗黙補正で危険物を通さない）し、全件を [`GuiValidationError`] で返す
//! （LLM の自己修正・エディタ表示用）。

use serde::Serialize;
use ts_rs::TS;
use utoipa::ToSchema;

/// 検証エラー（コード＋メッセージ＋位置）。workflow の `ValidationError` と同型の設計。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS, ToSchema)]
#[ts(export)]
pub struct GuiValidationError {
    /// エラーコード（例: `miniapp.workflow_not_bundled`）。
    pub code: String,
    /// 人向け（かつ LLM の自己修正向け）メッセージ。
    pub message: String,
    /// 紐付く位置（例: `workflows[0]`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl GuiValidationError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        GuiValidationError {
            code: code.into(),
            message: message.into(),
            path: None,
        }
    }

    #[must_use]
    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}
