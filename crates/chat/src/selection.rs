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
    /// スキルエディタで編集中のスキル全体（#919）。excerpt は SkillBody 相当の JSON
    /// （name・description・instructions ほか）。draft_name はスキル名。locator は無し。
    /// 部分選択ではなく全体を渡すため、抜粋の上限は [`SKILL_DRAFT_EXCERPT_MAX_CHARS`]。
    SkillDraft,
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

/// 編集中スキルの抜粋の上限（文字数）。指示文の上限（32KiB）＋設定が収まる大きさにする。
/// これを超える（長い few-shot・script を持つ）スキルは、途中で切らずに長い項目を丸ごと
/// 「省略」の印に置き換える（[`fit_skill_json`]）。切れた内容を AI が書き直すと後ろが消える。
pub const SKILL_DRAFT_EXCERPT_MAX_CHARS: usize = 48_000;

/// 省略した項目に入れる印。AI には「中身が見えていないので渡さない」ことを伝える
/// （save_skill は渡さなかった項目をエディタの今の値のまま残す）。
pub const SKILL_FIELD_OMITTED: &str =
    "（長いため省略。中身が見えていないので、この項目は save_skill に渡さないこと）";

/// スキルの JSON（オブジェクト）を `max` 文字以内に収める。収まらなければ、大きい項目から
/// 順に [`SKILL_FIELD_OMITTED`] へ置き換える（項目の途中では切らない）。
/// オブジェクトでなければ None（呼び出し側が通常の切り詰めにする）。
#[must_use]
pub fn fit_skill_json(value: &serde_json::Value, max: usize) -> Option<String> {
    let serde_json::Value::Object(map) = value else {
        return None;
    };
    let mut map = map.clone();
    let len = |m: &serde_json::Map<String, serde_json::Value>| {
        serde_json::to_string(m).map_or(usize::MAX, |s| s.chars().count())
    };
    while len(&map) > max {
        // 名前（識別キー）は残す。まだ省略していない中で最も大きい項目を印に置き換える。
        let largest = map
            .iter()
            .filter(|(k, v)| k.as_str() != "name" && v.as_str() != Some(SKILL_FIELD_OMITTED))
            .max_by_key(|(_, v)| serde_json::to_string(v).map_or(0, |s| s.chars().count()))
            .map(|(k, _)| k.clone());
        let Some(key) = largest else { break };
        map.insert(key, serde_json::Value::String(SKILL_FIELD_OMITTED.into()));
    }
    serde_json::to_string(&map).ok()
}

impl SelectionKind {
    /// 抜粋の上限（文字数）。
    #[must_use]
    pub const fn excerpt_max_chars(self) -> usize {
        match self {
            Self::SkillDraft => SKILL_DRAFT_EXCERPT_MAX_CHARS,
            Self::NoteSelection | Self::CsvRange | Self::SlideSelection | Self::OfficeSelection => {
                SELECTION_EXCERPT_MAX_CHARS
            }
        }
    }
}

impl SelectionContext {
    /// 抜粋・下書き名・locator をサーバ側の上限へ切り詰める（受理時に必ず通す）。
    #[must_use]
    pub fn clamped(mut self) -> Self {
        let max = self.kind.excerpt_max_chars();
        if self.excerpt.chars().count() > max {
            // 編集中のスキルは JSON なので、項目の途中で切らずに長い項目を省略の印にする。
            let fitted = (self.kind == SelectionKind::SkillDraft)
                .then(|| serde_json::from_str::<serde_json::Value>(&self.excerpt).ok())
                .flatten()
                .and_then(|v| fit_skill_json(&v, max))
                .filter(|s| s.chars().count() <= max);
            self.excerpt = fitted.unwrap_or_else(|| self.excerpt.chars().take(max).collect());
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
