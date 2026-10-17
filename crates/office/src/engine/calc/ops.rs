//! Excel 操作の語彙（LLM が書く形・#725）。
//!
//! 単一定義: ツールの description（`describe.rs`）・検証・正規化はすべてこの型を正とする。
//! 範囲は Excel の A1 表記（`B2:D10`・`'集計 2'!A1`）、色は `#RRGGBB`、列幅は Excel と同じ
//! 「文字数」、行高は pt、紙の余白は cm。**未指定のフィールドには触れない**（パッチの意味論）。

use serde::Deserialize;

/// 1 つの操作。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)] // 1 回の呼び出しで逆シリアル化して捨てる短命の値（箱に入れる利得が無い）。
pub enum SheetOp {
    /// 値・数式を書く。`range` は左上セルだけでもよい（大きさは values から決まる）。
    SetValues {
        range: String,
        values: Vec<Vec<CellInput>>,
    },
    /// 左上セル用に書いた数式を範囲全体へ相対参照で展開する（Excel のフィル）。
    FillFormula {
        range: String,
        formula: String,
    },
    /// 書式（指定した項目だけを変える）。
    Format {
        range: RangeList,
        #[serde(default)]
        number_format: Option<String>,
        #[serde(default)]
        font: Option<FontSpec>,
        /// 塗りつぶし。`null` を明示すると塗りなしにする。
        #[serde(default, deserialize_with = "explicit_null")]
        fill: Option<Patch<String>>,
        #[serde(default)]
        align: Option<AlignSpec>,
        #[serde(default)]
        border: Option<BorderSpec>,
    },
    /// 表をまとめて整える（見出し・縞・合計行・罫線を配色の名前ひとつで）。
    StyleTable {
        range: String,
        #[serde(default)]
        theme: TableTheme,
        /// 見出しの行数（先頭から）。
        #[serde(default = "one")]
        header_rows: u32,
        /// 最終行を合計行として強調する。
        #[serde(default)]
        total_row: bool,
        /// 末尾の何行を集計行（合計・平均など）として強調するか（total_row より優先）。
        #[serde(default)]
        total_rows: Option<u32>,
        /// 本体の行を 1 行おきに薄く塗る。
        #[serde(default = "yes")]
        banded: bool,
    },
    /// セル結合（`center` で中央寄せも行う）。
    Merge {
        range: String,
        #[serde(default)]
        center: bool,
    },
    Unmerge {
        range: String,
    },
    /// 列幅（文字数・Excel と同じ単位）。`"auto"` で内容に合わせる。
    ColumnWidth {
        columns: String,
        width: Size,
    },
    /// 行の高さ（pt）。`"auto"` で内容に合わせる。
    RowHeight {
        rows: String,
        height: Size,
    },
    /// `before` の行（1 始まり）の上に `count` 行入れる。
    InsertRows {
        #[serde(default)]
        sheet: Option<String>,
        before: u32,
        #[serde(default = "one")]
        count: u32,
    },
    DeleteRows {
        rows: String,
    },
    /// `before` の列（例 "C"）の左に `count` 列入れる。
    InsertColumns {
        #[serde(default)]
        sheet: Option<String>,
        before: String,
        #[serde(default = "one")]
        count: u32,
    },
    DeleteColumns {
        columns: String,
    },
    HideRows {
        rows: String,
    },
    ShowRows {
        rows: String,
    },
    HideColumns {
        columns: String,
    },
    ShowColumns {
        columns: String,
    },
    /// 消す（`contents`=値と数式・`formats`=書式・`all`=両方）。
    Clear {
        range: String,
        #[serde(default)]
        what: ClearWhat,
    },
    /// 並べ替え（`by` の列は範囲内の列名）。
    Sort {
        range: String,
        by: Vec<SortKey>,
        #[serde(default = "yes")]
        has_header: bool,
    },
    /// オートフィルタ。`enabled: false` で外す（そのときは `sheet` だけでよい）。
    Autofilter {
        #[serde(default)]
        range: Option<String>,
        #[serde(default)]
        sheet: Option<String>,
        #[serde(default = "yes")]
        enabled: bool,
    },
    /// ウィンドウ枠の固定（`cell` の左と上を固定する。`null` で解除）。
    Freeze {
        #[serde(default)]
        sheet: Option<String>,
        cell: Option<String>,
    },
    AddSheet {
        name: String,
        /// 1 始まりの位置（省略で末尾）。
        #[serde(default)]
        position: Option<u32>,
    },
    RenameSheet {
        sheet: String,
        name: String,
    },
    DeleteSheet {
        sheet: String,
    },
    MoveSheet {
        sheet: String,
        position: u32,
    },
    /// シートの見た目（見出しの色・表示/非表示）。
    SheetProps {
        sheet: String,
        #[serde(default, deserialize_with = "explicit_null")]
        tab_color: Option<Patch<String>>,
        #[serde(default)]
        visible: Option<bool>,
    },
    /// 条件付き書式（値の比較 or 数式）。
    ConditionalFormat {
        range: RangeList,
        when: Condition,
        style: CondStyle,
    },
    ClearConditionalFormats {
        range: String,
    },
    /// 入力規則。
    DataValidation {
        range: RangeList,
        rule: ValidationRule,
        #[serde(default)]
        input_message: Option<String>,
        #[serde(default)]
        error_message: Option<String>,
        #[serde(default = "yes")]
        allow_blank: bool,
    },
    /// 名前の定義（`range: null` で削除）。
    DefineName {
        name: String,
        range: Option<String>,
    },
    /// グラフ。`data` の先頭行・先頭列を見出しとして扱う。
    AddChart {
        #[serde(default)]
        sheet: Option<String>,
        name: String,
        #[serde(rename = "type")]
        kind: ChartKind,
        data: RangeList,
        /// 左上を置くセル。
        anchor: String,
        #[serde(default)]
        width_cm: Option<f64>,
        #[serde(default)]
        height_cm: Option<f64>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default = "yes")]
        legend: bool,
        /// 凡例の位置（既定は下）。
        #[serde(default)]
        legend_position: Option<LegendPosition>,
        /// 系列（円は項目）の色 `#RRGGBB` の並び（省略で既定の配色）。
        #[serde(default)]
        colors: Option<Vec<String>>,
        /// 値をラベルとして表示する（円は割合）。
        #[serde(default)]
        data_labels: bool,
        /// 縦軸の表示形式（例 "#,##0"）。
        #[serde(default)]
        y_number_format: Option<String>,
        /// 系列の向き（`columns`=列ごと・`rows`=行ごと）。
        #[serde(default)]
        series: SeriesIn,
        #[serde(default)]
        stacked: bool,
        #[serde(default)]
        percent: bool,
        #[serde(default)]
        x_title: Option<String>,
        #[serde(default)]
        y_title: Option<String>,
        #[serde(default = "yes")]
        first_row_is_header: bool,
        #[serde(default = "yes")]
        first_column_is_header: bool,
    },
    DeleteChart {
        #[serde(default)]
        sheet: Option<String>,
        name: String,
    },
    /// 印刷設定（指定した項目だけを変える）。
    PageSetup {
        #[serde(default)]
        sheet: Option<String>,
        #[serde(default)]
        orientation: Option<Orientation>,
        #[serde(default)]
        paper: Option<Paper>,
        /// 横 × 縦のページ数に収める（0 は「その方向は制限しない」）。
        #[serde(default)]
        fit_to: Option<FitTo>,
        /// 拡大縮小（%）。
        #[serde(default)]
        scale: Option<u16>,
        #[serde(default)]
        margins_cm: Option<Margins>,
        #[serde(default)]
        center_horizontally: Option<bool>,
        #[serde(default, deserialize_with = "explicit_null")]
        print_area: Option<Patch<String>>,
        /// 各ページに繰り返す行（例 "1:2"）。
        #[serde(default, deserialize_with = "explicit_null")]
        repeat_rows: Option<Patch<String>>,
        #[serde(default)]
        gridlines: Option<bool>,
        /// 各ページの上（ヘッダー）。`null` で消す。
        #[serde(default, deserialize_with = "explicit_null")]
        header: Option<Patch<HeaderFooter>>,
        /// 各ページの下（フッター）。`null` で消す。
        #[serde(default, deserialize_with = "explicit_null")]
        footer: Option<Patch<HeaderFooter>>,
    },
}

