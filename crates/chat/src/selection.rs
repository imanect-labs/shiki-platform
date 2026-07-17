//! エディタの選択コンテキスト（選択→AI 指示・Task 11.10・design §4.8.3）。
//!
//! クライアント由来の値は信用しない: kind は閉集合・node_id は api 層が実行主体の
//! viewer 権限で再解決できた場合のみ受理（fail-closed）・excerpt/locator/draft_name は
//! 受理時に必ず [`SelectionContext::clamped`] で上限へ切り詰める。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// エディタ選択の種別（閉集合・Task 11.10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelectionKind {
    /// ノート（TipTap）のテキスト選択。locator = `{ heading_path: string[] }`。
    NoteSelection,
    /// CSV グリッドのセル範囲選択。locator = `{ rows: [start, end], cols: [start, end] }`。
    CsvRange,
    /// スライドの要素選択。locator = `{ slide_id: string, element_index?: number }`。
    SlideSelection,
    /// Office 文書（Collabora・docx/xlsx/pptx）の選択。excerpt は Action_Copy で取得した
    /// 選択テキスト。locator は無し（Collabora 側の位置は WOPI ホストからは参照しない）。
    OfficeSelection,
}

/// エディタの選択コンテキスト（クライアント由来＝信用しない・api 層で検証してから受理）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SelectionContext {
    pub kind: SelectionKind,
    /// 選択元のノード（実体ドキュメント）。**api 層が実行主体の viewer 権限で再解決
    /// できた場合のみ受理**（読めない/存在しない対象は fail-closed で拒否・存在秘匿）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<uuid::Uuid>,
    /// 下書き由来の選択（このスレッドの下書き name。実体ノード無し）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_name: Option<String>,
    /// 選択内容の抜粋（表示・誘導用データ。サーバ側で上限に切り詰める）。
    pub excerpt: String,
    /// 位置ヒント（kind 別の JSON・権限の根拠にはしない）。
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub locator: serde_json::Value,
}

/// 選択抜粋の上限（文字数・プロンプト肥大とメモリ圧迫の遮断）。
pub const SELECTION_EXCERPT_MAX_CHARS: usize = 8_000;

impl SelectionContext {
    /// 抜粋・下書き名・locator をサーバ側の上限へ切り詰める（受理時に必ず通す）。
    #[must_use]
    pub fn clamped(mut self) -> Self {
        if self.excerpt.chars().count() > SELECTION_EXCERPT_MAX_CHARS {
            self.excerpt = self
                .excerpt
                .chars()
                .take(SELECTION_EXCERPT_MAX_CHARS)
                .collect();
        }
        if let Some(name) = &self.draft_name {
            if name.chars().count() > 200 {
                self.draft_name = Some(name.chars().take(200).collect());
            }
        }
        // locator は素通しせずサイズだけ制限する（4KB・構造はクライアントヒント）。
        if serde_json::to_string(&self.locator).map_or(0, |s| s.len()) > 4_096 {
            self.locator = serde_json::Value::Null;
        }
        self
    }
}
