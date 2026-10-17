//! Word 操作の語彙（LLM が書く形・#725）。
//!
//! 単一定義: ツールの description（`describe.rs`）・検証・正規化はこの型を正とする。
//! 対象は office.read が返すブロック ID（`b12-3fa9`）か範囲（`b12-3fa9:b15-77c1`）。
//! 本文は HTML（見出し・段落・箇条書き・表・強調・色・画像）で渡す。**未指定の項目は変えない**。

use serde::Deserialize;

pub use crate::engine::calc::ops::Patch;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)] // 1 回の呼び出しで逆シリアル化して捨てる短命の値。
pub enum DocOp {
    /// HTML を差し込む。位置は after / before（ブロック ID）か at（"start" / "end"）のどれか 1 つ。
    Insert {
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        before: Option<String>,
        #[serde(default)]
        at: Option<Edge>,
        html: String,
    },
    /// ブロック（または範囲）を HTML で置き換える。
    Replace { target: String, html: String },
    /// ブロック（または範囲）を消す。
    Delete { target: String },
    /// 文字列を置き換える（書式は保つ）。within で範囲を絞れる。
    ReplaceText {
        find: String,
        replace: String,
        #[serde(default)]
        within: Option<String>,
        #[serde(default)]
        all: bool,
        #[serde(default)]
        match_case: bool,
        #[serde(default)]
        whole_word: bool,
    },
    /// 見つけた文字列に文字の書式を付ける。
    FormatText {
        find: String,
        #[serde(default)]
        within: Option<String>,
        #[serde(default)]
        all: bool,
        #[serde(default)]
        match_case: bool,
        font: TextFont,
    },
    /// 段落の書式（スタイル・配置・字下げ・間隔・行間・改ページ・文字の書式）。
    FormatParagraph {
        target: String,
        #[serde(flatten)]
        para: ParaSpec,
        /// 表の中の段落にも当てる（既定は本文の段落だけ）。
        #[serde(default)]
        include_tables: bool,
    },
    /// スタイルを変える（無ければ作る）。見出しの色・本文の書体など文書全体の見た目を揃えるときに使う。
    SetStyle {
        name: String,
        #[serde(default)]
        family: StyleFamily,
        #[serde(default)]
        based_on: Option<String>,
        #[serde(flatten)]
        para: ParaSpec,
    },
    /// ページ設定。
    PageSetup {
        #[serde(default)]
        paper: Option<crate::engine::calc::ops::Paper>,
        #[serde(default)]
        orientation: Option<crate::engine::calc::ops::Orientation>,
        #[serde(default)]
        margins_mm: Option<MarginsMm>,
    },
    /// ヘッダー（`null` で消す）。
    Header {
        #[serde(deserialize_with = "nullable")]
        content: Option<HeaderFooter>,
    },
    /// フッター（`null` で消す）。
    Footer {
        #[serde(deserialize_with = "nullable")]
        content: Option<HeaderFooter>,
    },
    /// 目次を入れる（見出しから作る）。位置は before / after / at。既定は文書の先頭。
    InsertToc {
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        before: Option<String>,
        #[serde(default)]
        at: Option<Edge>,
        #[serde(default = "toc_title")]
        title: String,
        #[serde(default = "toc_levels")]
        levels: u8,
    },
    /// 目次などの索引を最新にする（見出しを変えた後に）。
    UpdateToc {},
    /// そのブロックの前で改ページする。
    PageBreak { before: String },
    /// 変更履歴の記録を切り替える。
    TrackChanges { enabled: bool },
}

fn toc_title() -> String {
    "目次".into()
}

fn toc_levels() -> u8 {
    3
}

/// 値か null（キーは必須）。
fn nullable<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

impl DocOp {
    pub fn name(&self) -> &'static str {
        match self {
            DocOp::Insert { .. } => "insert",
            DocOp::Replace { .. } => "replace",
            DocOp::Delete { .. } => "delete",
            DocOp::ReplaceText { .. } => "replace_text",
            DocOp::FormatText { .. } => "format_text",
            DocOp::FormatParagraph { .. } => "format_paragraph",
            DocOp::SetStyle { .. } => "set_style",
            DocOp::PageSetup { .. } => "page_setup",
            DocOp::Header { .. } => "header",
            DocOp::Footer { .. } => "footer",
            DocOp::InsertToc { .. } => "insert_toc",
            DocOp::UpdateToc {} => "update_toc",
            DocOp::PageBreak { .. } => "page_break",
            DocOp::TrackChanges { .. } => "track_changes",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Start,
    End,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyleFamily {
    #[default]
    Paragraph,
    Character,
}

/// 文字の書式（指定した項目だけを変える）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextFont {
    #[serde(default)]
    pub name: Option<String>,
    /// pt。
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub underline: Option<bool>,
    #[serde(default)]
    pub strike: Option<bool>,
    #[serde(default)]
    pub color: Option<String>,
    /// 蛍光ペン（文字の背景色）。
    #[serde(default)]
    pub highlight: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

/// 段落の書式（format_paragraph と set_style が共有）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ParaSpec {
    /// 段落スタイル（format_paragraph のみ）。
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub align: Option<Align>,
    #[serde(default)]
    pub indent_left_mm: Option<f64>,
    /// 1 行目の字下げ（負の値でぶら下げ）。
    #[serde(default)]
    pub first_line_indent_mm: Option<f64>,
    #[serde(default)]
    pub space_before_pt: Option<f64>,
    #[serde(default)]
    pub space_after_pt: Option<f64>,
    /// 行間（1.0 = 1 行・1.5 = 1.5 行）。
    #[serde(default)]
    pub line_spacing: Option<f64>,
    /// 行間を固定値（pt）にする。
    #[serde(default)]
    pub line_height_pt: Option<f64>,
    #[serde(default)]
    pub keep_with_next: Option<bool>,
    #[serde(default)]
    pub page_break_before: Option<bool>,
    #[serde(default)]
    pub font: Option<TextFont>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarginsMm {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// ヘッダー・フッター（`&[Page]` `&[Pages]` `&[Date]` `&[Title]` `&[File]` を差し込める）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderFooter {
    pub text: String,
    #[serde(default)]
    pub align: Option<Align>,
    /// pt。
    #[serde(default)]
    pub size: Option<f64>,
}