/// ヘッダー・フッターの左・中央・右の文字列（`&[Page]` `&[Pages]` `&[Date]` `&[Sheet]` `&[File]` を差し込める）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderFooter {
    #[serde(default)]
    pub left: Option<String>,
    #[serde(default)]
    pub center: Option<String>,
    #[serde(default)]
    pub right: Option<String>,
}

fn one() -> u32 {
    1
}

fn yes() -> bool {
    true
}

/// パッチの値: `null` の明示（既定へ戻す）か、新しい値か。キーが無いことは外側の `None` で表す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Patch<T> {
    /// `null` が明示された（塗りなし・自動の色・印刷範囲の解除など）。
    Clear,
    Set(T),
}

/// 「キーが無い」（`None`）と「`null` が明示された」（`Some(Patch::Clear)`）を区別する。
pub(crate) fn explicit_null<'de, D, T>(d: D) -> Result<Option<Patch<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(match Option::<T>::deserialize(d)? {
        Some(v) => Patch::Set(v),
        None => Patch::Clear,
    }))
}

impl SheetOp {
    pub fn name(&self) -> &'static str {
        match self {
            SheetOp::SetValues { .. } => "set_values",
            SheetOp::FillFormula { .. } => "fill_formula",
            SheetOp::Format { .. } => "format",
            SheetOp::StyleTable { .. } => "style_table",
            SheetOp::Merge { .. } => "merge",
            SheetOp::Unmerge { .. } => "unmerge",
            SheetOp::ColumnWidth { .. } => "column_width",
            SheetOp::RowHeight { .. } => "row_height",
            SheetOp::InsertRows { .. } => "insert_rows",
            SheetOp::DeleteRows { .. } => "delete_rows",
            SheetOp::InsertColumns { .. } => "insert_columns",
            SheetOp::DeleteColumns { .. } => "delete_columns",
            SheetOp::HideRows { .. } => "hide_rows",
            SheetOp::ShowRows { .. } => "show_rows",
            SheetOp::HideColumns { .. } => "hide_columns",
            SheetOp::ShowColumns { .. } => "show_columns",
            SheetOp::Clear { .. } => "clear",
            SheetOp::Sort { .. } => "sort",
            SheetOp::Autofilter { .. } => "autofilter",
            SheetOp::Freeze { .. } => "freeze",
            SheetOp::AddSheet { .. } => "add_sheet",
            SheetOp::RenameSheet { .. } => "rename_sheet",
            SheetOp::DeleteSheet { .. } => "delete_sheet",
            SheetOp::MoveSheet { .. } => "move_sheet",
            SheetOp::SheetProps { .. } => "sheet_props",
            SheetOp::ConditionalFormat { .. } => "conditional_format",
            SheetOp::ClearConditionalFormats { .. } => "clear_conditional_formats",
            SheetOp::DataValidation { .. } => "data_validation",
            SheetOp::DefineName { .. } => "define_name",
            SheetOp::AddChart { .. } => "add_chart",
            SheetOp::DeleteChart { .. } => "delete_chart",
            SheetOp::PageSetup { .. } => "page_setup",
        }
    }
}

/// セルに書く値。文字列の先頭が `=` なら数式（Excel の書き方）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CellInput {
    Null(()),
    Bool(bool),
    Number(f64),
    Text(String),
    /// 日付（`{"date": "2026-04-01"}`）。表示形式が標準なら yyyy/mm/dd を付ける。
    Date {
        date: String,
    },
    /// 先頭が `=` でも数式にしない文字列（`{"text": "=はじめ"}`）。
    Literal {
        text: String,
    },
}

/// 1 つの範囲か、範囲の並び（`"A1:C1"` または `["A1:C1", "A5:C5"]`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RangeList {
    One(String),
    Many(Vec<String>),
}

impl RangeList {
    pub fn items(&self) -> Vec<&str> {
        match self {
            RangeList::One(s) => vec![s.as_str()],
            RangeList::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontSpec {
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
    /// 文字色（`#RRGGBB`・`null` で自動）。
    #[serde(default, deserialize_with = "explicit_null")]
    pub color: Option<Patch<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HAlign {
    General,
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlignSpec {
    #[serde(default)]
    pub horizontal: Option<HAlign>,
    #[serde(default)]
    pub vertical: Option<VAlign>,
    /// 折り返して全体を表示する。
    #[serde(default)]
    pub wrap: Option<bool>,
    /// 縮小して全体を表示する。
    #[serde(default)]
    pub shrink: Option<bool>,
    /// インデント（字下げの段数）。
    #[serde(default)]
    pub indent: Option<u8>,
    /// 文字の回転（度）。
    #[serde(default)]
    pub rotate: Option<i32>,
}

/// 罫線（辺ごと）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BorderSpec {
    /// 外枠と内側の両方。
    #[serde(default)]
    pub all: Option<Line>,
    #[serde(default)]
    pub outline: Option<Line>,
    #[serde(default)]
    pub inner: Option<Line>,
    #[serde(default)]
    pub top: Option<Line>,
    #[serde(default)]
    pub bottom: Option<Line>,
    #[serde(default)]
    pub left: Option<Line>,
    #[serde(default)]
    pub right: Option<Line>,
    /// 内側の横線。
    #[serde(default)]
    pub inside_horizontal: Option<Line>,
    /// 内側の縦線。
    #[serde(default)]
    pub inside_vertical: Option<Line>,
}

/// 線。`"thin"` のような省略形か、`{"style": "medium", "color": "#1F3864"}`。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Line {
    Style(LineStyle),
    Full {
        style: LineStyle,
        #[serde(default)]
        color: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineStyle {
    None,
    Hair,
    Thin,
    Medium,
    Thick,
    Double,
    Dashed,
    Dotted,
}

/// 数値か `"auto"`。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Size {
    Value(f64),
    Keyword(AutoKeyword),
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoKeyword {
    Auto,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearWhat {
    #[default]
    Contents,
    Formats,
    All,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortKey {
    /// 列名（例 "E"）。
    pub column: String,
    #[serde(default)]
    pub order: SortOrder,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

/// 条件。`{"operator": "<", "value": 600}` / `{"operator": "between", "value": 1, "value2": 5}`
/// / `{"formula": "=$E2>1000"}`（左上セル基準の相対参照）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Condition {
    Formula {
        formula: String,
    },
    Compare {
        operator: CompareOp,
        value: serde_json::Value,
        #[serde(default)]
        value2: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum CompareOp {
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "<>")]
    Ne,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "between")]
    Between,
    #[serde(rename = "not_between")]
    NotBetween,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CondStyle {
    #[serde(default)]
    pub font_color: Option<String>,
    #[serde(default)]
    pub fill: Option<String>,
    #[serde(default)]
    pub bold: bool,
}

/// 入力規則。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValidationRule {
    /// 一覧から選ぶ（`items` を直接書くか、`source` の範囲を参照する）。
    List {
        #[serde(default)]
        items: Option<Vec<String>>,
        #[serde(default)]
        source: Option<String>,
    },
    WholeNumber {
        operator: CompareOp,
        value: f64,
        #[serde(default)]
        value2: Option<f64>,
    },
    Decimal {
        operator: CompareOp,
        value: f64,
        #[serde(default)]
        value2: Option<f64>,
    },
    Date {
        operator: CompareOp,
        value: String,
        #[serde(default)]
        value2: Option<String>,
    },
    TextLength {
        operator: CompareOp,
        value: u32,
        #[serde(default)]
        value2: Option<u32>,
    },
    Custom {
        formula: String,
    },
}

/// 表の配色（見出しの濃い色・縞の薄い色・合計行の色・罫線の色の組）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableTheme {
    #[default]
    Blue,
    Navy,
    Green,
    Gray,
    Orange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChartKind {
    Column,
    Bar,
    Line,
    Area,
    Pie,
    Doughnut,
    Scatter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegendPosition {
    Bottom,
    Right,
    Top,
    Left,
    None,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesIn {
    #[default]
    Columns,
    Rows,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum Paper {
    A3,
    A4,
    A5,
    B4,
    B5,
    #[serde(rename = "letter")]
    Letter,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FitTo {
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Margins {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}
